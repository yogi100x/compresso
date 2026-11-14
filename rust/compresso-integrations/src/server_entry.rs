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

use crate::ConfigFormat;

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
/// `expected_args` is the exact argument vector the adapter writes for the
/// selected scope. Any other argument vector, a relative command, or an
/// unexpected field is reported as [`ConfiguredServer::Modified`].
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
        ConfigFormat::Json => read_json_entry(&bytes, expected_args),
        ConfigFormat::Toml => read_toml_entry(&bytes, expected_args),
    }
}

fn read_json_entry(bytes: &[u8], expected_args: &[String]) -> ConfiguredServer {
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(bytes) else {
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
    )
}

fn read_toml_entry(bytes: &[u8], expected_args: &[String]) -> ConfiguredServer {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return ConfiguredServer::Unreadable;
    };
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
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
    )
}

fn exact_entry(
    shape_ok: bool,
    command: Option<&str>,
    args: Option<Vec<String>>,
    expected_args: &[String],
) -> ConfiguredServer {
    match (command, args) {
        (Some(command), Some(args))
            if shape_ok && Path::new(command).is_absolute() && args == expected_args =>
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
            command: PathBuf::from("/opt/compresso"),
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
}
