use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::{Args, Parser, Subcommand, ValueEnum};
use compresso_core::ClosedPipeGuard;
pub use compresso_eval::cli::CliExitCode;
use compresso_eval::cli::EvalCommand;
use compresso_store::Store;

mod decisions;
mod doctor;
mod engine;
mod index;
mod integrations;
mod mcp;
mod mcp_probe;
mod quiet_pipe;
mod recall;
mod repos;
mod retrieval;
mod setup;
mod transcript_stats;

pub use decisions::{
    DecisionsArgs, PostError, PullRequestPoster, SystemPullRequestPoster, execute_decisions,
};
pub use doctor::{ConfigExplainArgs, ConfigModeArg, DiagnosticEnvironment, DoctorArgs};
pub use engine::{CompressArgs, ExplainArgs, StatsArgs};
pub use index::IndexArgs;
pub use integrations::{IntegrationEnvironment, execute_integrations};
pub use recall::{RecallArgs, RecallEnvironment, RecallSourceArg, execute_recall};
pub use repos::{ReposCommand, ReposListArgs, ReposRemoveArgs};
pub use retrieval::{
    ContextArgs, ImpactArgs, MapArgs, OutlineArgs, ReadArgs, SearchArgs, UnfoldArgs,
};
pub use setup::{
    SetupArgs, SetupClientArg, SetupRuntime, SystemSetupRuntime, execute_setup,
    execute_setup_with_runtime,
};

#[derive(Debug, Parser)]
#[command(
    name = "compresso",
    version,
    about = "Local-first context intelligence"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: RootCommand,
}

#[derive(Debug, Subcommand)]
pub enum RootCommand {
    Init(MutationArgs),
    Doctor(DoctorArgs),
    Index(IndexArgs),
    Setup(SetupArgs),
    Map(MapArgs),
    Search(SearchArgs),
    Read(ReadArgs),
    Outline(OutlineArgs),
    Unfold(UnfoldArgs),
    Context(ContextArgs),
    Impact(ImpactArgs),
    Compress(CompressArgs),
    Explain(ExplainArgs),
    Stats(StatsArgs),
    /// Search past local session transcripts without a persistent index.
    Recall(RecallArgs),
    /// Draft a pull request comment from a session's recorded decisions.
    Decisions(DecisionsArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    Integrations {
        #[command(subcommand)]
        command: IntegrationsCommand,
    },
    Repos {
        #[command(subcommand)]
        command: ReposCommand,
    },
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
}

/// Arguments for the constant tool-routing reminder.
#[derive(Args, Debug, Default)]
pub struct RoutingReminderArgs {
    /// Emit the `SessionStart` hook JSON envelope that Claude Code and Codex
    /// add to model context after compaction, instead of plain text.
    #[arg(long)]
    pub session_start_hook: bool,
}

#[derive(Args, Debug, Default)]
pub struct OutputArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Default)]
pub struct MutationArgs {
    #[command(flatten)]
    pub output: OutputArgs,
    #[arg(long)]
    pub dry_run: bool,
}

/// Coding-client selector, including the explicitly refused Claude Desktop target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum IntegrationClientArg {
    #[value(name = "claude-code")]
    ClaudeCode,
    #[value(name = "claude-desktop", hide = true)]
    ClaudeDesktop,
    Codex,
    Cursor,
}

impl IntegrationClientArg {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
        }
    }
}

#[derive(Args, Debug)]
pub struct IntegrationMutationArgs {
    /// Coding client whose MCP entry should be changed.
    #[arg(value_enum)]
    pub client: IntegrationClientArg,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Preview the exact plan without writing config, receipts, or SQLite.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply the displayed plan without an interactive prompt.
    #[arg(long)]
    pub yes: bool,
    /// Use this repository's project-local MCP config instead of user scope.
    #[arg(long, value_name = "DIR")]
    pub project: Option<PathBuf>,
    /// Manage the post-compaction routing-reminder hook instead of the MCP
    /// server entry (Claude Code and Codex only). The hook is installed in
    /// user-level settings only; with `--project`, install and uninstall
    /// remove a hook an earlier version wrote into that repository.
    #[arg(long)]
    pub compaction_hook: bool,
    /// Install the MCP server entry without session recall. By default the
    /// entry enables the `compresso_recall` tool over this client's own local
    /// transcripts (Claude Code and Codex only).
    #[arg(long, conflicts_with = "compaction_hook")]
    pub no_recall: bool,
}

#[derive(Args, Debug)]
pub struct IntegrationReadArgs {
    /// Coding client to inspect.
    #[arg(value_enum)]
    pub client: IntegrationClientArg,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Inspect this repository's project-local MCP config instead of user scope.
    #[arg(long, value_name = "DIR")]
    pub project: Option<PathBuf>,
    /// Inspect the post-compaction routing-reminder hook in user-level
    /// settings instead of the MCP server entry (Claude Code and Codex only;
    /// not combined with `--project`).
    #[arg(long)]
    pub compaction_hook: bool,
}

#[derive(Args, Debug, Default)]
pub struct DeferredArgs {
    #[command(flatten)]
    pub output: OutputArgs,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub arguments: Vec<OsString>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Explain(ConfigExplainArgs),
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    Serve(DeferredArgs),
    /// Print the bounded Compresso tool-routing reminder.
    RoutingReminder(RoutingReminderArgs),
}

#[derive(Debug, Subcommand)]
pub enum IntegrationsCommand {
    Install(IntegrationMutationArgs),
    Status(IntegrationReadArgs),
    Verify(IntegrationReadArgs),
    Uninstall(IntegrationMutationArgs),
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    Login(MutationArgs),
    Status(OutputArgs),
    Logout(MutationArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountLogin {
    Authenticated,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountStatus {
    LocalOnly,
    SignedIn,
}

pub trait AccountClient: Send + Sync {
    fn login(&self) -> Result<AccountLogin, AccountError>;
    fn status(&self) -> Result<AccountStatus, AccountError>;
    fn logout(&self) -> Result<(), AccountError>;
}

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("account services are unavailable in this local-only build")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableAccountClient;

impl AccountClient for UnavailableAccountClient {
    fn login(&self) -> Result<AccountLogin, AccountError> {
        Err(AccountError::Unavailable)
    }

    fn status(&self) -> Result<AccountStatus, AccountError> {
        Ok(AccountStatus::LocalOnly)
    }

    fn logout(&self) -> Result<(), AccountError> {
        Ok(())
    }
}

/// Executes a parsed command line against the system environment.
///
/// Standard output is guarded. When its reader has gone away (`| head`, a
/// consumer that exited early) the command stops quietly like a standard Unix
/// filter: nothing more is written to `errors` and the exit is a success,
/// never a panic. An error that was already reported before the reader left
/// keeps the command's own exit code. Any other output failure is reported as
/// a typed `output_failed` error.
///
/// `compresso mcp serve` owns its transport and still reports a closed client
/// pipe as a typed `output_closed` failure.
pub fn execute(
    cli: Cli,
    account: &dyn AccountClient,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = cli.command.json();
    let reader_gone = AtomicBool::new(false);
    let mut observed = quiet_pipe::ObservedOutput::new(output, &reader_gone);
    let mut guarded = ClosedPipeGuard::new(&mut observed, "standard output");
    let mut errors = quiet_pipe::GatedErrors::new(errors, &reader_gone);
    let code = execute_command(cli, account, &mut guarded, &mut errors);
    match guarded.finish() {
        Ok(()) => code,
        Err(error) if error.is_closed() => {
            if errors.delivered() {
                code
            } else {
                CliExitCode::Success
            }
        }
        Err(error) => {
            let _ = write_error(&mut errors, error.code(), &error.to_string(), json);
            CliExitCode::Failure
        }
    }
}

fn execute_command(
    cli: Cli,
    account: &dyn AccountClient,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    match cli.command {
        RootCommand::Integrations { command } => {
            let environment = match IntegrationEnvironment::system() {
                Ok(environment) => environment,
                Err(error) => {
                    let json = command.json();
                    let _ = write_error(errors, error.code, &error.message, json);
                    return CliExitCode::Failure;
                }
            };
            let stdin = std::io::stdin();
            integrations::execute(command, &environment, &mut stdin.lock(), output, errors)
        }
        RootCommand::Mcp {
            command: McpCommand::RoutingReminder(arguments),
        } => execute_routing_reminder(&arguments, output),
        RootCommand::Mcp { command } => mcp::execute(command, errors),
        RootCommand::Eval { command } => compresso_eval::cli::execute(command, output, errors),
        RootCommand::Account { command } => execute_account(command, account, output, errors),
        RootCommand::Decisions(arguments) => {
            let store = match Store::open_default() {
                Ok(store) => store,
                Err(_) => {
                    let _ = write_error(
                        errors,
                        "internal",
                        "failed to open the local Compresso store",
                        arguments.output.json,
                    );
                    return CliExitCode::Failure;
                }
            };
            execute_decisions(
                arguments,
                &store,
                &SystemPullRequestPoster::from_environment(),
                output,
                errors,
            )
        }
        RootCommand::Recall(arguments) => {
            let Some(environment) = RecallEnvironment::system() else {
                let _ = write_error(
                    errors,
                    "environment",
                    "failed to resolve the home or current working directory",
                    arguments.output.json,
                );
                return CliExitCode::Failure;
            };
            execute_recall(arguments, &environment, output, errors)
        }
        RootCommand::Setup(arguments) => {
            let json = arguments.output.json;
            let environment = match IntegrationEnvironment::system() {
                Ok(environment) => environment,
                Err(error) => {
                    let _ = write_error(errors, error.code, &error.message, json);
                    return CliExitCode::Failure;
                }
            };
            let stdin = std::io::stdin();
            setup::execute_setup(
                arguments,
                None,
                &environment,
                &mut stdin.lock(),
                output,
                errors,
            )
        }
        RootCommand::Index(arguments) => {
            let json = arguments.output.json;
            let cwd = match std::env::current_dir() {
                Ok(cwd) => cwd,
                Err(_) => {
                    let _ = write_error(
                        errors,
                        "internal",
                        "failed to resolve the current working directory",
                        json,
                    );
                    return CliExitCode::Failure;
                }
            };
            if arguments.dry_run {
                execute_index(arguments, None, &cwd, output, errors)
            } else {
                let store = match Store::open_default() {
                    Ok(store) => store,
                    Err(_) => {
                        let _ = write_error(
                            errors,
                            "internal",
                            "failed to open the local Compresso store",
                            json,
                        );
                        return CliExitCode::Failure;
                    }
                };
                execute_index(arguments, Some(&store), &cwd, output, errors)
            }
        }
        RootCommand::Repos { command } => {
            let json = command.json();
            let store = match Store::open_default() {
                Ok(store) => store,
                Err(_) => {
                    let _ = write_error(
                        errors,
                        "internal",
                        "failed to open the local Compresso store",
                        json,
                    );
                    return CliExitCode::Failure;
                }
            };
            let cwd = match std::env::current_dir() {
                Ok(cwd) => cwd,
                Err(_) => {
                    let _ = write_error(
                        errors,
                        "internal",
                        "failed to resolve the current working directory",
                        json,
                    );
                    return CliExitCode::Failure;
                }
            };
            let stdin = std::io::stdin();
            repos::execute(command, &store, &cwd, &mut stdin.lock(), output, errors)
        }
        command if command.is_diagnostic() => {
            let json = command.json();
            let environment = match DiagnosticEnvironment::system() {
                Ok(environment) => environment,
                Err(error) => {
                    let _ = write_error(errors, error.code, &error.message, json);
                    return CliExitCode::Failure;
                }
            };
            execute_diagnostic(command, &environment, output, errors)
        }
        command if command.is_engine() => {
            let json = command.json();
            let store = match Store::open_default() {
                Ok(store) => store,
                Err(_) => {
                    let _ = write_error(
                        errors,
                        "internal",
                        "failed to open the local Compresso store",
                        json,
                    );
                    return CliExitCode::Failure;
                }
            };
            let stdin = std::io::stdin();
            execute_engine(command, &store, &mut stdin.lock(), output, errors)
        }
        command if command.is_retrieval() => {
            let json = command.json();
            let store = match Store::open_default() {
                Ok(store) => store,
                Err(_) => {
                    let _ = write_error(
                        errors,
                        "internal",
                        "failed to open the local Compresso store",
                        json,
                    );
                    return CliExitCode::Failure;
                }
            };
            let cwd = match std::env::current_dir() {
                Ok(cwd) => cwd,
                Err(_) => {
                    let _ = write_error(
                        errors,
                        "internal",
                        "failed to resolve the current working directory",
                        json,
                    );
                    return CliExitCode::Failure;
                }
            };
            execute_retrieval(command, &store, &cwd, output, errors)
        }
        command => {
            let json = command.json();
            let _ = write_error(
                errors,
                "unavailable",
                "command wiring belongs to a later E4/E5/E6 ticket",
                json,
            );
            CliExitCode::Failure
        }
    }
}

/// Executes a command against an injected local store and working directory.
///
/// This is the shared boundary used by the binary and deterministic CLI tests;
/// retrieval behavior remains implemented by Atlas and Store.
pub fn execute_with_store(
    cli: Cli,
    account: &dyn AccountClient,
    store: &Store,
    cwd: &std::path::Path,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    execute_with_store_and_input(
        cli,
        account,
        store,
        cwd,
        &mut std::io::empty(),
        output,
        errors,
    )
}

/// Executes a command with injected local Store, cwd, and compression input.
pub fn execute_with_store_and_input(
    cli: Cli,
    account: &dyn AccountClient,
    store: &Store,
    cwd: &std::path::Path,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    match cli.command {
        RootCommand::Integrations { command } => {
            let json = command.json();
            let _ = write_error(
                errors,
                "environment_required",
                "integration commands require an explicit IntegrationEnvironment at injected boundaries",
                json,
            );
            CliExitCode::Failure
        }
        RootCommand::Setup(arguments) => {
            let _ = write_error(
                errors,
                "environment_required",
                "setup requires an explicit IntegrationEnvironment at injected boundaries",
                arguments.output.json,
            );
            CliExitCode::Failure
        }
        RootCommand::Eval { command } => compresso_eval::cli::execute(command, output, errors),
        RootCommand::Account { command } => execute_account(command, account, output, errors),
        RootCommand::Decisions(arguments) => execute_decisions(
            arguments,
            store,
            &SystemPullRequestPoster::from_environment(),
            output,
            errors,
        ),
        RootCommand::Recall(arguments) => {
            let _ = write_error(
                errors,
                "environment_required",
                "recall requires an explicit RecallEnvironment at injected boundaries",
                arguments.output.json,
            );
            CliExitCode::Failure
        }
        RootCommand::Index(arguments) => execute_index(arguments, Some(store), cwd, output, errors),
        RootCommand::Repos { command } => {
            repos::execute(command, store, cwd, input, output, errors)
        }
        command if command.is_diagnostic() => {
            let json = command.json();
            let environment = match DiagnosticEnvironment::system() {
                Ok(environment) => environment,
                Err(error) => {
                    let _ = write_error(errors, error.code, &error.message, json);
                    return CliExitCode::Failure;
                }
            };
            execute_diagnostic(command, &environment, output, errors)
        }
        command if command.is_engine() => execute_engine(command, store, input, output, errors),
        command if command.is_retrieval() => execute_retrieval(command, store, cwd, output, errors),
        command => {
            let json = command.json();
            let _ = write_error(
                errors,
                "unavailable",
                "command wiring belongs to a later E4/E5/E6 ticket",
                json,
            );
            CliExitCode::Failure
        }
    }
}

/// Executes doctor/config diagnostics against injected runtime paths and environment values.
pub fn execute_diagnostics(
    cli: Cli,
    account: &dyn AccountClient,
    environment: &DiagnosticEnvironment,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    match cli.command {
        RootCommand::Account { command } => execute_account(command, account, output, errors),
        command if command.is_diagnostic() => {
            execute_diagnostic(command, environment, output, errors)
        }
        command => {
            let json = command.json();
            let _ = write_error(
                errors,
                "unavailable",
                "only doctor and config explain support an injected diagnostic environment",
                json,
            );
            CliExitCode::Failure
        }
    }
}

fn execute_diagnostic(
    command: RootCommand,
    environment: &DiagnosticEnvironment,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = command.json();
    match doctor::execute(command, environment) {
        Ok(rendered) => {
            let result = if json {
                serde_json::to_writer(&mut *output, &rendered.json)
                    .map_err(std::io::Error::other)
                    .and_then(|()| writeln!(output))
            } else {
                output.write_all(rendered.human.as_bytes())
            };
            if result.is_ok() && rendered.success {
                CliExitCode::Success
            } else {
                CliExitCode::Failure
            }
        }
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json);
            CliExitCode::Failure
        }
    }
}

fn execute_engine(
    command: RootCommand,
    store: &Store,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = command.json();
    match engine::execute(command, store, input) {
        Ok(rendered) => {
            let result = if json {
                serde_json::to_writer(&mut *output, &rendered.json)
                    .map_err(std::io::Error::other)
                    .and_then(|()| writeln!(output))
            } else {
                output.write_all(rendered.human.as_bytes())
            };
            if result.is_ok() {
                CliExitCode::Success
            } else {
                CliExitCode::Failure
            }
        }
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json);
            CliExitCode::Failure
        }
    }
}

fn execute_retrieval(
    command: RootCommand,
    store: &Store,
    cwd: &std::path::Path,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = command.json();
    match retrieval::execute(command, store, cwd) {
        Ok(rendered) => {
            let result = if json {
                serde_json::to_writer(&mut *output, &rendered.json)
                    .map_err(std::io::Error::other)
                    .and_then(|()| writeln!(output))
            } else {
                writeln!(output, "{}", rendered.human)
            };
            if result.is_ok() {
                CliExitCode::Success
            } else {
                CliExitCode::Failure
            }
        }
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json);
            CliExitCode::Failure
        }
    }
}

fn execute_index(
    arguments: IndexArgs,
    store: Option<&Store>,
    cwd: &std::path::Path,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = arguments.output.json;
    match index::execute(arguments, store, cwd) {
        Ok(rendered) => {
            let result = if json {
                serde_json::to_writer(&mut *output, &rendered.json)
                    .map_err(std::io::Error::other)
                    .and_then(|()| writeln!(output))
            } else {
                output.write_all(rendered.human.as_bytes())
            };
            if result.is_ok() {
                CliExitCode::Success
            } else {
                CliExitCode::Failure
            }
        }
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json);
            CliExitCode::Failure
        }
    }
}

/// Prints the constant routing reminder. It reads no input, opens no store,
/// and touches no network, so a client hook can run it after every compaction.
fn execute_routing_reminder(
    arguments: &RoutingReminderArgs,
    output: &mut impl Write,
) -> CliExitCode {
    let reminder = compresso_mcp::routing_reminder();
    let result = if arguments.session_start_hook {
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": reminder,
                }
            }),
        )
        .map_err(std::io::Error::other)
        .and_then(|()| writeln!(output))
    } else {
        writeln!(output, "{reminder}")
    };
    match result {
        Ok(()) => CliExitCode::Success,
        Err(_) => CliExitCode::Failure,
    }
}

fn execute_account(
    command: AccountCommand,
    account: &dyn AccountClient,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let result = match command {
        AccountCommand::Login(arguments) if arguments.dry_run => {
            write_status(output, "login_preview", arguments.output.json)
        }
        AccountCommand::Login(arguments) => match account.login() {
            Ok(AccountLogin::Authenticated) => {
                write_status(output, "authenticated", arguments.output.json)
            }
            Ok(AccountLogin::Cancelled) => write_status(output, "cancelled", arguments.output.json),
            Err(error) => return account_error(errors, error, arguments.output.json),
        },
        AccountCommand::Status(arguments) => match account.status() {
            Ok(AccountStatus::LocalOnly) => write_status(output, "local_only", arguments.json),
            Ok(AccountStatus::SignedIn) => write_status(output, "signed_in", arguments.json),
            Err(error) => return account_error(errors, error, arguments.json),
        },
        AccountCommand::Logout(arguments) if arguments.dry_run => {
            write_status(output, "logout_preview", arguments.output.json)
        }
        AccountCommand::Logout(arguments) => match account.logout() {
            Ok(()) => write_status(output, "logged_out", arguments.output.json),
            Err(error) => return account_error(errors, error, arguments.output.json),
        },
    };
    match result {
        Ok(()) => CliExitCode::Success,
        Err(_) => CliExitCode::Failure,
    }
}

fn account_error(errors: &mut impl Write, error: AccountError, json: bool) -> CliExitCode {
    let _ = write_error(errors, "account_unavailable", &error.to_string(), json);
    CliExitCode::Failure
}

fn write_status(output: &mut impl Write, status: &str, json: bool) -> std::io::Result<()> {
    if json {
        serde_json::to_writer(&mut *output, &serde_json::json!({ "status": status }))
            .map_err(std::io::Error::other)?;
        writeln!(output)
    } else {
        writeln!(output, "{status}")
    }
}

fn write_error(
    output: &mut impl Write,
    code: &str,
    message: &str,
    json: bool,
) -> std::io::Result<()> {
    if json {
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({ "error": { "code": code, "message": message } }),
        )
        .map_err(std::io::Error::other)?;
        writeln!(output)
    } else {
        writeln!(output, "error[{code}]: {message}")
    }
}

impl RootCommand {
    fn is_diagnostic(&self) -> bool {
        matches!(self, Self::Doctor(_) | Self::Config { .. })
    }

    fn is_engine(&self) -> bool {
        matches!(self, Self::Compress(_) | Self::Explain(_) | Self::Stats(_))
    }

    fn is_retrieval(&self) -> bool {
        matches!(
            self,
            Self::Map(_)
                | Self::Search(_)
                | Self::Read(_)
                | Self::Outline(_)
                | Self::Unfold(_)
                | Self::Context(_)
                | Self::Impact(_)
        )
    }

    fn json(&self) -> bool {
        match self {
            Self::Init(arguments) => arguments.output.json,
            Self::Doctor(arguments) => arguments.output.json,
            Self::Index(arguments) => arguments.output.json,
            Self::Setup(arguments) => arguments.output.json,
            Self::Map(arguments) => arguments.output.json,
            Self::Search(arguments) => arguments.output.json,
            Self::Read(arguments) => arguments.output.json,
            Self::Outline(arguments) => arguments.output.json,
            Self::Unfold(arguments) => arguments.output.json,
            Self::Context(arguments) => arguments.output.json,
            Self::Impact(arguments) => arguments.output.json,
            Self::Compress(arguments) => arguments.output.json,
            Self::Explain(arguments) => arguments.output.json,
            Self::Stats(arguments) => arguments.output.json,
            Self::Recall(arguments) => arguments.output.json,
            Self::Decisions(arguments) => arguments.output.json,
            Self::Config { command } => match command {
                ConfigCommand::Explain(arguments) => arguments.output.json,
            },
            Self::Mcp { command } => match command {
                McpCommand::Serve(arguments) => arguments.output.json,
                McpCommand::RoutingReminder(_) => false,
            },
            Self::Integrations { command } => match command {
                IntegrationsCommand::Install(arguments)
                | IntegrationsCommand::Uninstall(arguments) => arguments.output.json,
                IntegrationsCommand::Status(arguments) | IntegrationsCommand::Verify(arguments) => {
                    arguments.output.json
                }
            },
            Self::Repos { command } => command.json(),
            Self::Account { .. } | Self::Eval { .. } => false,
        }
    }
}

impl IntegrationsCommand {
    fn json(&self) -> bool {
        match self {
            Self::Install(arguments) | Self::Uninstall(arguments) => arguments.output.json,
            Self::Status(arguments) | Self::Verify(arguments) => arguments.output.json,
        }
    }
}
