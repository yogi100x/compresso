use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, error::ErrorKind};
use compresso_core::ClosedPipeGuard;
use serde::Serialize;

use crate::{EvalReport, EvalRunError, EvalRunner, FixtureSet, GateReport, Gates};

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
    /// Evaluate the report against the release thresholds at this path.
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

/// Parses and executes an evaluation command line.
///
/// Standard output is guarded: a reader that closed the pipe early yields a
/// typed `output_closed` error on `errors` and a failure exit, never a panic.
pub fn run(
    arguments: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let mut guarded = ClosedPipeGuard::new(output, "standard output");
    let code = run_command(arguments, &mut guarded, errors);
    match guarded.finish() {
        Ok(()) => code,
        Err(error) => {
            let _ = writeln!(errors, "error[{}]: {error}", error.code());
            CliExitCode::Failure
        }
    }
}

fn run_command(
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
            errors,
        ),
        EvalCommand::Gates(arguments) => evaluate(
            &arguments.fixtures,
            Some(&arguments.gates),
            arguments.json,
            output,
            errors,
        ),
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
    gates_path: Option<&Path>,
    json: bool,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> Result<(), CliError> {
    let set = FixtureSet::load(fixtures).map_err(|error| CliError::Evaluation {
        message: error.to_string(),
    })?;
    let report = EvalRunner.try_run(&set).map_err(CliError::Run)?;
    let Some(gates_path) = gates_path else {
        return write_output(output, &report, json);
    };
    let gates = Gates::load(gates_path).map_err(|error| CliError::GateConfiguration {
        message: error.to_string(),
    })?;
    let evaluated = gates.evaluate(&report);
    write_gated_output(output, errors, &report, &evaluated, json)?;
    ensure_gates_pass(&evaluated)
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

#[derive(Serialize)]
struct GatedEvalOutput<'a> {
    #[serde(flatten)]
    report: &'a EvalReport,
    gates: &'a GateReport,
}

fn write_gated_output(
    output: &mut impl Write,
    errors: &mut impl Write,
    report: &EvalReport,
    gates: &GateReport,
    json: bool,
) -> Result<(), CliError> {
    if !json {
        write_output(output, report, false)?;
        for line in gates.lines() {
            writeln!(errors, "{line}").map_err(|error| CliError::Evaluation {
                message: format!("failed to write gate report: {error}"),
            })?;
        }
        return Ok(());
    }

    serde_json::to_writer(&mut *output, &GatedEvalOutput { report, gates }).map_err(|error| {
        CliError::Evaluation {
            message: format!("failed to encode gated evaluation report: {error}"),
        }
    })?;
    writeln!(output).map_err(|error| CliError::Evaluation {
        message: format!("failed to write gated evaluation report: {error}"),
    })?;
    Ok(())
}

fn ensure_gates_pass(gates: &GateReport) -> Result<(), CliError> {
    if gates.passed {
        return Ok(());
    }
    Err(CliError::GateRegression {
        names: gates.failed_names(),
    })
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{message}")]
    Evaluation { message: String },
    #[error(transparent)]
    Run(EvalRunError),
    #[error("{message}")]
    GateConfiguration { message: String },
    #[error("release gates failed: {names}")]
    GateRegression { names: String },
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
            Self::GateConfiguration { .. } => "gate_configuration",
            Self::GateRegression { .. } => "gate_regression",
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
    use crate::{AggregateMetrics, FixtureRecallOutcome, RecallProbeOutcome, RecallReport};

    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_closed_output_pipe_is_reported_as_typed_failure() {
        let mut errors = Vec::new();

        let code = run(
            ["compresso-eval", "--help"].map(OsString::from),
            &mut ClosedPipe,
            &mut errors,
        );

        assert_eq!(code, CliExitCode::Failure);
        assert_eq!(
            String::from_utf8(errors).expect("UTF-8 diagnostics"),
            "error[output_closed]: standard output was closed by its reader before the write completed\n"
        );
    }

    fn gates_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plan/gates.toml")
    }

    fn passing_report() -> EvalReport {
        EvalReport {
            schema_version: 1,
            fixtures: Vec::new(),
            aggregate: AggregateMetrics {
                compression_fixtures: 1,
                eligible_fixtures: 5,
                median_reduction_eligible: 0.50,
                p95_latency_ms: 100,
                protected_mutations: 0,
                malformed_json: 0,
                fallback_rate: 0.0,
            },
            recall: RecallReport {
                fixtures: vec![FixtureRecallOutcome {
                    fixture: "synthetic".to_owned(),
                    probes: vec![RecallProbeOutcome {
                        fixture: "synthetic".to_owned(),
                        probe_index: 0,
                        protected: true,
                        passed: true,
                    }],
                }],
                protected_probe_recall: 1.0,
                overall_probe_recall: 1.0,
            },
            retrieval: None,
        }
    }

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

    #[test]
    fn test_gated_json_output_preserves_report_and_adds_gate_structure() {
        let report = passing_report();
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report);
        let mut output = Vec::new();
        let mut errors = Vec::new();

        write_gated_output(&mut output, &mut errors, &report, &evaluated, true)
            .expect("gated JSON should render");
        let json: serde_json::Value =
            serde_json::from_slice(&output).expect("gated output should be JSON");

        assert_eq!(json["schema_version"], 1);
        assert!(json["fixtures"].is_array());
        assert!(json["aggregate"].is_object());
        assert!(json["recall"].is_object());
        assert_eq!(json["gates"]["passed"], true);
        assert_eq!(json["gates"]["gates"].as_array().map(Vec::len), Some(7));
        assert!(errors.is_empty());
    }

    #[test]
    fn test_human_gate_output_has_one_named_line_per_gate() {
        let report = passing_report();
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report);
        let mut output = Vec::new();
        let mut errors = Vec::new();

        write_gated_output(&mut output, &mut errors, &report, &evaluated, false)
            .expect("human gates should render");
        let _: EvalReport =
            serde_json::from_slice(&output).expect("human report should remain valid JSON");
        let lines = String::from_utf8(errors).expect("gate lines should be UTF-8");
        let lines = lines.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 7);
        for name in [
            "median_reduction_eligible",
            "protected_mutations",
            "malformed_json",
            "p95_latency_ms",
            "recall_probe_count",
            "protected_probe_recall",
            "overall_probe_recall",
        ] {
            assert_eq!(lines.iter().filter(|line| line.contains(name)).count(), 1);
        }
    }

    #[test]
    fn test_gate_regression_has_distinct_named_code() {
        let mut report = passing_report();
        report.aggregate.median_reduction_eligible = 0.10;
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report);

        let error = ensure_gates_pass(&evaluated).expect_err("regression should fail");

        assert_eq!(error.code(), "gate_regression");
        assert!(error.to_string().contains("median_reduction_eligible"));
        assert!(!error.to_string().contains("protected_mutations"));
    }

    #[test]
    fn test_fixture_regression_error_names_failed_fixture() {
        let evaluated = GateReport {
            passed: false,
            gates: Vec::new(),
            failed_fixtures: vec!["fixture-regression".to_owned()],
        };

        let error = ensure_gates_pass(&evaluated).expect_err("fixture regression should fail");

        assert_eq!(error.code(), "gate_regression");
        assert!(error.to_string().contains("fixture:fixture-regression"));
    }
}
