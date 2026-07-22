use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, error::ErrorKind};

use crate::{EvalReport, EvalRunError, EvalRunner, FixtureSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliExitCode {
    Success,
    Failure,
    Usage,
}

impl CliExitCode {
    pub const fn value(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Usage => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, Parser, PartialEq)]
#[command(name = "compresso-eval", version, about = "Compresso evaluation tools")]
pub struct EvalCli {
    #[command(subcommand)]
    pub command: EvalCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum EvalCommand {
    Run(EvalRunArgs),
    Gates(EvalGatesArgs),
    DiffGates(EvalDiffGatesArgs),
    Workflow(EvalWorkflowArgs),
    CalibrateAnthropic(EvalCalibrateArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct EvalRunArgs {
    #[arg(value_name = "FIXTURES", required_unless_present = "fixtures_option")]
    pub fixtures: Option<PathBuf>,
    #[arg(
        long = "fixtures",
        value_name = "FIXTURES",
        conflicts_with = "fixtures"
    )]
    pub fixtures_option: Option<PathBuf>,
    /// Retained for E7-02 compatibility. E7-03 owns loading and evaluation.
    #[arg(long, value_name = "GATES")]
    pub gates: Option<PathBuf>,
    #[arg(long)]
    pub json: bool,
}

impl EvalRunArgs {
    fn fixtures(&self) -> &Path {
        self.fixtures
            .as_deref()
            .or(self.fixtures_option.as_deref())
            .expect("clap requires one fixtures path")
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct EvalGatesArgs {
    #[arg(long, default_value = "fixtures", value_name = "FIXTURES")]
    pub fixtures: PathBuf,
    #[arg(long, default_value = "docs/plan/gates.toml", value_name = "GATES")]
    pub gates: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct EvalDiffGatesArgs {
    pub baseline: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct EvalWorkflowArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct EvalCalibrateArgs {
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(
    arguments: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    match EvalCli::try_parse_from(arguments) {
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
    command: EvalCommand,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = command.json();
    let result = match command {
        EvalCommand::Run(arguments) => evaluate(
            arguments.fixtures(),
            arguments.gates.as_deref(),
            arguments.json,
            output,
        ),
        EvalCommand::Gates(_) => Err(CliError::Unavailable {
            feature: "eval gates (E7-03)",
        }),
        EvalCommand::DiffGates(_) => Err(CliError::Unavailable {
            feature: "eval diff-gates",
        }),
        EvalCommand::Workflow(_) => Err(CliError::Unavailable {
            feature: "eval workflow",
        }),
        EvalCommand::CalibrateAnthropic(arguments) => calibrate_anthropic(&arguments, output),
    };
    match result {
        Ok(()) => CliExitCode::Success,
        Err(error) => {
            let _ = write_error(errors, &error, json);
            CliExitCode::Failure
        }
    }
}

impl EvalCommand {
    fn json(&self) -> bool {
        match self {
            Self::Run(arguments) => arguments.json,
            Self::Gates(arguments) => arguments.json,
            Self::DiffGates(arguments) => arguments.json,
            Self::Workflow(arguments) => arguments.json,
            Self::CalibrateAnthropic(arguments) => arguments.json,
        }
    }
}

fn evaluate(
    fixtures: &Path,
    _gates_path: Option<&Path>,
    json: bool,
    output: &mut impl Write,
) -> Result<(), CliError> {
    let set = FixtureSet::load(fixtures).map_err(|error| CliError::Evaluation {
        message: error.to_string(),
    })?;
    let report = EvalRunner.try_run(&set).map_err(CliError::Run)?;
    // E7-02 accepted and retained --gates without evaluating it. E7-03 owns loading the
    // thresholds, rendering gate outcomes, and returning the gate-regression exit code.
    write_output(output, &report, json)
}

fn calibrate_anthropic(
    arguments: &EvalCalibrateArgs,
    output: &mut impl Write,
) -> Result<(), CliError> {
    if arguments.dry_run {
        if arguments.json {
            serde_json::to_writer(
                &mut *output,
                &serde_json::json!({ "status": "dry_run", "writes": 0 }),
            )
            .map_err(|error| CliError::Evaluation {
                message: format!("failed to encode dry-run output: {error}"),
            })?;
            writeln!(output).map_err(|error| CliError::Evaluation {
                message: format!("failed to write dry-run output: {error}"),
            })?;
        } else {
            writeln!(
                output,
                "calibrate-anthropic dry run: no fixture changes written"
            )
            .map_err(|error| CliError::Evaluation {
                message: format!("failed to write dry-run output: {error}"),
            })?;
        }
        return Ok(());
    }
    if std::env::var_os("ANTHROPIC_API_KEY").is_none() {
        return Err(CliError::MissingApiKey);
    }
    if arguments.model.is_none() {
        return Err(CliError::InvalidArguments {
            message: "--model is required when calibration is executed".to_owned(),
        });
    }
    Err(CliError::Unavailable {
        feature: "eval calibrate-anthropic (E2-02a)",
    })
}

fn write_output(output: &mut impl Write, report: &EvalReport, json: bool) -> Result<(), CliError> {
    if json {
        serde_json::to_writer(&mut *output, report)
    } else {
        serde_json::to_writer_pretty(&mut *output, report)
    }
    .map_err(|error| CliError::Evaluation {
        message: format!("failed to encode evaluation report: {error}"),
    })?;
    writeln!(output).map_err(|error| CliError::Evaluation {
        message: format!("failed to write evaluation report: {error}"),
    })?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{message}")]
    Evaluation { message: String },
    #[error(transparent)]
    Run(EvalRunError),
    #[error("ANTHROPIC_API_KEY is required for calibration")]
    MissingApiKey,
    #[error("{message}")]
    InvalidArguments { message: String },
    #[error("{feature} is not available in this build")]
    Unavailable { feature: &'static str },
}

impl CliError {
    fn code(&self) -> &'static str {
        match self {
            Self::Evaluation { .. } => "evaluation_failed",
            Self::Run(error) => error.code(),
            Self::MissingApiKey => "missing_api_key",
            Self::InvalidArguments { .. } => "invalid_arguments",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

fn write_error(output: &mut impl Write, error: &CliError, json: bool) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_documented_run_surfaces_and_retains_gates_path() {
        let positional = EvalCli::try_parse_from([
            "compresso-eval",
            "run",
            "fixtures/",
            "--gates",
            "docs/plan/gates.toml",
        ])
        .expect("documented command should parse");
        let named =
            EvalCli::try_parse_from(["compresso-eval", "run", "--fixtures", "fixtures/", "--json"])
                .expect("named fixtures command should parse");

        assert!(matches!(
            positional.command,
            EvalCommand::Run(arguments)
                if arguments.gates == Some(PathBuf::from("docs/plan/gates.toml"))
        ));
        assert!(matches!(named.command, EvalCommand::Run(arguments) if arguments.json));
    }

    #[test]
    fn test_parse_rejects_missing_gates_value() {
        let error = EvalCli::try_parse_from(["compresso-eval", "run", "fixtures/", "--gates"])
            .expect_err("missing gate path should fail");

        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn test_broken_probe_has_stable_code() {
        let error = CliError::Run(EvalRunError::BrokenProbe {
            fixture: "broken".to_owned(),
            probe_index: 2,
        });

        assert_eq!(error.code(), "broken_probe");
    }
}
