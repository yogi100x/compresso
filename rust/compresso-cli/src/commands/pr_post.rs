//! Posts a confirmed decisions draft with the GitHub CLI (ADR-0031).
//!
//! This is the second place in `compresso-cli` that may start a child
//! process (the first is the MCP health probe, ADR-0028). It runs exactly
//! `gh pr comment <number> --body-file -` with the draft on standard input:
//! no shell, no argument taken from the ledger, and a bounded wait. It is
//! reached only after `compresso decisions --pr N --post --yes`.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::decisions::PostError;

/// How long `gh` may take to deliver one comment.
pub(super) const POST_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Runs `gh pr comment` for `pull_request` with `body` on standard input.
pub(super) fn post_comment(
    gh: &Path,
    pull_request: u64,
    body: &str,
    timeout: Duration,
) -> Result<(), PostError> {
    let mut child = Command::new(gh)
        .args(["pr", "comment"])
        .arg(pull_request.to_string())
        .args(["--body-file", "-"])
        .stdin(Stdio::piped())
        // `gh` prints the comment URL and its own diagnostics; neither is
        // relayed, so nothing it prints can reach Compresso's output.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PostError::ExecutionUnavailable)?;

    // The draft is written from its own thread: a `gh` that does not read
    // its input must not be able to block the wait below.
    let writer = child.stdin.take().map(|mut stdin| {
        let body = body.to_owned();
        thread::spawn(move || {
            let _ = stdin.write_all(body.as_bytes());
        })
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) | Err(_) => break None,
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(PostError::TimedOut);
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    if status.success() {
        Ok(())
    } else {
        Err(PostError::Failed)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    use super::*;

    /// Writes an executable stand-in for `gh` and returns its path.
    ///
    /// The file that is executed is never opened for writing by this test
    /// process. Other tests in the binary spawn processes concurrently; a
    /// child forked while this thread held a write descriptor would keep it
    /// until its own exec, and executing the file then fails on Linux with
    /// `ETXTBSY` (seen once on CI as `ExecutionUnavailable`). So the script
    /// is written under another name and `cp` (its own process, whose
    /// descriptors no fork of this process inherits) creates the stand-in.
    fn stand_in(directory: &Path, script: &str) -> PathBuf {
        let draft = directory.join("gh.draft");
        let path = directory.join("gh");
        fs::write(&draft, format!("#!/bin/sh\n{script}\n")).expect("stand-in should be written");
        let copied = Command::new("cp")
            .arg(&draft)
            .arg(&path)
            .status()
            .expect("cp should run");
        assert!(copied.success(), "stand-in should be copied");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("stand-in should be executable");
        path
    }

    #[test]
    fn test_post_runs_gh_with_fixed_arguments_and_the_draft_on_stdin() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let record = directory.path().join("record");
        let gh = stand_in(
            directory.path(),
            &format!(
                "printf '%s\\n' \"$*\" > '{0}.args'\ncat > '{0}.stdin'",
                record.display()
            ),
        );
        let body = "## Decisions\n\n- `safe` mode; $(touch injected) `backticks` & \"quotes\"\n";

        let result = post_comment(&gh, 42, body, POST_TIMEOUT);

        assert_eq!(result, Ok(()));
        assert_eq!(
            fs::read_to_string(record.with_extension("args"))
                .expect("arguments should be recorded"),
            "pr comment 42 --body-file -\n"
        );
        assert_eq!(
            fs::read_to_string(record.with_extension("stdin")).expect("stdin should be recorded"),
            body
        );
        assert!(
            !directory.path().join("injected").exists(),
            "the draft must never reach a shell"
        );
    }

    #[test]
    fn test_post_reports_a_refusal_from_gh_as_failed() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let gh = stand_in(directory.path(), "cat > /dev/null\nexit 1");

        assert_eq!(
            post_comment(&gh, 1, "body", POST_TIMEOUT),
            Err(PostError::Failed)
        );
    }

    #[test]
    fn test_post_stops_a_gh_that_never_finishes() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let gh = stand_in(directory.path(), "sleep 30");
        let started = Instant::now();

        let result = post_comment(&gh, 1, "body", Duration::from_millis(300));

        assert_eq!(result, Err(PostError::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "returned after {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn test_post_reports_a_gh_that_cannot_be_started() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let missing = directory.path().join("gh");

        assert_eq!(
            post_comment(&missing, 1, "body", POST_TIMEOUT),
            Err(PostError::ExecutionUnavailable)
        );
    }
}
