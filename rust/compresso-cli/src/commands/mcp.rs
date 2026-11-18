use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;

use compresso_core::{Config, Engine, StreamError, TokenProvider, counter_for};
use compresso_mcp::{
    McpDeps, McpInputError, McpRecallOptions, McpResultFormat, McpRootError, McpServeOptions,
    McpToolProfile,
};
use compresso_recall::{RecallSource, SourceSupport};
use compresso_store::Store;

use super::recall::system_home_dir;
use super::{CliExitCode, McpCommand, write_error};

const USAGE: &str = "mcp serve accepts only optional --root PATH, --recall-source SOURCE, --tools full|lean, and --results json|text arguments";

/// Operator-supplied `mcp serve` options.
#[derive(Debug, Default, Eq, PartialEq)]
struct ServeOptions {
    root: Option<PathBuf>,
    /// Session sources the operator explicitly enabled for `compresso_recall`.
    recall_sources: Vec<RecallSource>,
    /// Advertised tool profile; `full` unless the operator opts into `lean`.
    tools: McpToolProfile,
    /// Result form; `None` takes the profile's default (JSON for `full`,
    /// text for `lean`).
    results: Option<McpResultFormat>,
}

pub(super) fn execute(command: McpCommand, errors: &mut impl Write) -> CliExitCode {
    let McpCommand::Serve(arguments) = command else {
        // The routing reminder needs no store, config, or runtime and is
        // dispatched before this server entry point.
        let _ = write_error(errors, "invalid_arguments", "not a server command", false);
        return CliExitCode::Failure;
    };
    let options = match parse_serve_arguments(&arguments.arguments) {
        Ok(options) => options,
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
    let ServeOptions {
        root,
        recall_sources,
        tools,
        results,
    } = options;
    let recall = if recall_sources.is_empty() {
        None
    } else {
        let Some(home_dir) = system_home_dir() else {
            let _ = write_error(
                errors,
                "environment",
                "failed to resolve the user home directory for --recall-source",
                false,
            );
            return CliExitCode::Failure;
        };
        Some(McpRecallOptions {
            home_dir,
            sources: recall_sources,
        })
    };
    let result = runtime.block_on(compresso_mcp::serve_stdio_with_options(
        deps,
        McpServeOptions {
            root,
            recall,
            tools,
            results,
        },
    ));
    match result {
        Ok(()) => CliExitCode::Success,
        Err(error) => {
            if let Some(stream) = error.downcast_ref::<StreamError>() {
                let _ = write_error(errors, stream.code(), &stream.to_string(), false);
                return CliExitCode::Failure;
            }
            let (code, message) = error
                .downcast_ref::<McpRootError>()
                .copied()
                .map(|error| (error.code(), error.user_message()))
                .or_else(|| {
                    error
                        .downcast_ref::<McpInputError>()
                        .copied()
                        .map(|error| (error.code(), error.user_message()))
                })
                .unwrap_or(("internal", "MCP stdio server failed"));
            let _ = write_error(errors, code, message, false);
            CliExitCode::Failure
        }
    }
}

fn parse_serve_arguments(arguments: &[OsString]) -> Result<ServeOptions, &'static str> {
    let mut options = ServeOptions::default();
    let mut tools_seen = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let (flag, inline) = match argument.to_str().and_then(|text| text.split_once('=')) {
            Some((flag, value)) => (OsStr::new(flag), Some(OsString::from(value))),
            None => (argument.as_os_str(), None),
        };
        if flag == OsStr::new("--root") {
            let path = inline
                .or_else(|| arguments.next().cloned())
                .filter(|path| !path.is_empty())
                .ok_or("--root requires a repository path")?;
            if options.root.replace(PathBuf::from(path)).is_some() {
                return Err("--root may be given only once");
            }
        } else if flag == OsStr::new("--recall-source") {
            let name = inline
                .or_else(|| arguments.next().cloned())
                .ok_or("--recall-source requires claude-code or codex")?;
            let source = name
                .to_str()
                .and_then(RecallSource::parse)
                .ok_or("--recall-source requires claude-code or codex")?;
            if source.support() != SourceSupport::Supported {
                return Err(
                    "--recall-source supports only claude-code and codex; other transcript formats are unverified",
                );
            }
            if !options.recall_sources.contains(&source) {
                options.recall_sources.push(source);
            }
        } else if flag == OsStr::new("--tools") {
            if tools_seen {
                return Err("--tools may be given only once");
            }
            tools_seen = true;
            options.tools = inline
                .or_else(|| arguments.next().cloned())
                .as_deref()
                .and_then(OsStr::to_str)
                .and_then(McpToolProfile::parse)
                .ok_or("--tools requires full or lean")?;
        } else if flag == OsStr::new("--results") {
            if options.results.is_some() {
                return Err("--results may be given only once");
            }
            options.results = Some(
                inline
                    .or_else(|| arguments.next().cloned())
                    .as_deref()
                    .and_then(OsStr::to_str)
                    .and_then(McpResultFormat::parse)
                    .ok_or("--results requires json or text")?,
            );
        } else {
            return Err(USAGE);
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::commands::{Cli, RootCommand};

    fn parse(arguments: &[&str]) -> Result<ServeOptions, &'static str> {
        parse_serve_arguments(&arguments.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn test_parse_root_argument() {
        assert_eq!(parse(&[]).unwrap(), ServeOptions::default());
        assert_eq!(
            parse(&["--root", "/repo"]).unwrap().root,
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            parse(&["--root=/repo"]).unwrap().root,
            Some(PathBuf::from("/repo"))
        );
        assert!(parse(&["--root"]).is_err());
        assert!(parse(&["--root="]).is_err());
        assert!(parse(&["--root", "/a", "--root", "/b"]).is_err());
        assert!(parse(&["--other"]).is_err());
        assert!(parse(&["/repo"]).is_err());
    }

    #[test]
    fn test_recall_sources_are_disabled_by_default_and_enabled_only_explicitly() {
        assert!(
            parse(&["--root", "/repo"])
                .unwrap()
                .recall_sources
                .is_empty()
        );
        assert_eq!(
            parse(&[
                "--recall-source",
                "codex",
                "--root=/repo",
                "--recall-source=claude-code",
                "--recall-source",
                "codex",
            ])
            .unwrap(),
            ServeOptions {
                root: Some(PathBuf::from("/repo")),
                recall_sources: vec![RecallSource::Codex, RecallSource::ClaudeCode],
                tools: McpToolProfile::Full,
                results: None,
            }
        );
        assert!(parse(&["--recall-source"]).is_err());
        assert!(parse(&["--recall-source", "claude-desktop"]).is_err());
        assert!(
            parse(&["--recall-source", "cursor"])
                .unwrap_err()
                .contains("unverified")
        );
    }

    #[test]
    fn test_tool_profile_is_full_by_default_and_lean_only_explicitly() {
        assert_eq!(parse(&[]).unwrap().tools, McpToolProfile::Full);
        assert_eq!(
            parse(&["--tools", "lean"]).unwrap().tools,
            McpToolProfile::Lean
        );
        assert_eq!(
            parse(&["--root=/repo", "--tools=full"]).unwrap().tools,
            McpToolProfile::Full
        );
        let combined = parse(&["--tools", "lean", "--recall-source", "claude-code"]).unwrap();
        assert_eq!(combined.tools, McpToolProfile::Lean);
        assert_eq!(combined.recall_sources, vec![RecallSource::ClaudeCode]);
        assert!(parse(&["--tools"]).is_err());
        assert!(parse(&["--tools", "minimal"]).is_err());
        assert!(parse(&["--tools", "lean", "--tools", "lean"]).is_err());
    }

    #[test]
    fn test_result_form_follows_the_profile_unless_chosen_explicitly() {
        assert_eq!(parse(&[]).unwrap().results, None);
        assert_eq!(
            parse(&["--tools", "lean", "--results", "json"])
                .unwrap()
                .results,
            Some(McpResultFormat::Json)
        );
        assert_eq!(
            parse(&["--results=text"]).unwrap().results,
            Some(McpResultFormat::Text)
        );
        assert!(parse(&["--results"]).is_err());
        assert!(parse(&["--results", "yaml"]).is_err());
        assert!(parse(&["--results", "json", "--results", "json"]).is_err());
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
            parse_serve_arguments(&arguments.arguments).unwrap().root,
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
