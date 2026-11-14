//! Command surface of the `compresso-eval-campaign` binary.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};

use super::adapter::{AdapterKind, resolve_adapter};
use super::checkpoint::Checkpoint;
use super::report::CampaignReport;
use super::runner::{CampaignError, CampaignPolicy, CampaignRunner, StartMode};
use crate::cli::CliExitCode;

#[derive(Clone, Debug, Eq, Parser, PartialEq)]
#[command(
    name = "compresso-eval-campaign",
    version,
    about = "Resumable benchmark campaigns over external task suites"
)]
pub struct CampaignCli {
    #[command(subcommand)]
    pub command: CampaignCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum CampaignCommand {
    /// Run a suite to completion, checkpointing every task transition.
    Run(CampaignRunArgs),
    /// Report the progress recorded in a checkpoint without running anything.
    Status(CampaignStatusArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum AdapterArg {
    Fixture,
    SweBenchVerified,
    TerminalBench,
}

impl From<AdapterArg> for AdapterKind {
    fn from(value: AdapterArg) -> Self {
        match value {
            AdapterArg::Fixture => Self::Fixture,
            AdapterArg::SweBenchVerified => Self::SweBenchVerified,
            AdapterArg::TerminalBench => Self::TerminalBench,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CampaignRunArgs {
    /// Suite location understood by the selected adapter.
    #[arg(long, value_name = "SUITE")]
    pub suite: PathBuf,
    /// Durable progress file; created by a fresh run.
    #[arg(long, value_name = "CHECKPOINT")]
    pub checkpoint: PathBuf,
    #[arg(long, value_enum, default_value_t = AdapterArg::Fixture)]
    pub adapter: AdapterArg,
    /// Continue the campaign recorded in an existing checkpoint.
    #[arg(long)]
    pub resume: bool,
    #[arg(long, default_value_t = 1, value_name = "TASKS")]
    pub concurrency: usize,
    #[arg(long, default_value_t = 2, value_name = "ATTEMPTS")]
    pub max_attempts: u32,
    #[arg(long, default_value_t = 600_000, value_name = "MILLISECONDS")]
    pub task_timeout_ms: u64,
    /// Permit adapters that need network or container access.
    #[arg(long)]
    pub allow_external: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CampaignStatusArgs {
    #[arg(long, value_name = "CHECKPOINT")]
    pub checkpoint: PathBuf,
    #[arg(long)]
    pub json: bool,
}

pub fn run(
    arguments: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    match CampaignCli::try_parse_from(arguments) {
        Ok(cli) => execute(cli.command, output, errors),
        Err(error) => {
            let display = matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            );
            let destination: &mut dyn Write = if display { output } else { errors };
            let _ = write!(destination, "{error}");
            if display {
                CliExitCode::Success
            } else {
                CliExitCode::Usage
            }
        }
    }
}

pub fn execute(
    command: CampaignCommand,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = match &command {
        CampaignCommand::Run(arguments) => arguments.json,
        CampaignCommand::Status(arguments) => arguments.json,
    };
    let result = match command {
        CampaignCommand::Run(arguments) => run_campaign(&arguments),
        CampaignCommand::Status(arguments) => Checkpoint::load(&arguments.checkpoint)
            .map(|checkpoint| CampaignReport::from_checkpoint(&checkpoint))
            .map_err(CampaignError::from),
    };
    let written = match &result {
        Ok(report) => write_report(output, report, json),
        Err(error) => write_error(errors, error, json),
    };
    match (result, written) {
        (Ok(report), Ok(())) if report.campaign.complete => CliExitCode::Success,
        _ => CliExitCode::Failure,
    }
}

fn run_campaign(arguments: &CampaignRunArgs) -> Result<CampaignReport, CampaignError> {
    let adapter = resolve_adapter(
        arguments.adapter.into(),
        &arguments.suite,
        arguments.allow_external,
    )?;
    let policy = CampaignPolicy {
        concurrency: arguments.concurrency,
        max_attempts: arguments.max_attempts,
        task_timeout: Duration::from_millis(arguments.task_timeout_ms),
    };
    let start = if arguments.resume {
        StartMode::Resume
    } else {
        StartMode::Fresh
    };
    let outcome = CampaignRunner::new(adapter, policy, &arguments.checkpoint)?.run(start)?;
    Ok(CampaignReport::from_checkpoint(&outcome.checkpoint))
}

fn write_report(
    output: &mut impl Write,
    report: &CampaignReport,
    json: bool,
) -> std::io::Result<()> {
    if json {
        serde_json::to_writer(&mut *output, report)
    } else {
        serde_json::to_writer_pretty(&mut *output, report)
    }
    .map_err(std::io::Error::other)?;
    writeln!(output)
}

fn write_error(output: &mut impl Write, error: &CampaignError, json: bool) -> std::io::Result<()> {
    if json {
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({
                "error": { "code": error.code(), "message": error.to_string() }
            }),
        )
        .map_err(std::io::Error::other)?;
        writeln!(output)
    } else {
        writeln!(output, "error[{}]: {error}", error.code())
    }
}
