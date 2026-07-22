use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use compresso_eval::{EvalRunner, FixtureSet};

fn main() -> Result<(), CliError> {
    run(std::env::args_os().skip(1))
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), CliError> {
    let command = parse_command(arguments)?;
    let set = FixtureSet::load(&command.fixtures).map_err(|error| CliError::Evaluation {
        message: error.to_string(),
    })?;
    let report = EvalRunner.run(&set);

    // E7-02 accepts and preserves this CLI surface. E7-03 owns loading and evaluating the file.
    let _gates = command.gates;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if command.json {
        serde_json::to_writer(&mut output, &report)
    } else {
        serde_json::to_writer_pretty(&mut output, &report)
    }
    .map_err(|error| CliError::Evaluation {
        message: format!("failed to encode evaluation report: {error}"),
    })?;
    writeln!(output).map_err(|error| CliError::Evaluation {
        message: format!("failed to write evaluation report: {error}"),
    })?;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct RunCommand {
    fixtures: PathBuf,
    gates: Option<PathBuf>,
    json: bool,
}

fn parse_command(arguments: impl IntoIterator<Item = OsString>) -> Result<RunCommand, CliError> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err(CliError::Usage("missing command"));
    };
    if command != "run" {
        return Err(CliError::Usage("expected `run` command"));
    }

    let mut fixtures = None;
    let mut gates = None;
    let mut json = false;
    while let Some(argument) = arguments.next() {
        if argument == "--fixtures" {
            let value = arguments
                .next()
                .ok_or(CliError::Usage("--fixtures requires a path"))?;
            set_once(&mut fixtures, PathBuf::from(value), "fixtures path")?;
        } else if argument == "--gates" {
            let value = arguments
                .next()
                .ok_or(CliError::Usage("--gates requires a path"))?;
            set_once(&mut gates, PathBuf::from(value), "gates path")?;
        } else if argument == "--json" {
            if json {
                return Err(CliError::Usage("--json may be supplied only once"));
            }
            json = true;
        } else if argument.to_string_lossy().starts_with('-') {
            return Err(CliError::OwnedUsage(format!(
                "unknown option {}",
                argument.to_string_lossy()
            )));
        } else {
            set_once(&mut fixtures, PathBuf::from(argument), "fixtures path")?;
        }
    }

    let fixtures = fixtures.ok_or(CliError::Usage("missing fixtures path"))?;
    Ok(RunCommand {
        fixtures,
        gates,
        json,
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &'static str) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError::OwnedUsage(format!(
            "{name} may be supplied only once"
        )));
    }
    *slot = Some(value);
    Ok(())
}

enum CliError {
    Usage(&'static str),
    OwnedUsage(String),
    Evaluation { message: String },
}

impl std::fmt::Debug for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) => write!(
                formatter,
                "{message}\nusage: compresso-eval run <fixtures> [--gates <path>] [--json]"
            ),
            Self::OwnedUsage(message) => write!(
                formatter,
                "{message}\nusage: compresso-eval run <fixtures> [--gates <path>] [--json]"
            ),
            Self::Evaluation { message } => formatter.write_str(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn test_parse_documented_run_surface_without_evaluating_gates() {
        assert_eq!(
            parse_command(strings(&[
                "run",
                "fixtures/",
                "--gates",
                "docs/plan/gates.toml",
            ]))
            .expect("documented command should parse"),
            RunCommand {
                fixtures: PathBuf::from("fixtures/"),
                gates: Some(PathBuf::from("docs/plan/gates.toml")),
                json: false,
            }
        );
    }

    #[test]
    fn test_parse_named_fixtures_and_json_surface() {
        assert_eq!(
            parse_command(strings(&["run", "--fixtures", "fixtures/", "--json"]))
                .expect("named fixtures command should parse"),
            RunCommand {
                fixtures: PathBuf::from("fixtures/"),
                gates: None,
                json: true,
            }
        );
    }

    #[test]
    fn test_parse_rejects_missing_gates_value() {
        assert!(matches!(
            parse_command(strings(&["run", "fixtures/", "--gates"])),
            Err(CliError::Usage("--gates requires a path"))
        ));
    }
}
