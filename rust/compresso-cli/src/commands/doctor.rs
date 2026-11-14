use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Args, ValueEnum};
use compresso_atlas::parsers::{Language, ParserRegistry};
use compresso_core::{
    Config, ConfigOverrides, ConfigSource, Mode, RuntimePaths, TokenProvider, counter_for,
};
use compresso_integrations::{
    ConfiguredServer, FileReceiptStore, ReceiptStore as _, read_configured_server,
    receipt_authenticates_server,
};
use compresso_store::Store;

use super::mcp_probe::{self, LauncherTrust, McpServerHealth, Severity};
use super::{ConfigCommand, OutputArgs, RootCommand};

const CONFIG_ENVIRONMENT: [&str; 5] = [
    "COMPRESSO_MODE",
    "COMPRESSO_TARGET_TOKENS",
    "COMPRESSO_TELEMETRY",
    "COMPRESSO_BYPASS",
    "COMPRESSO_LOG",
];

#[derive(Args, Debug, Default)]
pub struct DoctorArgs {
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ConfigModeArg {
    Safe,
    Balanced,
    Aggressive,
    Passthrough,
}

impl From<ConfigModeArg> for Mode {
    fn from(value: ConfigModeArg) -> Self {
        match value {
            ConfigModeArg::Safe => Self::Safe,
            ConfigModeArg::Balanced => Self::Balanced,
            ConfigModeArg::Aggressive => Self::Aggressive,
            ConfigModeArg::Passthrough => Self::Passthrough,
        }
    }
}

#[derive(Args, Debug, Default)]
pub struct ConfigExplainArgs {
    #[arg(long, value_enum)]
    pub mode: Option<ConfigModeArg>,
    #[arg(long)]
    pub target_tokens: Option<usize>,
    #[arg(long)]
    pub telemetry: Option<bool>,
    #[arg(long)]
    pub bypass: Option<bool>,
    #[arg(long)]
    pub log: Option<String>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticEnvironment {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub executable_path: PathBuf,
    pub environment: BTreeMap<String, String>,
    /// Home directory whose user-scope client configs name the MCP server to
    /// check. `None` skips the MCP server check.
    pub home_dir: Option<PathBuf>,
    /// Deadline for the live MCP handshake. `None` keeps the check
    /// process-free: launcher paths are validated but nothing is started.
    pub mcp_probe_timeout: Option<Duration>,
}

impl DiagnosticEnvironment {
    pub(crate) fn system() -> Result<Self, DiagnosticError> {
        let paths = RuntimePaths::system().map_err(|error| DiagnosticError {
            code: "invalid_config",
            message: error.to_string(),
        })?;
        let executable_path = std::env::current_exe().map_err(|error| DiagnosticError {
            code: "internal",
            message: format!("failed to resolve the running binary: {error}"),
        })?;
        let environment = CONFIG_ENVIRONMENT
            .into_iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|value| (name.to_owned(), value))
            })
            .collect();
        Ok(Self {
            config_path: paths.config_file,
            data_dir: paths.data_dir,
            database_path: paths.database_file,
            executable_path,
            environment,
            home_dir: super::integrations::system_home_dir().ok(),
            mcp_probe_timeout: Some(mcp_probe::DEFAULT_PROBE_TIMEOUT),
        })
    }
}

#[derive(Debug)]
pub(super) struct Rendered {
    pub json: serde_json::Value,
    pub human: String,
    pub success: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct DiagnosticError {
    pub code: &'static str,
    pub message: String,
}

pub(super) fn execute(
    command: RootCommand,
    environment: &DiagnosticEnvironment,
) -> Result<Rendered, DiagnosticError> {
    match command {
        RootCommand::Doctor(_) => Ok(run_doctor(environment)),
        RootCommand::Config {
            command: ConfigCommand::Explain(arguments),
        } => explain_config(arguments, environment),
        _ => Err(DiagnosticError {
            code: "unavailable",
            message: "command is not a diagnostic command".to_owned(),
        }),
    }
}

fn explain_config(
    arguments: ConfigExplainArgs,
    environment: &DiagnosticEnvironment,
) -> Result<Rendered, DiagnosticError> {
    let config = Config::load_from(
        &environment.config_path,
        &environment.environment,
        ConfigOverrides {
            mode: arguments.mode.map(Into::into),
            target_tokens: arguments.target_tokens,
            telemetry: arguments.telemetry,
            bypass: arguments.bypass,
            log: arguments.log,
        },
    )
    .map_err(|error| DiagnosticError {
        code: "invalid_config",
        message: error.to_string(),
    })?;
    let rows = config.explanation_rows();
    let values = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "key": row.key,
                "value": row.value,
                "source": row.source,
                "annotation": row.annotation,
            })
        })
        .collect::<Vec<_>>();
    let human = rows
        .iter()
        .map(|row| {
            let annotation = row
                .annotation
                .as_deref()
                .map_or(String::new(), |value| format!("; {value}"));
            format!(
                "{} = {} ({}){}",
                row.key,
                row.value,
                source_name(row.source),
                annotation
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Rendered {
        json: serde_json::json!({
            "schema_version": 1,
            "data": { "values": values },
        }),
        human: format!("{human}\n"),
        success: true,
    })
}

#[derive(Debug)]
struct DiagnosticCheck {
    name: &'static str,
    severity: Severity,
    detail: String,
    hint: String,
}

impl DiagnosticCheck {
    fn passed(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            severity: Severity::Ok,
            detail: detail.into(),
            hint: "No action required.".to_owned(),
        }
    }

    /// A check that passes but leaves the operator something to look at.
    fn warning(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name,
            severity: Severity::Warning,
            detail: detail.into(),
            hint: hint.into(),
        }
    }

    fn failed(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name,
            severity: Severity::Error,
            detail: detail.into(),
            hint: hint.into(),
        }
    }

    /// Warnings pass: only an error fails the check and the command.
    fn is_passed(&self) -> bool {
        self.severity != Severity::Error
    }
}

fn run_doctor(environment: &DiagnosticEnvironment) -> Rendered {
    let servers = configured_mcp_servers(environment);
    let checks = [
        check_binary(environment),
        check_data_directory(environment),
        check_database(environment),
        check_config(environment),
        check_tokenizers(),
        check_grammars(),
        check_mcp_server(environment, &servers),
    ];
    let success = checks.iter().all(DiagnosticCheck::is_passed);
    let json_checks = checks
        .iter()
        .map(|check| {
            serde_json::json!({
                "name": check.name,
                "passed": check.is_passed(),
                "severity": check.severity.code(),
                "detail": check.detail,
                "hint": check.hint,
            })
        })
        .collect::<Vec<_>>();
    let human = checks
        .iter()
        .map(|check| match check.severity {
            Severity::Ok => format!("[PASS] {}: {}", check.name, check.detail),
            Severity::Warning => format!(
                "[WARN] {}: {}\n       hint: {}",
                check.name, check.detail, check.hint
            ),
            Severity::Error => format!(
                "[FAIL] {}: {}\n       hint: {}",
                check.name, check.detail, check.hint
            ),
        })
        .collect::<Vec<_>>()
        .join("\n");
    Rendered {
        json: serde_json::json!({
            "schema_version": 1,
            "data": {
                "checks": json_checks,
                "mcp_servers": servers.iter().map(McpServerReport::to_json).collect::<Vec<_>>(),
            },
        }),
        human: format!("{human}\n"),
        success,
    }
}

/// Health of the Compresso MCP server one coding client is configured to start.
struct McpServerReport {
    client: &'static str,
    config_path: PathBuf,
    health: McpServerHealth,
}

impl McpServerReport {
    fn to_json(&self) -> serde_json::Value {
        let mut value = self.health.to_json();
        value["client"] = self.client.into();
        value["config_path"] = self.config_path.display().to_string().into();
        value
    }
}

/// Assesses every user-scope client entry, starting each distinct launcher at
/// most once so the whole check stays bounded by one deadline per launcher.
fn configured_mcp_servers(environment: &DiagnosticEnvironment) -> Vec<McpServerReport> {
    let Some(home) = environment.home_dir.as_deref() else {
        return Vec::new();
    };
    // Receipts record the canonical configuration path, so resolve the home
    // directory the same way the adapters do before looking one up.
    let home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let receipts = FileReceiptStore::new(super::integrations::receipt_store_path(
        &environment.data_dir,
    ));
    let args = mcp_probe::user_scope_args();
    let mut reports: Vec<McpServerReport> = Vec::new();
    let mut assessed: Vec<(ConfiguredServer, LauncherTrust, McpServerHealth)> = Vec::new();
    for config in mcp_probe::user_client_configs(&home) {
        let server = read_configured_server(config.format, &config.path, &args);
        if server == ConfiguredServer::NotConfigured {
            continue;
        }
        let receipt_authenticated = receipts
            .active_for(config.client, &config.path)
            .ok()
            .flatten()
            .is_some_and(|receipt| {
                receipt_authenticates_server(&receipt, config.format, &config.path)
            });
        let trust = LauncherTrust::new(Some(&environment.executable_path), receipt_authenticated);
        let health = match assessed
            .iter()
            .find(|(seen, seen_trust, _)| *seen == server && *seen_trust == trust)
        {
            Some((_, _, health)) => relabel(health, config.client),
            None => {
                let health = mcp_probe::assess(
                    &server,
                    config.client,
                    &trust,
                    environment.mcp_probe_timeout,
                );
                assessed.push((server, trust, health.clone()));
                health
            }
        };
        reports.push(McpServerReport {
            client: config.client,
            config_path: config.path,
            health,
        });
    }
    reports
}

/// Reuses a probe verdict for another client that starts the same launcher,
/// pointing its fix text at that client's own install command.
fn relabel(health: &McpServerHealth, client: &str) -> McpServerHealth {
    let mut health = health.clone();
    for known in ["claude-code", "codex", "cursor"] {
        health.fix = health.fix.replace(
            &format!("compresso integrations install {known}"),
            &format!("compresso integrations install {client}"),
        );
    }
    health
}

fn check_mcp_server(
    environment: &DiagnosticEnvironment,
    servers: &[McpServerReport],
) -> DiagnosticCheck {
    const NAME: &str = "mcp_server";
    if environment.home_dir.is_none() {
        return DiagnosticCheck::passed(NAME, "skipped: no home directory is available");
    }
    if servers.is_empty() {
        return DiagnosticCheck::passed(
            NAME,
            "no coding client is configured to start Compresso; run `compresso setup` to add one",
        );
    }
    let detail = servers
        .iter()
        .map(|server| {
            format!(
                "{}: {} ({})",
                server.client,
                server.health.state.code(),
                server.health.detail
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let severity_of = |server: &McpServerReport| server.health.state.severity();
    // Errors and warnings both keep their fix text; only an error fails.
    let fixes = servers
        .iter()
        .filter(|server| severity_of(server) != Severity::Ok)
        .map(|server| format!("{}: {}", server.client, server.health.fix))
        .collect::<Vec<_>>()
        .join(" ");
    if servers
        .iter()
        .any(|server| severity_of(server) == Severity::Error)
    {
        DiagnosticCheck::failed(NAME, detail, fixes)
    } else if fixes.is_empty() {
        DiagnosticCheck::passed(NAME, detail)
    } else {
        DiagnosticCheck::warning(NAME, detail, fixes)
    }
}

fn check_binary(environment: &DiagnosticEnvironment) -> DiagnosticCheck {
    match environment.executable_path.canonicalize() {
        Ok(path) if path.is_file() => {
            DiagnosticCheck::passed("binary_resolvable", format!("resolved {}", path.display()))
        }
        Ok(path) => DiagnosticCheck::failed(
            "binary_resolvable",
            format!("{} is not a file", path.display()),
            "Reinstall Compresso and verify that the executable is on PATH.",
        ),
        Err(error) => DiagnosticCheck::failed(
            "binary_resolvable",
            error.to_string(),
            format!(
                "Reinstall Compresso or repair the executable at {}.",
                environment.executable_path.display()
            ),
        ),
    }
}

fn check_data_directory(environment: &DiagnosticEnvironment) -> DiagnosticCheck {
    let result = (|| -> std::io::Result<()> {
        fs::create_dir_all(&environment.data_dir)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let probe = environment.data_dir.join(format!(
            ".compresso-write-check-{}-{nonce}",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        file.write_all(b"ok")?;
        drop(file);
        fs::remove_file(probe)
    })();
    match result {
        Ok(()) => DiagnosticCheck::passed(
            "data_dir_writable",
            format!("{} is writable", environment.data_dir.display()),
        ),
        Err(error) => DiagnosticCheck::failed(
            "data_dir_writable",
            error.to_string(),
            format!(
                "Create {} and grant the current user read/write access.",
                environment.data_dir.display()
            ),
        ),
    }
}

fn check_database(environment: &DiagnosticEnvironment) -> DiagnosticCheck {
    match Store::open(&environment.database_path) {
        Ok(_) => DiagnosticCheck::passed(
            "database_opens",
            format!(
                "{} opened with current migrations",
                environment.database_path.display()
            ),
        ),
        Err(error) => DiagnosticCheck::failed(
            "database_opens",
            error.to_string(),
            format!(
                "Check permissions for {} and restore or remove a corrupt local database.",
                environment.database_path.display()
            ),
        ),
    }
}

fn check_config(environment: &DiagnosticEnvironment) -> DiagnosticCheck {
    match Config::load_from(
        &environment.config_path,
        &environment.environment,
        ConfigOverrides::default(),
    ) {
        Ok(_) => DiagnosticCheck::passed(
            "config_parses",
            format!("{} parses", environment.config_path.display()),
        ),
        Err(error) => DiagnosticCheck::failed(
            "config_parses",
            error.to_string(),
            format!(
                "Fix or remove the invalid configuration file {}.",
                environment.config_path.display()
            ),
        ),
    }
}

fn check_tokenizers() -> DiagnosticCheck {
    let providers = [TokenProvider::OpenAiO200k, TokenProvider::OpenAiCl100k];
    let initialized = providers.into_iter().all(|provider| {
        let counter = counter_for(provider);
        counter.provider() == provider && counter.count("Compresso tokenizer check") > 0
    });
    if initialized {
        DiagnosticCheck::passed(
            "tokenizers_initialize",
            "OpenAI o200k and cl100k tokenizers initialized",
        )
    } else {
        DiagnosticCheck::failed(
            "tokenizers_initialize",
            "one or more exact tokenizers fell back to heuristic counting",
            "Reinstall the binary with its embedded tokenizer data intact.",
        )
    }
}

fn check_grammars() -> DiagnosticCheck {
    let fixtures = [
        (Language::Rust, "fn check() {}"),
        (Language::TypeScript, "export function check(): void {}"),
        (Language::Tsx, "export const Check = () => <div />;"),
        (Language::JavaScript, "export function check() {}"),
        (Language::Python, "def check():\n    pass\n"),
    ];
    for (language, source) in fixtures {
        if let Err(error) = ParserRegistry::extract(language, source) {
            return DiagnosticCheck::failed(
                "tree_sitter_grammars_load",
                error.to_string(),
                "Reinstall the binary with all embedded tree-sitter grammars.",
            );
        }
    }
    DiagnosticCheck::passed(
        "tree_sitter_grammars_load",
        "Rust, TypeScript, TSX, JavaScript, and Python grammars loaded",
    )
}

const fn source_name(source: ConfigSource) -> &'static str {
    source.as_str()
}
