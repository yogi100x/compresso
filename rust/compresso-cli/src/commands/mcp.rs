use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, McpRootError};
use compresso_store::Store;

use super::{CliExitCode, McpCommand, write_error};

pub(super) fn execute(command: McpCommand, errors: &mut impl Write) -> CliExitCode {
    let McpCommand::Serve(arguments) = command else {
        // The routing reminder needs no store, config, or runtime and is
        // dispatched before this server entry point.
        let _ = write_error(errors, "invalid_arguments", "not a server command", false);
        return CliExitCode::Failure;
    };
    let root = match parse_root_argument(&arguments.arguments) {
        Ok(root) => root,
        Err(message) => {
            let _ = write_error(errors, "invalid_arguments", message, arguments.output.json);
            return CliExitCode::Failure;
        }
    };
    if arguments.output.json || arguments.dry_run {
        let _ = write_error(
            errors,
            "invalid_arguments",
            "mcp serve does not accept --json or --dry-run",
            arguments.output.json,
        );
        return CliExitCode::Failure;
    }

    let config = match Config::load() {
        Ok(config) => config,
        Err(_) => {
            let _ = write_error(
                errors,
                "config",
                "failed to load the Compresso configuration",
                false,
            );
            return CliExitCode::Failure;
        }
    };
    let store = match Store::open_default() {
        Ok(store) => store,
        Err(_) => {
            let _ = write_error(
                errors,
                "internal",
                "failed to open the local Compresso store",
                false,
            );
            return CliExitCode::Failure;
        }
    };
    let engine = Engine::new(
        config.compression.mode,
        counter_for(TokenProvider::OpenAiO200k),
    );
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = write_error(
                errors,
                "internal",
                "failed to initialize the MCP runtime",
                false,
            );
            return CliExitCode::Failure;
        }
    };

    let deps = McpDeps {
        store,
        engine,
        config,
    };
    let result = match root {
        Some(root) => runtime.block_on(compresso_mcp::serve_stdio_for_root(deps, root)),
        None => runtime.block_on(compresso_mcp::serve_stdio(deps)),
    };
    match result {
        Ok(()) => CliExitCode::Success,
        Err(error) => {
            let (code, message) = error
                .downcast_ref::<McpRootError>()
                .copied()
                .map(|error| (error.code(), error.user_message()))
                .unwrap_or(("internal", "MCP stdio server failed"));
            let _ = write_error(errors, code, message, false);
            CliExitCode::Failure
        }
    }
}

fn parse_root_argument(arguments: &[OsString]) -> Result<Option<PathBuf>, &'static str> {
    match arguments {
        [] => Ok(None),
        [flag, path] if flag == OsStr::new("--root") && !path.is_empty() => {
            Ok(Some(PathBuf::from(path)))
        }
        [flag] if flag == OsStr::new("--root") => Err("--root requires a repository path"),
        [argument] => {
            let Some(argument) = argument.to_str() else {
                return Err("mcp serve accepts only an optional --root PATH argument");
            };
            let Some(root) = argument.strip_prefix("--root=") else {
                return Err("mcp serve accepts only an optional --root PATH argument");
            };
            if root.is_empty() {
                return Err("--root requires a repository path");
            }
            Ok(Some(PathBuf::from(root)))
        }
        _ => Err("mcp serve accepts only an optional --root PATH argument"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::commands::{Cli, RootCommand};

    #[test]
    fn test_parse_root_argument() {
        assert_eq!(parse_root_argument(&[]).unwrap(), None);
        assert_eq!(
            parse_root_argument(&[OsString::from("--root"), OsString::from("/repo")]).unwrap(),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            parse_root_argument(&[OsString::from("--root=/repo")]).unwrap(),
            Some(PathBuf::from("/repo"))
        );
        assert!(parse_root_argument(&[OsString::from("--root")]).is_err());
        assert!(parse_root_argument(&[OsString::from("--other")]).is_err());
    }

    #[test]
    fn test_clap_forwards_mcp_root_argument() {
        let cli = Cli::try_parse_from(["compresso", "mcp", "serve", "--root", "/repo"])
            .expect("mcp root should parse");
        let RootCommand::Mcp {
            command: McpCommand::Serve(arguments),
        } = cli.command
        else {
            panic!("mcp serve should be selected");
        };
        assert_eq!(
            parse_root_argument(&arguments.arguments).unwrap(),
            Some(PathBuf::from("/repo"))
        );
    }

    #[test]
    fn test_root_start_errors_are_actionable_and_content_free() {
        assert_eq!(McpRootError::InvalidRoot.code(), "invalid_root");
        assert!(
            McpRootError::InvalidRoot
                .user_message()
                .contains("existing directory")
        );
        assert_eq!(McpRootError::UnapprovedRoot.code(), "unapproved_root");
        assert!(
            McpRootError::UnapprovedRoot
                .user_message()
                .contains("compresso index ROOT")
        );
        for error in [McpRootError::InvalidRoot, McpRootError::UnapprovedRoot] {
            assert!(!error.user_message().contains("/Users/"));
            assert!(!error.user_message().contains("repo-secret"));
        }
    }
}
