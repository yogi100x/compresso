//! Bounded liveness probe for the configured Compresso MCP server.
//!
//! This is the only module in the CLI allowed to start a child process
//! (ADR-0028). Client configuration is untrusted input: a project-scope
//! config lives inside a repository and can name any executable. The probe
//! therefore starts a configured command only when it is a trusted Compresso
//! launcher, meaning either its canonical path is the running Compresso
//! executable or an authenticated install receipt proves Compresso wrote that
//! exact entry. Everything else is reported as `not_probed` or
//! `config_modified` and is never executed. Those two verdicts are warnings:
//! the entry could not be verified, which is not evidence that it is broken.
//!
//! An entry that enables session recall (`--recall-source`) is held to the
//! stricter of the two: it must name the client's own transcript source and be
//! covered by an authenticated receipt, so a recall argument added to a
//! configuration by anyone but an approved install plan is never launched.
//!
//! A trusted launch uses the fixed adapter-written arguments, no shell, a
//! scrubbed environment, a neutral working directory, and its own process
//! group; one MCP `initialize` plus `tools/list` exchange runs under a hard
//! deadline, after which the whole process group is killed and reaped.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use compresso_core::suppress_console_window;
use compresso_integrations::{ConfiguredServer, recall_source_for_client, server_recall_source};
use serde_json::{Value, json};

/// Handshake deadline used by the real CLI.
pub(crate) const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on server output read during one probe.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Grace period for a server to exit after its input closes.
const EXIT_GRACE: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const PROTOCOL_VERSION: &str = "2024-11-05";
const PROBE_CLIENT_NAME: &str = "compresso-doctor";
/// The only environment variables a probed server inherits: what Compresso
/// needs to locate its own configuration, data, and temporary directories.
const ENVIRONMENT_ALLOWLIST: [&str; 19] = [
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "COMPRESSO_MODE",
    "COMPRESSO_TARGET_TOKENS",
    "COMPRESSO_TELEMETRY",
    "COMPRESSO_BYPASS",
    "COMPRESSO_LOG",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
];

/// Evidence that a configured launcher is a Compresso binary the user chose.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LauncherTrust {
    /// Canonical path of the running Compresso executable, when resolvable.
    pub running_executable: Option<PathBuf>,
    /// An authenticated install receipt covers the entry exactly as it is now.
    pub receipt_authenticated: bool,
}

impl LauncherTrust {
    pub(crate) fn new(running_executable: Option<&Path>, receipt_authenticated: bool) -> Self {
        Self {
            running_executable: running_executable
                .and_then(|path| std::fs::canonicalize(path).ok()),
            receipt_authenticated,
        }
    }

    /// Returns the path to execute, or `None` when the command is untrusted.
    ///
    /// A command matching the running executable is started through its
    /// canonical path so a symbolic link swapped after the check cannot
    /// redirect the launch. A receipt-authenticated command is started exactly
    /// as the approved install recorded it.
    ///
    /// `enables_recall` entries read private session transcripts once a client
    /// calls the recall tool, so being the running executable is not enough:
    /// only a receipt proves an approved install plan wrote that argument.
    fn launch_path(&self, command: &Path, enables_recall: bool) -> Option<PathBuf> {
        let canonical = std::fs::canonicalize(command).ok()?;
        if !enables_recall && self.running_executable.as_ref() == Some(&canonical) {
            return Some(canonical);
        }
        self.receipt_authenticated.then(|| command.to_path_buf())
    }
}

/// How much a diagnostic verdict matters to the command's outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Severity {
    /// Nothing to do.
    Ok,
    /// Worth the operator's attention, but not a failure.
    Warning,
    /// A real failure: the command exits non-zero.
    Error,
}

impl Severity {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Observable health of one configured Compresso MCP server entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpServerState {
    NotConfigured,
    ConfigUnreadable,
    ConfigModified,
    NotProbed,
    BinaryMissing,
    BinaryNotExecutable,
    ProbeSkipped,
    SpawnFailed,
    ExitedEarly,
    HandshakeTimeout,
    MalformedResponse,
    Healthy,
}

impl McpServerState {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::ConfigUnreadable => "config_unreadable",
            Self::ConfigModified => "config_modified",
            Self::NotProbed => "not_probed",
            Self::BinaryMissing => "binary_missing",
            Self::BinaryNotExecutable => "binary_not_executable",
            Self::ProbeSkipped => "probe_skipped",
            Self::SpawnFailed => "spawn_failed",
            Self::ExitedEarly => "exited_early",
            Self::HandshakeTimeout => "handshake_timeout",
            Self::MalformedResponse => "malformed_response",
            Self::Healthy => "healthy",
        }
    }

    /// Classifies the state for exit codes and rendering.
    ///
    /// An entry Compresso refused to launch (`not_probed`, `config_modified`)
    /// is a warning: it was never executed, so nothing is known to be broken,
    /// and a hand-edited entry is the user's to keep. Only an observed fault
    /// (an unreadable config, a missing or non-executable launcher, a crash,
    /// a timeout, a malformed answer) is an error.
    pub(crate) const fn severity(self) -> Severity {
        match self {
            Self::NotConfigured | Self::ProbeSkipped | Self::Healthy => Severity::Ok,
            Self::ConfigModified | Self::NotProbed => Severity::Warning,
            Self::ConfigUnreadable
            | Self::BinaryMissing
            | Self::BinaryNotExecutable
            | Self::SpawnFailed
            | Self::ExitedEarly
            | Self::HandshakeTimeout
            | Self::MalformedResponse => Severity::Error,
        }
    }
}

/// One health verdict with operator-facing detail and fix text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpServerHealth {
    pub state: McpServerState,
    pub detail: String,
    pub fix: String,
}

impl McpServerHealth {
    fn new(state: McpServerState, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
            fix: fix.into(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "state": self.state.code(),
            "healthy": self.state.severity() == Severity::Ok,
            "severity": self.state.severity().code(),
            "detail": self.detail,
            "fix": self.fix,
        })
    }
}

/// Classifies one configured server entry, probing it only when
/// `probe_timeout` is set, the entry has the exact adapter-written shape, and
/// `trust` identifies its command as a trusted Compresso launcher.
pub(crate) fn assess(
    server: &ConfiguredServer,
    client: &str,
    trust: &LauncherTrust,
    probe_timeout: Option<Duration>,
) -> McpServerHealth {
    let reinstall = format!("compresso integrations install {client}");
    let (command, args) = match server {
        ConfiguredServer::NotConfigured => {
            return McpServerHealth::new(
                McpServerState::NotConfigured,
                "no Compresso MCP server entry is configured",
                format!("Run `{reinstall}` (or `compresso setup`) to configure it."),
            );
        }
        ConfiguredServer::Unreadable => {
            return McpServerHealth::new(
                McpServerState::ConfigUnreadable,
                "the client configuration is unreadable or malformed",
                format!(
                    "Repair the client configuration file so it parses, then run `{reinstall}`."
                ),
            );
        }
        ConfiguredServer::Modified => {
            return McpServerHealth::new(
                McpServerState::ConfigModified,
                "the Compresso entry was edited and no longer matches the installed shape; it was not launched",
                format!("Restore the entry with `{reinstall}`, then restart the client."),
            );
        }
        ConfiguredServer::Exact { command, args } => (command, args),
    };
    let recall_source = server_recall_source(args);
    if recall_source.is_some() && recall_source != recall_source_for_client(client) {
        // Each client may enable only its own transcripts; Cursor has none.
        return McpServerHealth::new(
            McpServerState::ConfigModified,
            "the Compresso entry names a session recall source that Compresso does not install for this client; it was not launched",
            format!("Restore the entry with `{reinstall}`, then restart the client."),
        );
    }
    let shown = command.display();
    match std::fs::metadata(command) {
        Ok(metadata) if metadata.is_file() && is_executable(&metadata) => {}
        Ok(_) => {
            return McpServerHealth::new(
                McpServerState::BinaryNotExecutable,
                format!("configured launcher {shown} is not an executable file"),
                format!(
                    "Reinstall Compresso, run `{reinstall}` to rewrite the launcher path, then restart the client."
                ),
            );
        }
        Err(_) => {
            return McpServerHealth::new(
                McpServerState::BinaryMissing,
                format!("configured launcher {shown} does not exist (stale path)"),
                format!(
                    "Compresso was moved, upgraded, or removed. Reinstall it, run `{reinstall}` to rewrite the launcher path, then restart the client."
                ),
            );
        }
    }
    let Some(launch_path) = trust.launch_path(command, recall_source.is_some()) else {
        return McpServerHealth::new(
            McpServerState::NotProbed,
            if recall_source.is_some() {
                format!(
                    "the entry for {shown} enables session recall but no Compresso install receipt covers it; it was not launched"
                )
            } else {
                format!(
                    "configured command {shown} is not a launcher this Compresso installed; it was not launched"
                )
            },
            format!(
                "If you trust this configuration, run `{reinstall}` to write a verified entry, then restart the client. Otherwise remove the entry."
            ),
        );
    };
    let Some(timeout) = probe_timeout else {
        return McpServerHealth::new(
            McpServerState::ProbeSkipped,
            format!("configured launcher {shown} is executable; live handshake not run"),
            "No action required.",
        );
    };
    let manual = format!("{shown} {}", args.join(" "));
    match probe(&launch_path, args, timeout) {
        ProbeOutcome::Healthy { tools } => McpServerHealth::new(
            McpServerState::Healthy,
            format!("MCP handshake succeeded with {tools} tools"),
            "No action required.",
        ),
        ProbeOutcome::SpawnFailed => McpServerHealth::new(
            McpServerState::SpawnFailed,
            format!("configured launcher {shown} could not be started"),
            format!(
                "Check the file's permissions and architecture, reinstall Compresso, then run `{reinstall}`."
            ),
        ),
        ProbeOutcome::ExitedEarly { status } => McpServerHealth::new(
            McpServerState::ExitedEarly,
            format!(
                "server exited before completing the MCP handshake ({})",
                status.map_or_else(
                    || "terminated by a signal".to_owned(),
                    |code| format!("exit status {code}")
                )
            ),
            format!(
                "Run `{manual}` in a terminal to see the startup error, fix what it reports (other `compresso doctor` checks cover config and database), then restart the client."
            ),
        ),
        ProbeOutcome::HandshakeTimeout => McpServerHealth::new(
            McpServerState::HandshakeTimeout,
            format!(
                "server did not answer the MCP handshake within {} ms",
                timeout.as_millis()
            ),
            format!(
                "Restart the coding client to relaunch the server. If it stays unresponsive, reinstall Compresso and run `{reinstall}`."
            ),
        ),
        ProbeOutcome::MalformedResponse => McpServerHealth::new(
            McpServerState::MalformedResponse,
            "server answered with output that is not a valid Compresso MCP handshake",
            format!(
                "The configured launcher is not a working Compresso build. Reinstall Compresso and run `{reinstall}`."
            ),
        ),
    }
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeOutcome {
    Healthy { tools: usize },
    SpawnFailed,
    ExitedEarly { status: Option<i32> },
    HandshakeTimeout,
    MalformedResponse,
}

/// One unit of server output from the reader thread.
enum Frame {
    Line(String),
    /// The output was not valid UTF-8 or could not be read.
    Unreadable,
}

enum Exchange {
    Response(Value),
    Closed,
    TimedOut,
    Malformed,
}

fn probe(command: &Path, args: &[String], timeout: Duration) -> ProbeOutcome {
    let mut launch = Command::new(command);
    launch
        .args(args)
        .env_clear()
        .current_dir(neutral_directory());
    for name in ENVIRONMENT_ALLOWLIST {
        if let Some(value) = std::env::var_os(name) {
            launch.env(name, value);
        }
    }
    isolate_process_group(&mut launch);
    // A doctor run from a windowless parent must not flash a console.
    suppress_console_window(|flags| {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            launch.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = flags;
    });
    let mut child = match launch
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return ProbeOutcome::SpawnFailed,
    };
    let stdin = child.stdin.take();
    let lines = child.stdout.take().map(spawn_line_reader);
    let outcome = match (stdin, lines) {
        (Some(mut stdin), Some(lines)) => {
            let outcome = handshake(&mut stdin, &lines, Instant::now() + timeout);
            drop(stdin);
            outcome
        }
        _ => None,
    };
    let status = reap(&mut child);
    outcome.unwrap_or(ProbeOutcome::ExitedEarly { status })
}

/// Returns `None` when the server closed its pipes before answering; the
/// caller then reports the reaped exit status.
fn handshake(
    stdin: &mut ChildStdin,
    lines: &Receiver<Frame>,
    deadline: Instant,
) -> Option<ProbeOutcome> {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": PROBE_CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") }
        }
    });
    if send(stdin, &initialize).is_err() {
        return None;
    }
    let initialized = match await_response(lines, 1, deadline) {
        Exchange::Response(response) => response,
        Exchange::Closed => return None,
        Exchange::TimedOut => return Some(ProbeOutcome::HandshakeTimeout),
        Exchange::Malformed => return Some(ProbeOutcome::MalformedResponse),
    };
    if !initialized["result"]["serverInfo"].is_object() {
        return Some(ProbeOutcome::MalformedResponse);
    }
    let follow_up = [
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    ];
    if follow_up
        .iter()
        .any(|message| send(stdin, message).is_err())
    {
        return None;
    }
    let listed = match await_response(lines, 2, deadline) {
        Exchange::Response(response) => response,
        Exchange::Closed => return None,
        Exchange::TimedOut => return Some(ProbeOutcome::HandshakeTimeout),
        Exchange::Malformed => return Some(ProbeOutcome::MalformedResponse),
    };
    let tools = listed["result"]["tools"].as_array();
    let all_compresso = tools.is_some_and(|tools| {
        !tools.is_empty()
            && tools.iter().all(|tool| {
                tool["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("compresso_"))
            })
    });
    Some(match tools {
        Some(tools) if all_compresso => ProbeOutcome::Healthy { tools: tools.len() },
        _ => ProbeOutcome::MalformedResponse,
    })
}

fn send(stdin: &mut ChildStdin, message: &Value) -> std::io::Result<()> {
    let mut frame = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    frame.push(b'\n');
    stdin.write_all(&frame)?;
    stdin.flush()
}

fn await_response(lines: &Receiver<Frame>, id: u64, deadline: Instant) -> Exchange {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(remaining) {
            Ok(Frame::Unreadable) => return Exchange::Malformed,
            Ok(Frame::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    return Exchange::Malformed;
                };
                if !message.is_object() {
                    return Exchange::Malformed;
                }
                if message["id"].as_u64() == Some(id) {
                    return Exchange::Response(message);
                }
            }
            Err(RecvTimeoutError::Timeout) => return Exchange::TimedOut,
            Err(RecvTimeoutError::Disconnected) => return Exchange::Closed,
        }
    }
}

fn spawn_line_reader(stdout: impl std::io::Read + Send + 'static) -> Receiver<Frame> {
    let (sender, receiver) = mpsc::channel();
    // Detached on purpose: the thread ends at EOF, at the byte cap, or when
    // the receiver is dropped, and never outlives a reaped child's pipe.
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout.take(MAX_RESPONSE_BYTES));
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if sender.send(Frame::Line(line)).is_err() {
                        break;
                    }
                }
                // Output that cannot be read as text came from a live server,
                // so it must not look like the server closing its pipe.
                Err(_) => {
                    let _ = sender.send(Frame::Unreadable);
                    break;
                }
            }
        }
    });
    receiver
}

/// Waits briefly for a clean exit; otherwise kills the child's whole process
/// group and reaps the child. The group is signalled only while the unreaped
/// child still reserves its process id, so the id cannot name another group.
fn reap(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + EXIT_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            _ => break,
        }
    }
    kill_process_group(child.id());
    let _ = child.kill();
    child.wait().ok().and_then(|status| status.code())
}

/// A working directory that is never the repository being inspected. The
/// temporary directory itself is not used because it can be redirected inside
/// a repository; the root of its volume cannot be one.
fn neutral_directory() -> PathBuf {
    if cfg!(unix) {
        PathBuf::from("/")
    } else {
        volume_root(&std::env::temp_dir())
    }
}

/// The top of the volume that holds an absolute `path`. A relative path has
/// no volume root and is returned unchanged.
fn volume_root(path: &Path) -> PathBuf {
    match path.ancestors().last() {
        Some(root) if root.has_root() => root.to_path_buf(),
        _ => path.to_path_buf(),
    }
}

#[cfg(unix)]
fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_command: &mut Command) {}

/// Kills every process in the probed server's group. The child leads its own
/// group, so the group id equals its process id. The system `kill` utility is
/// invoked by absolute path with a fixed argument vector.
#[cfg(unix)]
fn kill_process_group(process_id: u32) {
    for kill in ["/bin/kill", "/usr/bin/kill"] {
        if !Path::new(kill).is_file() {
            continue;
        }
        let _ = Command::new(kill)
            .args(["-KILL", "--", &format!("-{process_id}")])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return;
    }
}

#[cfg(not(unix))]
fn kill_process_group(_process_id: u32) {}

/// The base user-scope argument vector every adapter writes. Claude Code and
/// Codex entries may additionally end in their own `--recall-source`.
pub(crate) fn user_scope_args() -> Vec<String> {
    vec!["mcp".to_owned(), "serve".to_owned()]
}

/// One client's user-scope MCP configuration location.
pub(crate) struct ClientConfig {
    pub client: &'static str,
    pub format: compresso_integrations::ConfigFormat,
    pub path: PathBuf,
}

/// User-scope configuration files of every supported coding client.
pub(crate) fn user_client_configs(home: &Path) -> [ClientConfig; 3] {
    use compresso_integrations::ConfigFormat;
    [
        ClientConfig {
            client: "claude-code",
            format: ConfigFormat::Json,
            path: home.join(".claude.json"),
        },
        ClientConfig {
            client: "codex",
            format: ConfigFormat::Toml,
            path: home.join(".codex/config.toml"),
        },
        ClientConfig {
            client: "cursor",
            format: ConfigFormat::Json,
            path: home.join(".cursor/mcp.json"),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange_for(output: impl std::io::Read + Send + 'static) -> Exchange {
        let lines = spawn_line_reader(output);
        await_response(&lines, 1, Instant::now() + Duration::from_secs(10))
    }

    #[test]
    fn test_volume_root_is_the_top_of_an_absolute_path_only() {
        let root = Path::new(std::path::MAIN_SEPARATOR_STR);
        let nested = root.join("repo").join("tmp").join("probe");
        assert_eq!(volume_root(&nested), root);
        assert_eq!(volume_root(root), root);
        let relative = Path::new("repo").join("tmp");
        assert_eq!(volume_root(&relative), relative);
    }

    #[test]
    fn test_invalid_utf8_output_is_malformed_not_an_early_exit() {
        let output = std::io::Cursor::new(vec![0xff, 0xfe, b'\n']);
        assert!(matches!(exchange_for(output), Exchange::Malformed));
    }

    #[test]
    fn test_output_past_the_byte_cap_is_malformed_not_an_early_exit() {
        // Well-formed notifications that never answer, past the byte cap.
        let line = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n";
        let repeats = usize::try_from(MAX_RESPONSE_BYTES).expect("cap fits usize") / line.len() + 2;
        let output = std::io::Cursor::new(line.repeat(repeats));
        assert!(matches!(exchange_for(output), Exchange::Malformed));
    }

    #[test]
    fn test_output_that_ends_without_a_response_is_closed() {
        let output = std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":7}\n".to_vec());
        assert!(matches!(exchange_for(output), Exchange::Closed));
    }
}
