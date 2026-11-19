//! Machine-readable inventory of the public command, tool, and configuration
//! surface, derived from the live clap trees, the MCP tool registry, and the
//! configuration key table rather than from source text.
//!
//! The documentation drift linter (`scripts/doc-drift-lint.ts`) treats this
//! inventory as the source of truth.

use clap::{Command, CommandFactory};
use compresso_core::Config;
use compresso_eval::campaign::cli::CampaignCli;
use compresso_eval::cli::EvalCli;
use serde_json::{Value, json};

use crate::Cli;

pub const SURFACE_SCHEMA_VERSION: u32 = 1;

/// Builds the inventory. Output is deterministic for a given build.
pub fn surface_inventory() -> Value {
    let binaries = [Cli::command(), EvalCli::command(), CampaignCli::command()]
        .into_iter()
        .map(|mut command| {
            // Materializes generated help/version arguments and propagated globals.
            command.build();
            let name = command.get_name().to_owned();
            let mut commands = Vec::new();
            collect(&command, vec![name.clone()], false, &mut commands);
            json!({ "name": name, "commands": commands })
        })
        .collect::<Vec<_>>();

    let mut config_keys = Config::default()
        .explain()
        .into_iter()
        .map(|(key, _, _)| key)
        .collect::<Vec<_>>();
    config_keys.sort();

    let mut mcp_tools = compresso_mcp::tool_names();
    mcp_tools.sort();

    json!({
        "schema_version": SURFACE_SCHEMA_VERSION,
        "binaries": binaries,
        "mcp_tools": mcp_tools,
        "config_keys": config_keys,
    })
}

fn collect(command: &Command, path: Vec<String>, parent_hidden: bool, output: &mut Vec<Value>) {
    let hidden = parent_hidden || command.is_hide_set();
    let mut flags = Vec::new();
    for argument in command.get_arguments() {
        if let Some(long) = argument.get_long() {
            flags.push(format!("--{long}"));
        }
        for alias in argument.get_all_aliases().unwrap_or_default() {
            flags.push(format!("--{alias}"));
        }
        if let Some(short) = argument.get_short() {
            flags.push(format!("-{short}"));
        }
    }
    // `mcp serve` parses its own flags behind a trailing argument list.
    if path
        .iter()
        .map(String::as_str)
        .eq(["compresso", "mcp", "serve"])
    {
        flags.extend(
            crate::commands::mcp::SERVE_FLAGS
                .iter()
                .map(|flag| (*flag).to_owned()),
        );
    }
    flags.sort();
    flags.dedup();

    let children = command
        .get_subcommands()
        .filter(|child| child.get_name() != "help")
        .collect::<Vec<_>>();
    output.push(json!({
        "path": path,
        "hidden": hidden,
        "has_subcommands": !children.is_empty(),
        "flags": flags,
    }));
    for child in children {
        let mut child_path = path.clone();
        child_path.push(child.get_name().to_owned());
        collect(child, child_path, hidden, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command<'a>(inventory: &'a Value, path: &[&str]) -> &'a Value {
        inventory["binaries"]
            .as_array()
            .expect("binaries array")
            .iter()
            .flat_map(|binary| binary["commands"].as_array().expect("commands array"))
            .find(|command| {
                command["path"]
                    .as_array()
                    .expect("path array")
                    .iter()
                    .map(|segment| segment.as_str().expect("path segment"))
                    .eq(path.iter().copied())
            })
            .unwrap_or_else(|| panic!("{path:?} should be in the inventory"))
    }

    fn has_flag(command: &Value, flag: &str) -> bool {
        command["flags"]
            .as_array()
            .expect("flags array")
            .iter()
            .any(|value| value == flag)
    }

    #[test]
    fn test_mcp_serve_lists_its_hand_parsed_flags() {
        let inventory = surface_inventory();
        let serve = command(&inventory, &["compresso", "mcp", "serve"]);
        for flag in [
            "--edits",
            "--recall-source",
            "--results",
            "--root",
            "--tools",
        ] {
            assert!(has_flag(serve, flag), "{flag}");
        }
    }

    #[test]
    fn test_surface_inventory_reflects_live_clap_trees() {
        let inventory = surface_inventory();

        assert_eq!(inventory["schema_version"], SURFACE_SCHEMA_VERSION);
        assert_eq!(
            inventory["binaries"]
                .as_array()
                .expect("binaries array")
                .iter()
                .map(|binary| binary["name"].as_str().expect("binary name"))
                .collect::<Vec<_>>(),
            ["compresso", "compresso-eval", "compresso-eval-campaign"]
        );

        let root = command(&inventory, &["compresso"]);
        assert_eq!(root["has_subcommands"], true);
        assert!(has_flag(root, "--help"));
        assert!(has_flag(root, "--version"));

        let explain = command(&inventory, &["compresso", "config", "explain"]);
        assert_eq!(explain["has_subcommands"], false);
        assert_eq!(explain["hidden"], false);
        assert!(has_flag(explain, "--json"));

        let context = command(&inventory, &["compresso", "context"]);
        assert!(has_flag(context, "--max-tokens"));
        assert!(
            has_flag(context, "--budget"),
            "argument aliases are part of the accepted surface"
        );

        assert!(has_flag(
            command(&inventory, &["compresso-eval", "run"]),
            "--gates"
        ));
        assert!(has_flag(
            command(&inventory, &["compresso-eval-campaign", "run"]),
            "--allow-external"
        ));
        let generated_help = inventory["binaries"]
            .as_array()
            .expect("binaries array")
            .iter()
            .flat_map(|binary| binary["commands"].as_array().expect("commands array"))
            .any(|command| {
                command["path"].as_array().expect("path").last() == Some(&json!("help"))
            });
        assert!(
            !generated_help,
            "clap's generated help subcommand is omitted"
        );
    }

    #[test]
    fn test_surface_inventory_lists_registered_mcp_tools_and_config_keys() {
        let inventory = surface_inventory();
        let names = |key: &str| {
            inventory[key]
                .as_array()
                .expect("array")
                .iter()
                .map(|value| value.as_str().expect("string").to_owned())
                .collect::<Vec<_>>()
        };

        let tools = names("mcp_tools");
        assert_eq!(tools.len(), compresso_mcp::tool_names().len());
        assert!(tools.contains(&"compresso_repo_map".to_owned()));
        assert!(tools.is_sorted());

        let keys = names("config_keys");
        assert!(keys.contains(&"compression.mode".to_owned()));
        assert!(keys.contains(&"privacy.telemetry".to_owned()));
        assert!(keys.is_sorted());
        assert_eq!(keys.len(), Config::default().explain().len());
    }
}
