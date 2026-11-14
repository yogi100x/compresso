//! A closed or broken stdio pipe must never panic a Compresso process.
//!
//! The `compresso` CLI behaves like a standard Unix filter: when the reader
//! of its standard output goes away it stops quietly, with no message and a
//! success exit. The long-running MCP server instead reports the typed
//! `output_closed` error, because a server that lost its client is a failure.
//!
//! These tests launch the real binary so the failure crosses a real OS pipe:
//! `EPIPE` on Unix and `ERROR_BROKEN_PIPE` / `ERROR_NO_DATA` on Windows.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use compresso_core::{ClosedPipeGuard, StreamError, suppress_console_window};
use tempfile::{TempDir, tempdir};

const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds a background launch of the CLI against an isolated home.
fn compresso(home: &TempDir) -> Command {
    let root = home.path();
    let mut command = Command::new(env!("CARGO_BIN_EXE_compresso"));
    command
        .env("HOME", root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("APPDATA", root.join("appdata"))
        .env("LOCALAPPDATA", root.join("localappdata"))
        .env_remove("COMPRESSO_LAUNCHER_PATH")
        .current_dir(root);
    let applied = suppress_console_window(|flags| {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = flags;
    });
    assert_eq!(applied, cfg!(windows));
    command
}

fn wait_for_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + EXIT_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("child did not exit after its output reader closed");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_stderr(child: &mut Child) -> String {
    let mut text = String::new();
    std::io::Read::read_to_string(
        child.stderr.as_mut().expect("stderr should be piped"),
        &mut text,
    )
    .expect("stderr should be readable UTF-8");
    text
}

fn assert_reported_closed_output(status: ExitStatus, stderr: &str) {
    assert_eq!(
        status.code(),
        Some(1),
        "a closed reader must be an ordinary failure exit, not a signal or panic: {stderr}"
    );
    assert!(
        stderr.contains("error[output_closed]"),
        "missing typed report: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "process panicked: {stderr}");
}

/// Enough output to overflow any pipe buffer, so a write must observe the
/// closed reader instead of completing into kernel memory.
fn large_input() -> Vec<u8> {
    b"a line of ordinary text that passes through unchanged\n".repeat(40_000)
}

/// Feeds `input` to the child from another thread so a full pipe never
/// deadlocks the test, tolerating a child that stops reading.
fn feed(child: &mut Child, input: Vec<u8>) -> thread::JoinHandle<()> {
    let mut stdin = child.stdin.take().expect("stdin should be piped");
    thread::spawn(move || {
        let _ = stdin.write_all(&input);
    })
}

#[test]
fn test_cli_exits_silently_when_stdout_reader_closed_early() {
    let home = tempdir().expect("temporary home");
    let mut child = compresso(&home)
        .args(["compress", "--mode", "passthrough"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compresso should launch");

    // Close the reader first; the command cannot write until stdin ends.
    drop(child.stdout.take());
    let feeder = feed(&mut child, large_input());

    let status = wait_for_exit(&mut child);
    let stderr = read_stderr(&mut child);
    feeder.join().expect("input feeder should finish");
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr, "", "a closed reader must not be reported");
}

#[cfg(unix)]
#[test]
fn test_cli_piped_into_head_exits_zero_without_stderr_output() {
    let home = tempdir().expect("temporary home");
    let input = home.path().join("input.txt");
    std::fs::write(&input, large_input()).expect("input file should be written");
    let mut shell = Command::new("/bin/sh");
    shell
        .arg("-c")
        .arg(
            "{ \"$COMPRESSO_BIN\" compress --mode passthrough --file \"$INPUT\"; \
             echo \"status=$?\" >&2; } | head -c1 >/dev/null",
        )
        .env("COMPRESSO_BIN", env!("CARGO_BIN_EXE_compresso"))
        .env("INPUT", &input)
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .current_dir(home.path());

    let output = shell.output().expect("shell pipeline should run");

    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "status=0\n",
        "`compresso ... | head -c1` must be silent and exit 0"
    );
}

#[test]
fn test_cli_reports_real_error_even_when_stdout_reader_is_closed() {
    let home = tempdir().expect("temporary home");
    let mut child = compresso(&home)
        .args(["compress", "--file", "does-not-exist.txt"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compresso should launch");
    drop(child.stdout.take());

    let status = wait_for_exit(&mut child);
    let stderr = read_stderr(&mut child);

    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("error[invalid_arguments]"),
        "an error raised before any output must still be reported: {stderr}"
    );
}

#[test]
fn test_cli_failure_with_closed_stderr_exits_without_panicking() {
    let home = tempdir().expect("temporary home");
    let mut child = compresso(&home)
        .args(["compress", "--file", "does-not-exist.txt"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compresso should launch");
    drop(child.stderr.take());

    // A panic would exit 101 and a signal would report no code at all.
    assert_eq!(wait_for_exit(&mut child).code(), Some(1));
}

#[test]
fn test_cli_usage_error_with_closed_stderr_exits_without_panicking() {
    let home = tempdir().expect("temporary home");
    let mut child = compresso(&home)
        .args(["compress", "--mode", "no-such-mode"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compresso should launch");
    drop(child.stderr.take());

    assert_eq!(wait_for_exit(&mut child).code(), Some(2));
}

#[test]
fn test_cli_exits_zero_when_both_output_streams_are_closed() {
    let home = tempdir().expect("temporary home");
    let mut child = compresso(&home)
        .args(["compress", "--mode", "passthrough"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compresso should launch");
    drop(child.stdout.take());
    drop(child.stderr.take());
    let feeder = feed(&mut child, large_input());

    let status = wait_for_exit(&mut child);
    feeder.join().expect("input feeder should finish");
    assert_eq!(status.code(), Some(0));
}

#[test]
fn test_mcp_server_exits_with_typed_error_when_stdout_reader_closed_early() {
    let home = tempdir().expect("temporary home");
    let mut child = compresso(&home)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compresso MCP server should launch");

    drop(child.stdout.take());
    // Standard input stays open: only the closed output may end the server.
    let mut input = child.stdin.take().expect("stdin should be piped");
    input
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"closed-pipe-test\",\"version\":\"1.0.0\"}}}\n",
        )
        .expect("initialize request should be written");
    input.flush().expect("initialize request should be flushed");

    let status = wait_for_exit(&mut child);
    let stderr = read_stderr(&mut child);
    drop(input);
    assert_reported_closed_output(status, &stderr);
}

#[test]
fn test_writing_to_child_that_exited_early_is_typed_closed_error() {
    let home = tempdir().expect("temporary home");
    let mut child = compresso(&home)
        .arg("--version")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("compresso should launch");
    let input = child.stdin.take().expect("stdin should be piped");
    assert!(wait_for_exit(&mut child).success());

    // The child never read its input and is gone; the pipe has no reader.
    let mut guarded = ClosedPipeGuard::new(input, "child input");
    let payload = vec![b'x'; 1024 * 1024];
    let write = guarded.write_all(&payload).and_then(|()| guarded.flush());
    let error = guarded
        .finish()
        .expect_err("writing to an exited child must be reported");

    assert!(write.is_err(), "the write itself must fail");
    assert!(
        matches!(
            error,
            StreamError::Closed {
                stream: "child input",
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
    assert_eq!(error.code(), "output_closed");
}

#[test]
fn test_isolated_home_is_not_the_real_home() {
    // Guards the fixture itself: launched children must never see user state.
    let home = tempdir().expect("temporary home");
    let command = compresso(&home);
    let configured = command
        .get_envs()
        .find(|(name, _)| *name == "HOME")
        .and_then(|(_, value)| value)
        .map(Path::new);

    assert_eq!(configured, Some(home.path()));
}
