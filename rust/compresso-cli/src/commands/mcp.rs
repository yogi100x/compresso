use std::io::Write;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::McpDeps;
use compresso_store::Store;

use super::{CliExitCode, McpCommand, write_error};

pub(super) fn execute(command: McpCommand, errors: &mut impl Write) -> CliExitCode {
    let McpCommand::Serve(arguments) = command;
    if arguments.output.json || arguments.dry_run || !arguments.arguments.is_empty() {
        let _ = write_error(
            errors,
            "invalid_arguments",
            "mcp serve does not accept --json, --dry-run, or trailing arguments",
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

    match runtime.block_on(compresso_mcp::serve_stdio(McpDeps {
        store,
        engine,
        config,
    })) {
        Ok(()) => CliExitCode::Success,
        Err(_) => {
            let _ = write_error(errors, "internal", "MCP stdio server failed", false);
            CliExitCode::Failure
        }
    }
}
