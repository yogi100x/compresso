use std::ffi::OsString;
use std::io::Write;

use clap::{Args, Parser, Subcommand};
pub use compresso_eval::cli::CliExitCode;
use compresso_eval::cli::EvalCommand;

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
    Doctor(DeferredArgs),
    Index(DeferredArgs),
    Map(DeferredArgs),
    Search(DeferredArgs),
    Read(DeferredArgs),
    Outline(DeferredArgs),
    Unfold(DeferredArgs),
    Context(DeferredArgs),
    Impact(DeferredArgs),
    Compress(DeferredArgs),
    Explain(DeferredArgs),
    Stats(DeferredArgs),
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
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
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
    Explain(DeferredArgs),
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    Serve(DeferredArgs),
}

#[derive(Debug, Subcommand)]
pub enum IntegrationsCommand {
    Install(MutationArgs),
    Status(DeferredArgs),
    Verify(DeferredArgs),
    Uninstall(MutationArgs),
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

pub fn execute(
    cli: Cli,
    account: &dyn AccountClient,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    match cli.command {
        RootCommand::Eval { command } => compresso_eval::cli::execute(command, output, errors),
        RootCommand::Account { command } => execute_account(command, account, output, errors),
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
    fn json(&self) -> bool {
        match self {
            Self::Init(arguments) => arguments.output.json,
            Self::Doctor(arguments)
            | Self::Index(arguments)
            | Self::Map(arguments)
            | Self::Search(arguments)
            | Self::Read(arguments)
            | Self::Outline(arguments)
            | Self::Unfold(arguments)
            | Self::Context(arguments)
            | Self::Impact(arguments)
            | Self::Compress(arguments)
            | Self::Explain(arguments)
            | Self::Stats(arguments) => arguments.output.json,
            Self::Config { command } => match command {
                ConfigCommand::Explain(arguments) => arguments.output.json,
            },
            Self::Mcp { command } => match command {
                McpCommand::Serve(arguments) => arguments.output.json,
            },
            Self::Integrations { command } => match command {
                IntegrationsCommand::Install(arguments)
                | IntegrationsCommand::Uninstall(arguments) => arguments.output.json,
                IntegrationsCommand::Status(arguments) | IntegrationsCommand::Verify(arguments) => {
                    arguments.output.json
                }
            },
            Self::Account { .. } | Self::Eval { .. } => false,
        }
    }
}
