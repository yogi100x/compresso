//! Read-only view of the Compresso MCP server entry a client is configured to
//! launch.
//!
//! Health diagnostics need the exact command a coding client would start. This
//! module never mutates configuration and never executes anything: it reports
//! the entry only when it still has the exact shape a Compresso adapter writes,
//! so a caller cannot be steered into launching an arbitrary user-edited
//! command.

use std::fs;
use std::path::{Path, PathBuf};

use crate::text_fidelity::{strip_bom, strip_bom_str};
use crate::{ConfigFormat, InstallReceipt};

/// Flag that enables the `compresso_recall` tool for one transcript source.
pub(crate) const RECALL_SOURCE_FLAG: &str = "--recall-source";
const CLAUDE_CODE_CLIENT: &str = "claude-code";
const CODEX_CLIENT: &str = "codex";

/// The transcript source a client's server entry may enable, if any.
///
/// A client is only ever given its own transcripts. Cursor has no supported
/// transcript format, so its entry never carries a recall source.
pub fn recall_source_for_client(client: &str) -> Option<&'static str> {
    match client {
        CLAUDE_CODE_CLIENT => Some(CLAUDE_CODE_CLIENT),
        CODEX_CLIENT => Some(CODEX_CLIENT),
        _ => None,
    }
}

/// The recall source named by adapter-shaped server arguments, if any.
///
/// Returns `Some` only when the arguments end in a
/// `--recall-source <known source>` pair.
pub fn server_recall_source(args: &[String]) -> Option<&str> {
    match args {
        [.., flag, source] if flag == RECALL_SOURCE_FLAG && is_recall_source(source) => {
            Some(source.as_str())
        }
        _ => None,
    }
}

pub(crate) fn is_recall_source(source: &str) -> bool {
    matches!(source, CLAUDE_CODE_CLIENT | CODEX_CLIENT)
}

/// Appends the client's own recall source to adapter-written base arguments.
pub(crate) fn with_recall_source(mut args: Vec<String>, client: &str) -> Vec<String> {
    if let Some(source) = recall_source_for_client(client) {
        args.extend([RECALL_SOURCE_FLAG.to_owned(), source.to_owned()]);
    }
    args
}

/// Arguments that select the main-session profile a default Claude Code
/// project install writes (E17-51, D116): the lean tools plus the two tools
/// the `/compresso` session commands call. They follow the base arguments.
pub(crate) const LEAN_SESSION_ARGS: [&str; 3] = ["--tools", "lean", "--session-tools"];

/// Appends the main-session profile arguments to adapter-written base
/// arguments, before any `--edits`.
pub(crate) fn with_lean_session(mut args: Vec<String>) -> Vec<String> {
    args.extend(LEAN_SESSION_ARGS.iter().map(|arg| (*arg).to_owned()));
    args
}

/// Whether adapter-shaped server arguments select the main-session profile
/// (`--tools lean --session-tools`).
pub fn server_serves_lean_session(args: &[String]) -> bool {
    args.windows(LEAN_SESSION_ARGS.len())
        .any(|window| window.iter().zip(LEAN_SESSION_ARGS).all(|(a, b)| a == b))
}

/// Flag that enables the opt-in `compresso_edit` tool (E17-32, D97).
pub(crate) const EDITS_FLAG: &str = "--edits";

/// Appends the edit-tool flag to adapter-written base arguments. It precedes
/// any recall source, so the recall pair stays last.
pub(crate) fn with_edits(mut args: Vec<String>) -> Vec<String> {
    args.push(EDITS_FLAG.to_owned());
    args
}

/// Flag that enables the opt-in `compresso_sql` tool (E17-50, D115); its
/// value names the environment variable holding the database URL.
pub(crate) const SQL_ENV_FLAG: &str = "--sql-env";
const SQL_ENV_PREFIX: &str = "COMPRESSO_SQL_";

/// Whether `name` is an acceptable `--sql-env` variable name:
/// `COMPRESSO_SQL_` and at least one of `A-Z`, `0-9`, `_` (the same rule the
/// server applies; this crate does not link the database crate).
pub fn is_sql_env_name(name: &str) -> bool {
    name.strip_prefix(SQL_ENV_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

/// Appends `--sql-env NAME` to adapter-written base arguments, after any
/// `--edits` and before any recall source.
pub(crate) fn with_sql_env(mut args: Vec<String>, name: &str) -> Vec<String> {
    args.extend([SQL_ENV_FLAG.to_owned(), name.to_owned()]);
    args
}

/// Flag that enables the opt-in `compresso_mongo` tool (E17-52, D117); its
/// value names the environment variable holding the server URL.
pub(crate) const MONGO_ENV_FLAG: &str = "--mongo-env";
const MONGO_ENV_PREFIX: &str = "COMPRESSO_MONGO_";

/// Whether `name` is an acceptable `--mongo-env` variable name:
/// `COMPRESSO_MONGO_` and at least one of `A-Z`, `0-9`, `_` (the same rule
/// the server applies).
pub fn is_mongo_env_name(name: &str) -> bool {
    name.strip_prefix(MONGO_ENV_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

/// Appends `--mongo-env NAME` to adapter-written base arguments, after any
/// `--sql-env NAME` and before any recall source.
pub(crate) fn with_mongo_env(mut args: Vec<String>, name: &str) -> Vec<String> {
    args.extend([MONGO_ENV_FLAG.to_owned(), name.to_owned()]);
    args
}

/// Whether `args` is `base`, optionally followed (Claude Code only) by the
/// main-session profile `--tools lean --session-tools`, optionally followed
/// by `--edits`, optionally followed by `--sql-env NAME` and then by
/// `--mongo-env NAME` (valid names; never with the lean profile, which
/// refuses both), optionally followed by `client`'s own recall source, in
/// that canonical order. Another client's source, a repeated flag, or any
/// other argument never matches.
pub(crate) fn is_adapter_args(args: &[String], base: &[String], client: &str) -> bool {
    let Some(mut rest) = args.strip_prefix(base) else {
        return false;
    };
    let mut lean = false;
    if client == CLAUDE_CODE_CLIENT
        && rest.len() >= LEAN_SESSION_ARGS.len()
        && rest
            .iter()
            .zip(LEAN_SESSION_ARGS)
            .all(|(argument, expected)| argument == expected)
    {
        rest = &rest[LEAN_SESSION_ARGS.len()..];
        lean = true;
    }
    if let [flag, tail @ ..] = rest
        && flag == EDITS_FLAG
    {
        rest = tail;
    }
    if let [flag, name, tail @ ..] = rest
        && flag == SQL_ENV_FLAG
        && is_sql_env_name(name)
        && !lean
    {
        rest = tail;
    }
    if let [flag, name, tail @ ..] = rest
        && flag == MONGO_ENV_FLAG
        && is_mongo_env_name(name)
        && !lean
    {
        rest = tail;
    }
    rest.is_empty() || rest == with_recall_source(Vec::new(), client).as_slice()
}

/// The `--mongo-env` variable name adapter-shaped server arguments carry, if
/// any.
pub fn server_mongo_env(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find(|pair| pair[0] == MONGO_ENV_FLAG && is_mongo_env_name(&pair[1]))
        .map(|pair| pair[1].as_str())
}

/// The `--sql-env` variable name adapter-shaped server arguments carry, if
/// any.
pub fn server_sql_env(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find(|pair| pair[0] == SQL_ENV_FLAG && is_sql_env_name(&pair[1]))
        .map(|pair| pair[1].as_str())
}

/// Whether adapter-shaped server arguments enable the edit tool.
pub fn server_enables_edits(args: &[String]) -> bool {
    args.iter().any(|argument| argument == EDITS_FLAG)
}

/// The only client whose recall source an entry in a `format` configuration
/// may carry.
const fn format_recall_client(format: ConfigFormat) -> &'static str {
    match format {
        // Whole-file Markdown (the exploration agent) is never a server
        // entry; it is grouped with the client that reads it.
        ConfigFormat::Json | ConfigFormat::Markdown => CLAUDE_CODE_CLIENT,
        ConfigFormat::Toml => CODEX_CLIENT,
    }
}

/// Largest client configuration file inspected for a server entry.
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// The Compresso MCP server entry found in one client configuration file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfiguredServer {
    /// The configuration file or the Compresso entry does not exist.
    NotConfigured,
    /// The configuration exists but cannot be read or parsed.
    Unreadable,
    /// A Compresso entry exists but no longer has the adapter-written shape.
    Modified,
    /// The entry has the exact adapter-written shape.
    Exact {
        /// Absolute launcher path the client starts.
        command: PathBuf,
        /// Exact fixed arguments the client passes.
        args: Vec<String>,
    },
}

/// Reads the Compresso server entry from `config_path` without mutation.
///
/// `expected_args` is the exact base argument vector the adapter writes for
/// the selected scope. The entry may additionally end in the one recall
/// source an adapter writes into that configuration format
/// (`--recall-source claude-code` for JSON, `--recall-source codex` for TOML).
/// Cursor shares the JSON format and never carries a recall source, so a
/// caller that knows the client must still compare [`server_recall_source`]
/// with [`recall_source_for_client`]. Any other argument vector, a relative
/// command, or an unexpected field is reported as
/// [`ConfiguredServer::Modified`].
pub fn read_configured_server(
    format: ConfigFormat,
    config_path: &Path,
    expected_args: &[String],
) -> ConfiguredServer {
    let bytes = match fs::metadata(config_path) {
        Ok(metadata) if metadata.len() > MAX_CONFIG_BYTES => return ConfiguredServer::Unreadable,
        Ok(_) => match fs::read(config_path) {
            Ok(bytes) => bytes,
            Err(_) => return ConfiguredServer::Unreadable,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ConfiguredServer::NotConfigured;
        }
        Err(_) => return ConfiguredServer::Unreadable,
    };
    match format {
        ConfigFormat::Json => read_json_entry(&bytes, expected_args, format),
        ConfigFormat::Toml => read_toml_entry(&bytes, expected_args, format),
        ConfigFormat::Markdown => ConfiguredServer::Unreadable,
    }
}

/// Whether `receipt` proves that Compresso itself wrote the server entry
/// currently stored in `config_path`.
///
/// The receipt must be active, name this exact configuration file, and record
/// an owned-value hash equal to the hash of the entry as it exists now. Any
/// later edit to the entry, including its command, breaks the match, so a
/// caller can treat a `true` result as "this launcher path is the one an
/// approved Compresso install plan recorded".
pub fn receipt_authenticates_server(
    receipt: &InstallReceipt,
    format: ConfigFormat,
    config_path: &Path,
) -> bool {
    if receipt.uninstalled_at.is_some() || receipt.config_path != config_path {
        return false;
    }
    let (key, current) = match format {
        ConfigFormat::Json => ("mcpServers.compresso", current_json_hash(config_path)),
        ConfigFormat::Toml => ("mcp_servers.compresso", current_toml_hash(config_path)),
        ConfigFormat::Markdown => return false,
    };
    match (receipt.owned_value_hashes.get(key), current) {
        (Some(installed), Some(current)) => *installed == current,
        _ => false,
    }
}

fn read_bounded(config_path: &Path) -> Option<Vec<u8>> {
    let metadata = fs::metadata(config_path).ok()?;
    if metadata.len() > MAX_CONFIG_BYTES {
        return None;
    }
    fs::read(config_path).ok()
}

fn current_json_hash(config_path: &Path) -> Option<String> {
    let bytes = read_bounded(config_path)?;
    let document: serde_json::Value = serde_json::from_slice(strip_bom(&bytes)).ok()?;
    let entry = document.get("mcpServers")?.get("compresso")?;
    let bytes = serde_json::to_vec(entry).ok()?;
    Some(blake3::hash(&bytes).to_hex().to_string())
}

fn current_toml_hash(config_path: &Path) -> Option<String> {
    let bytes = read_bounded(config_path)?;
    let document = strip_bom_str(std::str::from_utf8(&bytes).ok()?)
        .parse::<toml_edit::DocumentMut>()
        .ok()?;
    let entry = document.as_item().get("mcp_servers")?.get("compresso")?;
    Some(crate::plan::hash_toml_owned_item(entry))
}

fn read_json_entry(
    bytes: &[u8],
    expected_args: &[String],
    format: ConfigFormat,
) -> ConfiguredServer {
    // Adapters keep an existing byte-order mark, so it is not a modification.
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(strip_bom(bytes)) else {
        return ConfiguredServer::Unreadable;
    };
    let Some(document) = document.as_object() else {
        return ConfiguredServer::Unreadable;
    };
    let Some(entry) = document
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .and_then(|servers| servers.get("compresso"))
    else {
        return ConfiguredServer::NotConfigured;
    };
    let Some(entry) = entry.as_object() else {
        return ConfiguredServer::Modified;
    };
    let only_known_fields = entry
        .keys()
        .all(|key| matches!(key.as_str(), "type" | "command" | "args"));
    let stdio = entry
        .get("type")
        .is_none_or(|kind| kind.as_str() == Some("stdio"));
    let args = entry.get("args").and_then(|args| {
        args.as_array()?
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
    });
    exact_entry(
        only_known_fields && stdio,
        entry.get("command").and_then(serde_json::Value::as_str),
        args,
        expected_args,
        format,
    )
}

fn read_toml_entry(
    bytes: &[u8],
    expected_args: &[String],
    format: ConfigFormat,
) -> ConfiguredServer {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return ConfiguredServer::Unreadable;
    };
    let Ok(document) = strip_bom_str(text).parse::<toml_edit::DocumentMut>() else {
        return ConfiguredServer::Unreadable;
    };
    let Some(entry) = document
        .as_item()
        .get("mcp_servers")
        .and_then(|servers| servers.get("compresso"))
    else {
        return ConfiguredServer::NotConfigured;
    };
    let Some(table) = entry.as_table_like() else {
        return ConfiguredServer::Modified;
    };
    let only_known_fields = table
        .iter()
        .all(|(key, _)| matches!(key, "command" | "args"));
    let args = table
        .get("args")
        .and_then(toml_edit::Item::as_array)
        .and_then(|args| {
            args.iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        });
    exact_entry(
        only_known_fields,
        table.get("command").and_then(toml_edit::Item::as_str),
        args,
        expected_args,
        format,
    )
}

fn exact_entry(
    shape_ok: bool,
    command: Option<&str>,
    args: Option<Vec<String>>,
    expected_args: &[String],
    format: ConfigFormat,
) -> ConfiguredServer {
    match (command, args) {
        (Some(command), Some(args))
            if shape_ok
                && Path::new(command).is_absolute()
                && is_adapter_args(&args, expected_args, format_recall_client(format)) =>
        {
            ConfiguredServer::Exact {
                command: PathBuf::from(command),
                args,
            }
        }
        _ => ConfiguredServer::Modified,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    /// Absolute launcher path used by the fixtures. `/opt/compresso` is not
    /// absolute on Windows, where the reader would rightly call it modified.
    #[cfg(not(windows))]
    const OPT_COMPRESSO: &str = "/opt/compresso";
    #[cfg(windows)]
    const OPT_COMPRESSO: &str = r"C:\opt\compresso";

    /// Fixture writer: rewrites `/opt/compresso` to the platform's absolute
    /// launcher path (escaped for JSON and TOML strings) before writing.
    mod fs {
        pub fn write(
            path: impl AsRef<std::path::Path>,
            contents: impl AsRef<[u8]>,
        ) -> std::io::Result<()> {
            let text = String::from_utf8_lossy(contents.as_ref()).replace(
                "/opt/compresso",
                &super::OPT_COMPRESSO.replace('\\', "\\\\"),
            );
            std::fs::write(path, text)
        }
    }

    fn serve_args() -> Vec<String> {
        vec!["mcp".to_owned(), "serve".to_owned()]
    }

    #[test]
    fn test_missing_config_and_missing_entry_are_not_configured() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        assert_eq!(
            read_configured_server(ConfigFormat::Json, &path, &serve_args()),
            ConfiguredServer::NotConfigured
        );
        fs::write(&path, "{\"mcpServers\":{\"other\":{}}}").expect("config written");
        assert_eq!(
            read_configured_server(ConfigFormat::Json, &path, &serve_args()),
            ConfiguredServer::NotConfigured
        );
    }

    #[test]
    fn test_exact_json_and_toml_entries_return_command_and_args() {
        let directory = tempdir().expect("temp directory");
        let json = directory.path().join("config.json");
        fs::write(
            &json,
            "{\"mcpServers\":{\"compresso\":{\"type\":\"stdio\",\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\"]}}}",
        )
        .expect("json written");
        let toml = directory.path().join("config.toml");
        fs::write(
            &toml,
            "[mcp_servers.compresso]\ncommand = \"/opt/compresso\"\nargs = [\"mcp\", \"serve\"]\n",
        )
        .expect("toml written");
        let expected = ConfiguredServer::Exact {
            command: PathBuf::from(OPT_COMPRESSO),
            args: serve_args(),
        };
        assert_eq!(
            read_configured_server(ConfigFormat::Json, &json, &serve_args()),
            expected
        );
        assert_eq!(
            read_configured_server(ConfigFormat::Toml, &toml, &serve_args()),
            expected
        );
    }

    #[test]
    fn test_user_edited_entries_are_modified_and_never_exact() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        for entry in [
            "{\"command\":\"compresso\",\"args\":[\"mcp\",\"serve\"]}",
            "{\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\",\"--extra\"]}",
            "{\"command\":\"/bin/sh\",\"args\":[\"-c\",\"anything\"]}",
            "{\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\"],\"env\":{\"A\":\"B\"}}",
            "{\"type\":\"http\",\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\"]}",
            "\"/opt/compresso\"",
        ] {
            fs::write(
                &path,
                format!("{{\"mcpServers\":{{\"compresso\":{entry}}}}}"),
            )
            .expect("config written");
            assert_eq!(
                read_configured_server(ConfigFormat::Json, &path, &serve_args()),
                ConfiguredServer::Modified,
                "{entry}"
            );
        }
        let toml = directory.path().join("config.toml");
        fs::write(
            &toml,
            "[mcp_servers.compresso]\ncommand = \"/opt/compresso\"\nargs = [\"mcp\", \"serve\"]\nenabled = false\n",
        )
        .expect("toml written");
        assert_eq!(
            read_configured_server(ConfigFormat::Toml, &toml, &serve_args()),
            ConfiguredServer::Modified
        );
    }

    #[test]
    fn test_own_recall_source_suffix_is_exact_for_each_format() {
        let directory = tempdir().expect("temp directory");
        let json = directory.path().join("config.json");
        fs::write(
            &json,
            "{\"mcpServers\":{\"compresso\":{\"type\":\"stdio\",\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\",\"--recall-source\",\"claude-code\"]}}}",
        )
        .expect("json written");
        let toml = directory.path().join("config.toml");
        fs::write(
            &toml,
            "[mcp_servers.compresso]\ncommand = \"/opt/compresso\"\nargs = [\"mcp\", \"serve\", \"--recall-source\", \"codex\"]\n",
        )
        .expect("toml written");
        let ConfiguredServer::Exact { args, .. } =
            read_configured_server(ConfigFormat::Json, &json, &serve_args())
        else {
            panic!("JSON entry with its own recall source should be exact");
        };
        assert_eq!(server_recall_source(&args), Some("claude-code"));
        let ConfiguredServer::Exact { args, .. } =
            read_configured_server(ConfigFormat::Toml, &toml, &serve_args())
        else {
            panic!("TOML entry with its own recall source should be exact");
        };
        assert_eq!(server_recall_source(&args), Some("codex"));
        assert_eq!(server_recall_source(&serve_args()), None);
    }

    /// The health readers (`doctor`, `integrations status`) see an
    /// installer-written `--edits` entry as exact, not modified.
    #[test]
    fn test_an_edits_entry_is_exact_and_keeps_its_recall_source() {
        let directory = tempdir().expect("temp directory");
        let json = directory.path().join("config.json");
        fs::write(
            &json,
            "{\"mcpServers\":{\"compresso\":{\"type\":\"stdio\",\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\",\"--edits\",\"--recall-source\",\"claude-code\"]}}}",
        )
        .expect("json written");
        let ConfiguredServer::Exact { args, .. } =
            read_configured_server(ConfigFormat::Json, &json, &serve_args())
        else {
            panic!("an --edits entry should be exact");
        };
        assert!(server_enables_edits(&args));
        assert_eq!(server_recall_source(&args), Some("claude-code"));
    }

    #[test]
    fn test_foreign_repeated_or_trailing_recall_arguments_are_modified() {
        let directory = tempdir().expect("temp directory");
        let json = directory.path().join("config.json");
        for args in [
            "[\"mcp\",\"serve\",\"--recall-source\",\"codex\"]",
            "[\"mcp\",\"serve\",\"--recall-source\",\"cursor\"]",
            "[\"mcp\",\"serve\",\"--recall-source\"]",
            "[\"mcp\",\"serve\",\"--recall-source=claude-code\"]",
            "[\"mcp\",\"serve\",\"--recall-source\",\"claude-code\",\"--recall-source\",\"codex\"]",
            "[\"mcp\",\"serve\",\"--recall-source\",\"claude-code\",\"--recall-source\",\"claude-code\"]",
            "[\"mcp\",\"serve\",\"--recall-source\",\"claude-code\",\"--extra\"]",
            "[\"--recall-source\",\"claude-code\",\"mcp\",\"serve\"]",
        ] {
            fs::write(
                &json,
                format!(
                    "{{\"mcpServers\":{{\"compresso\":{{\"command\":\"/opt/compresso\",\"args\":{args}}}}}}}"
                ),
            )
            .expect("json written");
            assert_eq!(
                read_configured_server(ConfigFormat::Json, &json, &serve_args()),
                ConfiguredServer::Modified,
                "{args}"
            );
        }
        let toml = directory.path().join("config.toml");
        fs::write(
            &toml,
            "[mcp_servers.compresso]\ncommand = \"/opt/compresso\"\nargs = [\"mcp\", \"serve\", \"--recall-source\", \"claude-code\"]\n",
        )
        .expect("toml written");
        assert_eq!(
            read_configured_server(ConfigFormat::Toml, &toml, &serve_args()),
            ConfiguredServer::Modified
        );
    }

    #[test]
    fn test_recall_source_mapping_gives_each_client_only_its_own_transcripts() {
        assert_eq!(recall_source_for_client("claude-code"), Some("claude-code"));
        assert_eq!(recall_source_for_client("codex"), Some("codex"));
        assert_eq!(recall_source_for_client("cursor"), None);
        assert_eq!(
            with_recall_source(serve_args(), "cursor"),
            serve_args(),
            "Cursor never gains a recall source"
        );
        assert!(is_adapter_args(
            &with_recall_source(serve_args(), "codex"),
            &serve_args(),
            "codex"
        ));
        let edits = with_edits(serve_args());
        assert!(is_adapter_args(&edits, &serve_args(), "codex"));
        assert!(is_adapter_args(
            &with_recall_source(edits.clone(), "codex"),
            &serve_args(),
            "codex"
        ));
        assert!(server_enables_edits(&edits));
        assert!(!server_enables_edits(&serve_args()));
        // Canonical order only: --edits after the recall pair, or twice, is
        // not an adapter-written entry.
        let mut misplaced = with_recall_source(serve_args(), "codex");
        misplaced.push(EDITS_FLAG.to_owned());
        assert!(!is_adapter_args(&misplaced, &serve_args(), "codex"));
        assert!(!is_adapter_args(&with_edits(edits), &serve_args(), "codex"));
        assert!(!is_adapter_args(
            &with_recall_source(serve_args(), "codex"),
            &serve_args(),
            "claude-code"
        ));
    }

    #[test]
    fn test_a_main_session_entry_is_exact_only_for_claude_code_in_canonical_order() {
        let lean = with_lean_session(serve_args());
        assert!(server_serves_lean_session(&lean));
        assert!(!server_serves_lean_session(&serve_args()));
        assert!(is_adapter_args(&lean, &serve_args(), "claude-code"));
        let full = with_recall_source(with_edits(lean.clone()), "claude-code");
        assert!(is_adapter_args(&full, &serve_args(), "claude-code"));
        assert!(server_enables_edits(&full));
        // Codex and Cursor never write it.
        assert!(!is_adapter_args(&lean, &serve_args(), "codex"));
        assert!(!is_adapter_args(&lean, &serve_args(), "cursor"));
        // Out of order, partial, or repeated: not adapter-written.
        let after_edits = with_lean_session(with_edits(serve_args()));
        assert!(!is_adapter_args(&after_edits, &serve_args(), "claude-code"));
        let mut partial = serve_args();
        partial.extend(["--tools".to_owned(), "lean".to_owned()]);
        assert!(!is_adapter_args(&partial, &serve_args(), "claude-code"));
        let twice = with_lean_session(lean.clone());
        assert!(!is_adapter_args(&twice, &serve_args(), "claude-code"));
        // The lean profile refuses --sql-env, so that shape is never written.
        let sql = with_sql_env(lean, "COMPRESSO_SQL_MAIN");
        assert!(!is_adapter_args(&sql, &serve_args(), "claude-code"));
    }

    #[test]
    fn test_a_sql_env_entry_is_exact_only_in_canonical_order_with_a_valid_name() {
        let name = "COMPRESSO_SQL_MAIN";
        let sql = with_sql_env(serve_args(), name);
        assert!(is_adapter_args(&sql, &serve_args(), "codex"));
        assert_eq!(server_sql_env(&sql), Some(name));
        assert_eq!(server_sql_env(&serve_args()), None);
        let full = with_recall_source(with_sql_env(with_edits(serve_args()), name), "claude-code");
        assert!(is_adapter_args(&full, &serve_args(), "claude-code"));
        assert!(server_enables_edits(&full));
        // Out of order, repeated, or with an invalid name: not adapter-written.
        let swapped = with_edits(with_sql_env(serve_args(), name));
        assert!(!is_adapter_args(&swapped, &serve_args(), "codex"));
        let twice = with_sql_env(sql.clone(), name);
        assert!(!is_adapter_args(&twice, &serve_args(), "codex"));
        let mut after_recall = with_recall_source(serve_args(), "codex");
        after_recall.extend([SQL_ENV_FLAG.to_owned(), name.to_owned()]);
        assert!(!is_adapter_args(&after_recall, &serve_args(), "codex"));
        for invalid in [
            "DATABASE_URL",
            "COMPRESSO_SQL_",
            "COMPRESSO_SQL_a",
            "--edits",
        ] {
            let entry = with_sql_env(serve_args(), invalid);
            assert!(
                !is_adapter_args(&entry, &serve_args(), "codex"),
                "{invalid}"
            );
            assert_eq!(server_sql_env(&entry), None);
        }
        let missing_value = with_edits(serve_args())
            .into_iter()
            .chain([SQL_ENV_FLAG.to_owned()])
            .collect::<Vec<_>>();
        assert!(!is_adapter_args(&missing_value, &serve_args(), "codex"));
    }

    #[test]
    fn test_a_mongo_env_entry_is_exact_only_in_canonical_order_with_a_valid_name() {
        let name = "COMPRESSO_MONGO_MAIN";
        let sql = "COMPRESSO_SQL_MAIN";
        let mongo = with_mongo_env(serve_args(), name);
        assert!(is_adapter_args(&mongo, &serve_args(), "codex"));
        assert_eq!(server_mongo_env(&mongo), Some(name));
        assert_eq!(server_mongo_env(&serve_args()), None);
        assert_eq!(server_sql_env(&mongo), None);
        let full = with_recall_source(
            with_mongo_env(with_sql_env(with_edits(serve_args()), sql), name),
            "claude-code",
        );
        assert!(is_adapter_args(&full, &serve_args(), "claude-code"));
        assert_eq!(server_sql_env(&full), Some(sql));
        assert_eq!(server_mongo_env(&full), Some(name));
        // Out of order, repeated, or with an invalid name: not adapter-written.
        let swapped = with_sql_env(with_mongo_env(serve_args(), name), sql);
        assert!(!is_adapter_args(&swapped, &serve_args(), "codex"));
        let twice = with_mongo_env(mongo.clone(), name);
        assert!(!is_adapter_args(&twice, &serve_args(), "codex"));
        let mut after_recall = with_recall_source(serve_args(), "codex");
        after_recall.extend([MONGO_ENV_FLAG.to_owned(), name.to_owned()]);
        assert!(!is_adapter_args(&after_recall, &serve_args(), "codex"));
        for invalid in [
            "MONGO_URL",
            "COMPRESSO_MONGO_",
            "COMPRESSO_MONGO_a",
            "COMPRESSO_SQL_MAIN",
            "--edits",
        ] {
            let entry = with_mongo_env(serve_args(), invalid);
            assert!(
                !is_adapter_args(&entry, &serve_args(), "codex"),
                "{invalid}"
            );
            assert_eq!(server_mongo_env(&entry), None);
        }
    }

    #[test]
    fn test_byte_order_mark_does_not_hide_an_exact_entry() {
        let directory = tempdir().expect("temp directory");
        let json = directory.path().join("config.json");
        fs::write(
            &json,
            "\u{feff}{\"mcpServers\":{\"compresso\":{\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\"]}}}",
        )
        .expect("json written");
        let toml = directory.path().join("config.toml");
        fs::write(
            &toml,
            "\u{feff}[mcp_servers.compresso]\r\ncommand = \"/opt/compresso\"\r\nargs = [\"mcp\", \"serve\"]\r\n",
        )
        .expect("toml written");
        for (format, path) in [(ConfigFormat::Json, &json), (ConfigFormat::Toml, &toml)] {
            assert!(
                matches!(
                    read_configured_server(format, path, &serve_args()),
                    ConfiguredServer::Exact { .. }
                ),
                "{format:?}"
            );
        }
    }

    #[test]
    fn test_malformed_config_is_unreadable() {
        let directory = tempdir().expect("temp directory");
        let json = directory.path().join("config.json");
        fs::write(&json, "{").expect("json written");
        let toml = directory.path().join("config.toml");
        fs::write(&toml, "mcp_servers = [").expect("toml written");
        assert_eq!(
            read_configured_server(ConfigFormat::Json, &json, &serve_args()),
            ConfiguredServer::Unreadable
        );
        assert_eq!(
            read_configured_server(ConfigFormat::Toml, &toml, &serve_args()),
            ConfiguredServer::Unreadable
        );
    }

    #[test]
    fn test_byte_order_mark_does_not_change_the_owned_entry_hash() {
        let directory = tempdir().expect("temp directory");
        let json = "{\"mcpServers\":{\"compresso\":{\"command\":\"/opt/compresso\",\"args\":[\"mcp\",\"serve\"]}}}";
        let toml =
            "[mcp_servers.compresso]\ncommand = \"/opt/compresso\"\nargs = [\"mcp\", \"serve\"]\n";
        let plain_json = directory.path().join("plain.json");
        let marked_json = directory.path().join("marked.json");
        let plain_toml = directory.path().join("plain.toml");
        let marked_toml = directory.path().join("marked.toml");
        fs::write(&plain_json, json).expect("json written");
        fs::write(&marked_json, format!("\u{feff}{json}")).expect("json written");
        fs::write(&plain_toml, toml).expect("toml written");
        fs::write(&marked_toml, format!("\u{feff}{toml}")).expect("toml written");

        let json_hash = current_json_hash(&plain_json);
        assert!(json_hash.is_some());
        assert_eq!(current_json_hash(&marked_json), json_hash);
        let toml_hash = current_toml_hash(&plain_toml);
        assert!(toml_hash.is_some());
        assert_eq!(current_toml_hash(&marked_toml), toml_hash);
    }
}
