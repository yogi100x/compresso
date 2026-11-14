use std::sync::atomic::{AtomicUsize, Ordering};

use clap::{CommandFactory, Parser};
use compresso_cli::{
    AccountClient, AccountError, AccountLogin, AccountStatus, Cli, CliExitCode, RootCommand,
    UnavailableAccountClient, execute,
};
use compresso_eval::cli::EvalCli;

#[test]
fn test_canonical_command_snapshot() {
    let root = Cli::command();
    let names = root
        .get_subcommands()
        .map(|command| command.get_name())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        [
            "init",
            "doctor",
            "index",
            "setup",
            "map",
            "search",
            "read",
            "outline",
            "unfold",
            "context",
            "impact",
            "compress",
            "explain",
            "stats",
            "config",
            "mcp",
            "integrations",
            "repos",
            "account",
            "eval",
        ]
    );

    assert_eq!(subcommands(&root, "account"), ["login", "status", "logout"]);
    assert_eq!(
        subcommands(&root, "eval"),
        [
            "run",
            "gates",
            "diff-gates",
            "workflow",
            "calibrate-anthropic",
        ]
    );
    assert_eq!(subcommands(&root, "config"), ["explain"]);
    assert_eq!(subcommands(&root, "mcp"), ["serve", "routing-reminder"]);
    assert_eq!(
        subcommands(&root, "integrations"),
        ["install", "status", "verify", "uninstall"]
    );
    assert_eq!(subcommands(&root, "repos"), ["list", "remove"]);
}

#[test]
fn test_help_json_and_dry_run_surfaces_parse() {
    Cli::try_parse_from(["compresso", "--help"]).expect_err("help should short-circuit");

    let cli = Cli::try_parse_from(["compresso", "init", "--json", "--dry-run"])
        .expect("modifying command flags should parse");
    let RootCommand::Init(arguments) = cli.command else {
        panic!("init should parse");
    };
    assert!(arguments.output.json);
    assert!(arguments.dry_run);

    Cli::try_parse_from(["compresso", "map", "--json"]).expect("automation JSON flag should parse");
}

#[test]
fn test_both_eval_entry_points_share_the_same_parser() {
    let embedded = Cli::try_parse_from([
        "compresso",
        "eval",
        "run",
        "fixtures/",
        "--gates",
        "docs/plan/gates.toml",
        "--json",
    ])
    .expect("embedded eval should parse");
    let standalone = EvalCli::try_parse_from([
        "compresso-eval",
        "run",
        "fixtures/",
        "--gates",
        "docs/plan/gates.toml",
        "--json",
    ])
    .expect("standalone eval should parse");

    let RootCommand::Eval { command } = embedded.command else {
        panic!("eval should parse");
    };
    assert_eq!(command, standalone.command);
}

#[test]
fn test_both_eval_entry_points_share_output_and_exit_codes() {
    let embedded = Cli::try_parse_from(["compresso", "eval", "workflow", "--json"])
        .expect("embedded eval should parse");
    let standalone = EvalCli::try_parse_from(["compresso-eval", "workflow", "--json"])
        .expect("standalone eval should parse");
    let mut embedded_output = Vec::new();
    let mut embedded_errors = Vec::new();
    let mut standalone_output = Vec::new();
    let mut standalone_errors = Vec::new();

    let embedded_exit = execute(
        embedded,
        &UnavailableAccountClient,
        &mut embedded_output,
        &mut embedded_errors,
    );
    let standalone_exit = compresso_eval::cli::execute(
        standalone.command,
        &mut standalone_output,
        &mut standalone_errors,
    );

    assert_eq!(embedded_exit, standalone_exit);
    assert_eq!(embedded_output, standalone_output);
    assert_eq!(embedded_errors, standalone_errors);
    assert_eq!(embedded_exit, CliExitCode::Failure);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&embedded_errors)
            .expect("shared error should be JSON")["error"]["code"],
        "unavailable"
    );
}

#[test]
fn test_cancelled_login_writes_nothing() {
    let account = CancelledAccountClient::default();
    let cli = Cli::try_parse_from(["compresso", "account", "login", "--json"])
        .expect("account login should parse");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let exit = execute(cli, &account, &mut output, &mut errors);

    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(account.writes.load(Ordering::Relaxed), 0);
    assert!(errors.is_empty());
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("cancelled login should emit JSON");
    assert_eq!(json["status"], "cancelled");
}

#[test]
fn test_unavailable_account_stub_is_explicitly_local_only() {
    let status = Cli::try_parse_from(["compresso", "account", "status", "--json"])
        .expect("account status should parse");
    let mut status_output = Vec::new();
    let mut status_errors = Vec::new();
    let status_exit = execute(
        status,
        &UnavailableAccountClient,
        &mut status_output,
        &mut status_errors,
    );

    assert_eq!(status_exit, CliExitCode::Success);
    assert!(status_errors.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&status_output).expect("status should be JSON")
            ["status"],
        "local_only"
    );

    let login = Cli::try_parse_from(["compresso", "account", "login", "--json"])
        .expect("account login should parse");
    let mut login_output = Vec::new();
    let mut login_errors = Vec::new();
    let login_exit = execute(
        login,
        &UnavailableAccountClient,
        &mut login_output,
        &mut login_errors,
    );

    assert_eq!(login_exit, CliExitCode::Failure);
    assert!(login_output.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&login_errors)
            .expect("login error should be JSON")["error"]["code"],
        "account_unavailable"
    );
}

fn subcommands<'a>(root: &'a clap::Command, name: &str) -> Vec<&'a str> {
    root.find_subcommand(name)
        .expect("subcommand should exist")
        .get_subcommands()
        .map(|command| command.get_name())
        .collect()
}

#[derive(Default)]
struct CancelledAccountClient {
    writes: AtomicUsize,
}

impl AccountClient for CancelledAccountClient {
    fn login(&self) -> Result<AccountLogin, AccountError> {
        Ok(AccountLogin::Cancelled)
    }

    fn status(&self) -> Result<AccountStatus, AccountError> {
        Ok(AccountStatus::LocalOnly)
    }

    fn logout(&self) -> Result<(), AccountError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
