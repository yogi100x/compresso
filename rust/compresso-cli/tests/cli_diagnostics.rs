use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, DiagnosticEnvironment, UnavailableAccountClient, execute_diagnostics,
};

fn environment(directory: &tempfile::TempDir) -> DiagnosticEnvironment {
    let data_dir = directory.path().join("data");
    DiagnosticEnvironment {
        config_path: directory.path().join("config.toml"),
        database_path: data_dir.join("compresso.db"),
        data_dir,
        executable_path: std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_PKG_NAME"))),
        environment: BTreeMap::new(),
        home_dir: None,
        mcp_probe_timeout: None,
    }
}

fn run(arguments: &[&str], environment: &DiagnosticEnvironment) -> (CliExitCode, String, String) {
    let cli = Cli::try_parse_from(arguments).expect("arguments should parse");
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = execute_diagnostics(
        cli,
        &UnavailableAccountClient,
        environment,
        &mut output,
        &mut errors,
    );
    (
        exit,
        String::from_utf8(output).expect("stdout should be UTF-8"),
        String::from_utf8(errors).expect("stderr should be UTF-8"),
    )
}

#[test]
fn test_doctor_reports_all_checks_green_for_healthy_runtime() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let environment = environment(&directory);

    let (exit, output, errors) = run(&["compresso", "doctor", "--json"], &environment);

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let payload: serde_json::Value =
        serde_json::from_str(&output).expect("doctor output should be JSON");
    assert_eq!(payload["schema_version"], 1);
    let checks = payload["data"]["checks"]
        .as_array()
        .expect("checks should be an array");
    assert_eq!(checks.len(), 7);
    assert!(checks.iter().all(|check| check["passed"] == true));
    assert!(checks.iter().all(|check| check["severity"] == "ok"));
}

#[test]
fn test_doctor_bad_config_fails_gracefully_with_path_hint() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let environment = environment(&directory);
    fs::write(&environment.config_path, "mode = [")
        .expect("malformed configuration should be written");

    let (exit, output, errors) = run(&["compresso", "doctor", "--json"], &environment);

    assert_eq!(exit, CliExitCode::Failure, "{errors}");
    let payload: serde_json::Value =
        serde_json::from_str(&output).expect("doctor output should remain JSON");
    let check = payload["data"]["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|check| check["name"] == "config_parses"))
        .expect("config diagnostic should exist");
    assert_eq!(check["passed"], false);
    assert_eq!(check["severity"], "error");
    assert!(
        check["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains(environment.config_path.to_string_lossy().as_ref()))
    );
}

#[test]
fn test_config_explain_shows_environment_source_and_forced_telemetry() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut environment = environment(&directory);
    fs::write(&environment.config_path, "[compression]\nmode = \"safe\"\n")
        .expect("configuration should be written");
    environment
        .environment
        .insert("COMPRESSO_MODE".to_owned(), "passthrough".to_owned());
    environment
        .environment
        .insert("COMPRESSO_TELEMETRY".to_owned(), "true".to_owned());

    let (exit, output, errors) = run(&["compresso", "config", "explain", "--json"], &environment);

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let payload: serde_json::Value =
        serde_json::from_str(&output).expect("config output should be JSON");
    let values = payload["data"]["values"]
        .as_array()
        .expect("values should be an array");
    let mode = values
        .iter()
        .find(|value| value["key"] == "compression.mode")
        .expect("mode should be present");
    assert_eq!(mode["value"], "passthrough");
    assert_eq!(mode["source"], "env");
    let telemetry = values
        .iter()
        .find(|value| value["key"] == "privacy.telemetry")
        .expect("telemetry should be present");
    assert_eq!(telemetry["value"], "false");
    assert_eq!(telemetry["source"], "env");
    assert_eq!(telemetry["annotation"], "forced off (D6)");
}

#[test]
fn test_config_explain_shows_flag_over_environment() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut environment = environment(&directory);
    environment
        .environment
        .insert("COMPRESSO_MODE".to_owned(), "safe".to_owned());

    let (exit, output, errors) = run(
        &[
            "compresso",
            "config",
            "explain",
            "--mode",
            "balanced",
            "--json",
        ],
        &environment,
    );

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let payload: serde_json::Value =
        serde_json::from_str(&output).expect("config output should be JSON");
    let mode = payload["data"]["values"]
        .as_array()
        .and_then(|values| {
            values
                .iter()
                .find(|value| value["key"] == "compression.mode")
        })
        .expect("mode should be present");
    assert_eq!(mode["value"], "balanced");
    assert_eq!(mode["source"], "flag");
}
