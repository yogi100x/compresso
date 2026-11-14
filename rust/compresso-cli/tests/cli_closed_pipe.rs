//! A closed or broken stdio pipe must become a typed, reported error.
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

#[test]
fn test_cli_reports_typed_error_when_stdout_reader_closed_early() {
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
    let mut input = child.stdin.take().expect("stdin should be piped");
    input
        .write_all(b"line one\nline two\nline three\n")
        .expect("input should be written");
    drop(input);

    let status = wait_for_exit(&mut child);
    let stderr = read_stderr(&mut child);
    assert_reported_closed_output(status, &stderr);
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
