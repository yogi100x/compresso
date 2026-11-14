//! Codex CLI and Desktop configuration adapter.
//!
//! Both Codex clients read the same user configuration. Detection is kept
//! filesystem-only: callers inject the bounded `PATH` directories and version
//! evidence they obtained from their trusted host boundary.

use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::plan::{MutationRequest, hash_toml_owned_item};
use crate::server_entry::{is_adapter_args, with_recall_source};
use crate::text_fidelity::strip_bom_str;
use crate::{
    ConfigFormat, Detection, InstallCtx, InstallReceipt, Integration, IntegrationError,
    MutationKind, MutationPlan, PlanApproval, PlanManager, ReceiptStore, RestoreReport, Scope,
    VerificationCheck, VerificationReport,
};

const CLIENT_ID: &str = "codex";
const OWNED_PATH: &str = "mcp_servers.compresso";
const CONFIG_RELATIVE_PATH: &str = ".codex/config.toml";
const MAX_SEARCH_DIRECTORIES: usize = 128;

/// Platform family used by Codex detection.
///
/// It is injectable so unsupported-platform behavior can be tested without
/// compiling for that target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexPlatform {
    /// Apple desktop platforms.
    MacOs,
    /// Linux desktop platforms.
    Linux,
    /// Windows desktop platforms.
    Windows,
    /// A platform for which this adapter has no safe config contract.
    Unsupported(String),
}

impl CodexPlatform {
    /// Returns the platform family selected at compile time.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Unsupported(std::env::consts::OS.to_owned())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecutableBinding {
    canonical_path: PathBuf,
    metadata_fingerprint: String,
}

impl ExecutableBinding {
    fn capture(path: &Path) -> Result<Self, IntegrationError> {
        let canonical_path = fs::canonicalize(path).map_err(|source| IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let metadata = fs::metadata(&canonical_path).map_err(|source| IntegrationError::Io {
            path: canonical_path.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(invalid("executable evidence must identify a regular file"));
        }
        Ok(Self {
            canonical_path,
            metadata_fingerprint: metadata_fingerprint(&metadata),
        })
    }

    fn matches(&self, path: &Path) -> bool {
        Self::capture(path).is_ok_and(|current| current == *self)
    }
}

/// Strict client-version evidence bound to one canonical Codex executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexVersionEvidence {
    executable: ExecutableBinding,
    version: String,
}

impl CodexVersionEvidence {
    /// Parses `codex --version` output and binds it to the executable that was
    /// invoked by the trusted, process-capable outer boundary.
    pub fn from_output(executable: &Path, output: &str) -> Result<Self, IntegrationError> {
        let version = parse_version_evidence(output)
            .ok_or_else(|| invalid("Codex version evidence is not strict semantic versioning"))?;
        Ok(Self {
            executable: ExecutableBinding::capture(executable)?,
            version,
        })
    }
}

/// Process-free client verification method used by trusted outer boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexVerificationMethod {
    /// A trusted `codex mcp list` caller reported whether Compresso was listed.
    McpList { compresso_listed: bool },
    /// A trusted MCP initialize probe reported whether Compresso initialized.
    LiveInitialize { initialized: bool },
}

/// Trusted verification evidence bound to exact config bytes and the canonical
/// configured Compresso executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexVerificationEvidence {
    method: CodexVerificationMethod,
    command: ExecutableBinding,
    config_hash: String,
}

impl CodexVerificationEvidence {
    /// Records a `codex mcp list` observation for the exact current config.
    pub fn mcp_list(
        configured_command: &Path,
        config_bytes: &[u8],
        compresso_listed: bool,
    ) -> Result<Self, IntegrationError> {
        Self::new(
            configured_command,
            config_bytes,
            CodexVerificationMethod::McpList { compresso_listed },
        )
    }

    /// Records a live MCP initialize observation for the exact current config.
    /// This is the required fallback when the installed Codex lacks `mcp list`.
    pub fn live_initialize(
        configured_command: &Path,
        config_bytes: &[u8],
        initialized: bool,
    ) -> Result<Self, IntegrationError> {
        Self::new(
            configured_command,
            config_bytes,
            CodexVerificationMethod::LiveInitialize { initialized },
        )
    }

    fn new(
        configured_command: &Path,
        config_bytes: &[u8],
        method: CodexVerificationMethod,
    ) -> Result<Self, IntegrationError> {
        Ok(Self {
            method,
            command: ExecutableBinding::capture(configured_command)?,
            config_hash: exact_bytes_hash(config_bytes),
        })
    }
}

/// Filesystem-only Codex adapter.
///
/// `search_directories` is deliberately capped during lookup. `version_evidence`
/// is injected because this crate is not permitted to spawn `codex --version`.
#[derive(Clone, Debug)]
pub struct CodexAdapter {
    home_dir: PathBuf,
    search_directories: Vec<PathBuf>,
    version_evidence: Option<CodexVersionEvidence>,
    compresso_version: String,
    platform: CodexPlatform,
    verification_evidence: Option<CodexVerificationEvidence>,
}

/// Naming-compatible public adapter type used by integration registries.
pub type CodexIntegration = CodexAdapter;

impl CodexAdapter {
    /// Creates an adapter for the current platform.
    pub fn new(
        home_dir: impl Into<PathBuf>,
        search_directories: Vec<PathBuf>,
        version_evidence: Option<CodexVersionEvidence>,
        compresso_version: impl Into<String>,
    ) -> Result<Self, IntegrationError> {
        Self::with_platform(
            home_dir,
            search_directories,
            version_evidence,
            compresso_version,
            CodexPlatform::current(),
        )
    }

    /// Creates an adapter with explicit platform evidence.
    pub fn with_platform(
        home_dir: impl Into<PathBuf>,
        search_directories: Vec<PathBuf>,
        version_evidence: Option<CodexVersionEvidence>,
        compresso_version: impl Into<String>,
        platform: CodexPlatform,
    ) -> Result<Self, IntegrationError> {
        Ok(Self {
            home_dir: canonical_home(home_dir.into())?,
            search_directories,
            version_evidence,
            compresso_version: compresso_version.into(),
            platform,
            verification_evidence: None,
        })
    }

    /// Supplies trusted verification evidence obtained by a process-capable
    /// outer boundary.
    pub fn with_verification_evidence(mut self, evidence: CodexVerificationEvidence) -> Self {
        self.verification_evidence = Some(evidence);
        self
    }

    /// The single config path shared by Codex CLI and Codex Desktop.
    pub fn config_path(&self) -> PathBuf {
        self.home_dir.join(CONFIG_RELATIVE_PATH)
    }

    fn unsupported_reason(&self, scope: &Scope) -> Option<String> {
        if let Scope::Project(_) = scope {
            return Some("Codex supports only the user config scope".to_owned());
        }
        match &self.platform {
            CodexPlatform::Unsupported(name) => {
                Some(format!("Codex integration is unsupported on {name}"))
            }
            _ => None,
        }
    }

    fn resolve_codex_binary(&self) -> Option<PathBuf> {
        let names: &[&str] = if self.platform == CodexPlatform::Windows {
            &["codex.exe", "codex.cmd"]
        } else {
            &["codex"]
        };
        self.search_directories
            .iter()
            .take(MAX_SEARCH_DIRECTORIES)
            .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
            .find_map(|candidate| {
                if executable_metadata(&candidate).is_ok() {
                    fs::canonicalize(candidate).ok()
                } else {
                    None
                }
            })
    }

    fn parsed_version(&self, executable: &Path) -> Option<String> {
        match &self.version_evidence {
            Some(evidence) if evidence.executable.matches(executable) => {
                Some(evidence.version.clone())
            }
            Some(_) => None,
            None => None,
        }
    }

    /// Resolves the shared Codex config path after rejecting symlinked or
    /// out-of-home targets. Callers must use this before any recovery write.
    pub fn checked_config_path(&self) -> Result<PathBuf, IntegrationError> {
        checked_config_path(&self.home_dir)
    }

    fn ensure_supported(&self, scope: &Scope) -> Result<(), IntegrationError> {
        self.unsupported_reason(scope).map_or(Ok(()), |reason| {
            Err(IntegrationError::Unsupported { reason })
        })
    }
}

impl Integration for CodexAdapter {
    fn client_id(&self) -> &'static str {
        CLIENT_ID
    }

    fn detect(&self, scope: &Scope) -> Detection {
        if let Some(reason) = self.unsupported_reason(scope) {
            return Detection::Unsupported { reason };
        }
        let config_path = match self.checked_config_path() {
            Ok(path) => path,
            Err(error) => {
                return Detection::Unsupported {
                    reason: error.to_string(),
                };
            }
        };
        let Some(executable) = self.resolve_codex_binary() else {
            return Detection::NotFound;
        };
        match self.parsed_version(&executable) {
            Some(version) => Detection::Installed {
                version,
                config_path,
            },
            None => Detection::Unsupported {
                reason: "Codex version evidence is unavailable or malformed".to_owned(),
            },
        }
    }

    fn plan_install(
        &self,
        manager: &PlanManager,
        context: &InstallCtx,
    ) -> Result<MutationPlan, IntegrationError> {
        self.ensure_supported(&context.scope)?;
        let command = resolved_compresso_command(&context.compresso_bin, &self.platform)?;
        let config_path = self.checked_config_path()?;
        let mut document = read_document(&config_path)?;
        replace_owned_table(&mut document, &command, context.session_recall)?;
        manager.issue(MutationRequest {
            operation: MutationKind::Install,
            client: CLIENT_ID.to_owned(),
            config_path,
            config_format: ConfigFormat::Toml,
            target_contents: document.to_string(),
            owned_paths: vec![OWNED_PATH.to_owned()],
            compresso_version: self.compresso_version.clone(),
            prior_receipt: None,
            warnings: Vec::new(),
        })
    }

    fn install(
        &self,
        manager: &PlanManager,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
    ) -> Result<InstallReceipt, IntegrationError> {
        self.checked_config_path()?;
        manager.apply_install(approval, receipts)
    }

    fn verify(&self, scope: &Scope) -> VerificationReport {
        if let Some(reason) = self.unsupported_reason(scope) {
            return VerificationReport {
                client: CLIENT_ID.to_owned(),
                verified: false,
                checks: vec![VerificationCheck {
                    name: "scope_platform_supported".to_owned(),
                    passed: false,
                    detail: reason,
                }],
            };
        }

        let path = match self.checked_config_path() {
            Ok(path) => path,
            Err(_) => {
                return VerificationReport {
                    client: CLIENT_ID.to_owned(),
                    verified: false,
                    checks: vec![VerificationCheck {
                        name: "config_target_safe".to_owned(),
                        passed: false,
                        detail: "Codex config target is outside the safe home boundary or uses a symlink"
                            .to_owned(),
                    }],
                };
            }
        };
        let config_bytes = fs::read(&path).ok();
        let parsed = config_bytes
            .as_deref()
            .and_then(|contents| std::str::from_utf8(contents).ok())
            .and_then(|contents| strip_bom_str(contents).parse::<DocumentMut>().ok());
        let parse_passed = parsed.is_some();
        let entry = parsed.as_ref().and_then(owned_table);
        let entry_passed = entry.is_some_and(is_exact_server_entry);
        let command = entry
            .and_then(|table| table.get("command"))
            .and_then(Item::as_str)
            .map(PathBuf::from);
        let binary_passed = command.as_deref().is_some_and(|command| {
            command.is_absolute() && compresso_executable_metadata(command, &self.platform).is_ok()
        });
        let (evidence_name, evidence_passed, evidence_detail) = verification_check(
            self.verification_evidence.as_ref(),
            command.as_deref(),
            config_bytes.as_deref(),
        );
        let checks = vec![
            VerificationCheck {
                name: "config_parses".to_owned(),
                passed: parse_passed,
                detail: if parse_passed {
                    "Codex config is valid TOML".to_owned()
                } else {
                    "Codex config is missing or malformed".to_owned()
                },
            },
            VerificationCheck {
                name: "server_entry".to_owned(),
                passed: entry_passed,
                detail: if entry_passed {
                    "Compresso entry has the exact command and MCP serve arguments".to_owned()
                } else {
                    "Compresso entry is missing or has an unexpected shape".to_owned()
                },
            },
            VerificationCheck {
                name: "binary_resolves".to_owned(),
                passed: binary_passed,
                detail: if binary_passed {
                    "configured command is an absolute executable file".to_owned()
                } else {
                    "configured command is missing, relative, or not executable".to_owned()
                },
            },
            VerificationCheck {
                name: evidence_name.to_owned(),
                passed: evidence_passed,
                detail: evidence_detail.to_owned(),
            },
        ];
        VerificationReport {
            client: CLIENT_ID.to_owned(),
            verified: checks.iter().all(|check| check.passed),
            checks,
        }
    }

    fn plan_uninstall(
        &self,
        manager: &PlanManager,
        receipt: &InstallReceipt,
    ) -> Result<MutationPlan, IntegrationError> {
        self.ensure_supported(&Scope::User)?;
        let config_path = self.checked_config_path()?;
        if receipt.client != CLIENT_ID
            || receipt.config_path != config_path
            || receipt.keys_owned != [OWNED_PATH]
        {
            return Err(invalid("receipt does not belong to this Codex adapter"));
        }

        let mut document = read_existing_document(&config_path)?;
        let current_hash = owned_table(&document).map(hash_toml_owned_item);
        let installed_hash = receipt.owned_value_hashes.get(OWNED_PATH);
        let mut warnings = Vec::new();
        if current_hash.as_ref() == installed_hash {
            remove_owned_table(&mut document)?;
        } else if !(receipt.uninstalled_at.is_some() && current_hash.is_none()) {
            warnings.push(format!("key_modified:{OWNED_PATH}"));
        }

        manager.issue(MutationRequest {
            operation: MutationKind::Uninstall,
            client: CLIENT_ID.to_owned(),
            config_path,
            config_format: ConfigFormat::Toml,
            target_contents: document.to_string(),
            owned_paths: vec![OWNED_PATH.to_owned()],
            compresso_version: receipt.compresso_version.clone(),
            prior_receipt: Some(receipt.clone()),
            warnings,
        })
    }

    fn uninstall(
        &self,
        manager: &PlanManager,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
    ) -> Result<RestoreReport, IntegrationError> {
        self.checked_config_path()?;
        manager.apply_uninstall(approval, receipts)
    }
}

fn read_document(path: &Path) -> Result<DocumentMut, IntegrationError> {
    match fs::read_to_string(path) {
        Ok(contents) => strip_bom_str(&contents)
            .parse::<DocumentMut>()
            .map_err(|_| invalid("Codex config is malformed TOML")),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(source) => Err(IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_existing_document(path: &Path) -> Result<DocumentMut, IntegrationError> {
    match fs::read_to_string(path) {
        Ok(contents) => strip_bom_str(&contents)
            .parse::<DocumentMut>()
            .map_err(|_| invalid("Codex config is malformed TOML")),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(IntegrationError::UninstallTargetMissing {
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn replace_owned_table(
    document: &mut DocumentMut,
    command: &str,
    session_recall: bool,
) -> Result<(), IntegrationError> {
    if document.as_table().get("mcp_servers").is_none() {
        let mut parent = Table::new();
        parent.set_implicit(true);
        document
            .as_table_mut()
            .insert("mcp_servers", Item::Table(parent));
    }
    let mut arguments = base_server_args();
    if session_recall {
        arguments = with_recall_source(arguments, CLIENT_ID);
    }
    let mut args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    match document.as_table_mut().get_mut("mcp_servers") {
        Some(Item::Table(parent)) => {
            let mut server = Table::new();
            server.insert("command", value(command));
            server.insert("args", value(args));
            parent.insert("compresso", Item::Table(server));
            Ok(())
        }
        Some(Item::Value(Value::InlineTable(parent))) => {
            let mut server = InlineTable::new();
            server.insert("command", Value::from(command));
            server.insert("args", Value::Array(args));
            parent.insert("compresso", Value::InlineTable(server));
            Ok(())
        }
        Some(_) => Err(IntegrationError::Unsupported {
            reason: "mcp_servers uses a valid TOML value shape that is not a table".to_owned(),
        }),
        None => unreachable!("the parent table was inserted above"),
    }
}

fn remove_owned_table(document: &mut DocumentMut) -> Result<(), IntegrationError> {
    let Some(parent) = document.as_table_mut().get_mut("mcp_servers") else {
        return Ok(());
    };
    let parent = parent
        .as_table_like_mut()
        .ok_or_else(|| IntegrationError::Unsupported {
            reason: "mcp_servers uses a valid TOML value shape that is not a table".to_owned(),
        })?;
    parent.remove("compresso");
    Ok(())
}

fn owned_table(document: &DocumentMut) -> Option<&Item> {
    document.as_item().get("mcp_servers")?.get("compresso")
}

fn canonical_home(home_dir: PathBuf) -> Result<PathBuf, IntegrationError> {
    if !home_dir.is_absolute()
        || home_dir
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid(
            "Codex home must be an absolute, lexically normalized path",
        ));
    }
    let metadata = fs::symlink_metadata(&home_dir).map_err(|source| IntegrationError::Io {
        path: home_dir.clone(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(IntegrationError::Unsupported {
            reason: "Codex home must be a real directory, not a symlink".to_owned(),
        });
    }
    fs::canonicalize(&home_dir).map_err(|source| IntegrationError::Io {
        path: home_dir,
        source,
    })
}

fn checked_config_path(home_dir: &Path) -> Result<PathBuf, IntegrationError> {
    let home_metadata = fs::symlink_metadata(home_dir).map_err(|source| IntegrationError::Io {
        path: home_dir.to_path_buf(),
        source,
    })?;
    if home_metadata.file_type().is_symlink() || !home_metadata.is_dir() {
        return Err(unsafe_config_target());
    }
    let current_home = fs::canonicalize(home_dir).map_err(|source| IntegrationError::Io {
        path: home_dir.to_path_buf(),
        source,
    })?;
    if current_home != home_dir {
        return Err(unsafe_config_target());
    }

    let config_dir = home_dir.join(".codex");
    match fs::symlink_metadata(&config_dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(unsafe_config_target());
            }
            let canonical =
                fs::canonicalize(&config_dir).map_err(|source| IntegrationError::Io {
                    path: config_dir.clone(),
                    source,
                })?;
            if canonical != config_dir || !canonical.starts_with(home_dir) {
                return Err(unsafe_config_target());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(IntegrationError::Io {
                path: config_dir,
                source,
            });
        }
    }

    let config_path = home_dir.join(CONFIG_RELATIVE_PATH);
    match fs::symlink_metadata(&config_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(unsafe_config_target());
            }
            let canonical =
                fs::canonicalize(&config_path).map_err(|source| IntegrationError::Io {
                    path: config_path.clone(),
                    source,
                })?;
            if canonical != config_path || !canonical.starts_with(home_dir) {
                return Err(unsafe_config_target());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(IntegrationError::Io {
                path: config_path,
                source,
            });
        }
    }
    Ok(config_path)
}

fn unsafe_config_target() -> IntegrationError {
    IntegrationError::Unsupported {
        reason: "Codex config target must remain inside the canonical home without symlinks"
            .to_owned(),
    }
}

fn executable_metadata(path: &Path) -> Result<(), std::io::Error> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("path is not a file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "file has no executable bit",
            ));
        }
    }
    Ok(())
}

fn compresso_executable_metadata(
    path: &Path,
    platform: &CodexPlatform,
) -> Result<(), std::io::Error> {
    executable_metadata(path)?;
    if *platform == CodexPlatform::Windows
        && !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows Compresso command must be an .exe file",
        ));
    }
    Ok(())
}

fn resolved_compresso_command(
    path: &Path,
    platform: &CodexPlatform,
) -> Result<String, IntegrationError> {
    if !is_normalized_absolute_path(path) {
        return Err(invalid(
            "Compresso command path must be normalized and absolute",
        ));
    }
    let resolved = fs::canonicalize(path).map_err(|source| IntegrationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if compresso_executable_metadata(&resolved, platform).is_err() {
        return Err(invalid(
            "Compresso command path must resolve to an executable file",
        ));
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Compresso command path must be UTF-8"))
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
}

fn is_exact_server_entry(item: &Item) -> bool {
    let Some(table) = item.as_table_like() else {
        return false;
    };
    let args = table.get("args").and_then(Item::as_array);
    table.len() == 2
        && table
            .get("command")
            .and_then(Item::as_str)
            .is_some_and(|command| Path::new(command).is_absolute())
        && args
            .and_then(|args| {
                args.iter()
                    .map(|value| value.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .is_some_and(|args| is_adapter_args(&args, &base_server_args(), CLIENT_ID))
}

fn base_server_args() -> Vec<String> {
    vec!["mcp".to_owned(), "serve".to_owned()]
}

fn verification_check(
    evidence: Option<&CodexVerificationEvidence>,
    configured_command: Option<&Path>,
    config_bytes: Option<&[u8]>,
) -> (&'static str, bool, &'static str) {
    let Some(evidence) = evidence else {
        return (
            "client_verification_evidence",
            false,
            "trusted MCP-list or initialize evidence was not supplied",
        );
    };
    let binding_matches = configured_command
        .is_some_and(|command| evidence.command.matches(command))
        && config_bytes.is_some_and(|bytes| evidence.config_hash == exact_bytes_hash(bytes));
    if !binding_matches {
        return (
            "client_verification_evidence",
            false,
            "trusted verification evidence is stale or bound to another local artifact",
        );
    }
    match &evidence.method {
        CodexVerificationMethod::McpList {
            compresso_listed: true,
        } => (
            "mcp_list",
            true,
            "trusted Codex MCP list evidence includes Compresso",
        ),
        CodexVerificationMethod::McpList {
            compresso_listed: false,
        } => (
            "mcp_list",
            false,
            "trusted Codex MCP list evidence omits Compresso",
        ),
        CodexVerificationMethod::LiveInitialize { initialized: true } => (
            "live_initialize",
            true,
            "trusted MCP initialize evidence succeeded",
        ),
        CodexVerificationMethod::LiveInitialize { initialized: false } => (
            "live_initialize",
            false,
            "trusted MCP initialize evidence failed",
        ),
    }
}

fn exact_bytes_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn metadata_fingerprint(metadata: &Metadata) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&metadata.len().to_le_bytes());
    match metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
    {
        Some(duration) => {
            hasher.update(b"modified\0");
            hasher.update(&duration.as_secs().to_le_bytes());
            hasher.update(&duration.subsec_nanos().to_le_bytes());
        }
        None => {
            hasher.update(b"modified-unavailable\0");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(&metadata.dev().to_le_bytes());
        hasher.update(&metadata.ino().to_le_bytes());
        hasher.update(&metadata.ctime().to_le_bytes());
        hasher.update(&metadata.ctime_nsec().to_le_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn parse_version_evidence(evidence: &str) -> Option<String> {
    evidence
        .split_whitespace()
        .map(|part| {
            let trimmed = part.trim_matches(|character: char| {
                matches!(character, ',' | ';' | ':' | '(' | ')' | '[' | ']')
            });
            trimmed.strip_prefix('v').unwrap_or(trimmed)
        })
        .find(|part| is_semver(part))
        .map(str::to_owned)
}

fn is_semver(candidate: &str) -> bool {
    let (core_and_prerelease, build) = candidate
        .split_once('+')
        .map_or((candidate, None), |(left, right)| (left, Some(right)));
    if build.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, false)) {
        return false;
    }
    let (core, prerelease) = core_and_prerelease
        .split_once('-')
        .map_or((core_and_prerelease, None), |(left, right)| {
            (left, Some(right))
        });
    if prerelease.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, true)) {
        return false;
    }
    let mut core = core.split('.');
    [core.next(), core.next(), core.next()]
        .into_iter()
        .all(|part| part.is_some_and(is_semver_number))
        && core.next().is_none()
}

fn valid_semver_identifiers(value: &str, reject_leading_zero_numeric: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_leading_zero_numeric
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || is_semver_number(identifier))
        })
}

fn is_semver_number(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn invalid(reason: impl Into<String>) -> IntegrationError {
    IntegrationError::InvalidRequest {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{MemoryReceiptStore, PlanApproval, ReceiptStore};

    fn executable(path: &Path) {
        fs::create_dir_all(path.parent().expect("binary parent")).expect("binary parent created");
        fs::write(path, b"codex-test-binary").expect("binary created");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).expect("metadata").permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(path, permissions).expect("executable permissions");
        }
    }

    fn fixture(platform: CodexPlatform) -> (TempDir, CodexAdapter, PathBuf, PathBuf) {
        let home = tempdir().expect("temporary home");
        let bin_dir = home.path().join("bin");
        let codex_name = if platform == CodexPlatform::Windows {
            "codex.exe"
        } else {
            "codex"
        };
        let codex_bin = bin_dir.join(codex_name);
        executable(&codex_bin);
        let compresso_bin = bin_dir.join(if platform == CodexPlatform::Windows {
            "compresso.exe"
        } else {
            "compresso"
        });
        executable(&compresso_bin);
        let adapter = CodexAdapter::with_platform(
            home.path(),
            vec![bin_dir],
            Some(
                CodexVersionEvidence::from_output(&codex_bin, "codex-cli 0.42.0")
                    .expect("version evidence"),
            ),
            "0.1.0",
            platform,
        )
        .expect("Codex adapter");
        let config = adapter.config_path();
        (home, adapter, compresso_bin, config)
    }

    fn install(
        adapter: &CodexAdapter,
        binary: &Path,
        manager: &PlanManager,
        receipts: &MemoryReceiptStore,
    ) -> InstallReceipt {
        let plan = adapter
            .plan_install(
                manager,
                &InstallCtx {
                    compresso_bin: binary.to_path_buf(),
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect("install plan");
        adapter
            .install(manager, &PlanApproval::from(&plan), receipts)
            .expect("installation")
    }

    #[test]
    fn test_fresh_install_detects_and_verifies_shared_codex_config() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::MacOs);
        assert_eq!(
            adapter.detect(&Scope::User),
            Detection::Installed {
                version: "0.42.0".to_owned(),
                config_path: config.clone(),
            }
        );
        let manager = PlanManager::new("codex-fresh").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = install(&adapter, &binary, &manager, &receipts);

        let document = fs::read_to_string(&config)
            .expect("config written")
            .parse::<DocumentMut>()
            .expect("valid TOML");
        let server = owned_table(&document).expect("owned table");
        assert_eq!(
            server
                .get("args")
                .and_then(Item::as_array)
                .expect("args")
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["mcp", "serve"]
        );
        assert_eq!(receipt.keys_owned, [OWNED_PATH]);
        assert_eq!(
            server.get("command").and_then(Item::as_str),
            binary.to_str()
        );
        let config_bytes = fs::read(&config).expect("config evidence bytes");
        let evidence = CodexVerificationEvidence::mcp_list(&binary, &config_bytes, true)
            .expect("bound verification evidence");
        assert!(
            adapter
                .with_verification_evidence(evidence)
                .verify(&Scope::User)
                .verified
        );
    }

    #[test]
    fn test_existing_config_preserves_comments_and_sibling_tables() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        let original = concat!(
            "# user model comment\n",
            "model = \"gpt-5\"\n\n",
            "[mcp_servers.other] # sibling comment\n",
            "command = \"/opt/other\"\n",
            "args = [\"one\"]\n\n",
            "[features]\n",
            "experimental = true # keep me\n",
        );
        fs::write(&config, original).expect("base config");
        let manager = PlanManager::new("codex-existing").expect("manager");
        let receipts = MemoryReceiptStore::default();
        install(&adapter, &binary, &manager, &receipts);

        let installed = fs::read_to_string(&config).expect("installed config");
        let parsed = installed
            .parse::<DocumentMut>()
            .expect("valid installed TOML");
        assert_eq!(
            parsed["mcp_servers"]["other"]["command"].as_str(),
            Some("/opt/other")
        );
        assert_eq!(parsed["features"]["experimental"].as_bool(), Some(true));
        let mut stripped = parsed;
        remove_owned_table(&mut stripped).expect("owned table removable");
        assert_eq!(stripped.to_string(), original);
    }

    #[test]
    fn test_reinstall_preserves_sensitive_sibling_bytes_and_previews_only_command_change() {
        const CANARY: &str = "CODEX_SIBLING_BEARER_CANARY_f9a06b31";

        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        let original = format!(
            "# exact top-level formatting\nmodel  =  \"gpt-5\"\n\n[mcp_servers.cubic] # preserve this sibling exactly\ncommand = \"cubic\"\nhttp_headers = {{ Authorization = \"Bearer {CANARY}\" }}\n\n[mcp_servers.compresso]\ncommand = \"/old/bin/compresso\"\nargs = [\"mcp\", \"serve\"]\n\n[features]\nexperimental = true # preserve this too\n"
        );
        fs::write(&config, &original).expect("base config");
        let manager = PlanManager::new("codex-secret-safe-reinstall").expect("manager");
        let receipts = MemoryReceiptStore::default();

        let plan = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary.clone(),
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect("reinstall plan");

        assert!(plan.unified_diff.contains("/old/bin/compresso"));
        let binary_literal =
            serde_json::to_string(&binary.to_string_lossy()).expect("binary path should encode");
        assert!(plan.unified_diff.contains(&binary_literal));
        assert!(!plan.unified_diff.contains(CANARY));
        assert!(!plan.unified_diff.contains("Authorization"));
        assert!(!plan.unified_diff.contains("mcp_servers.cubic"));
        assert!(plan.unified_diff.lines().count() <= 8);

        let receipt = adapter
            .install(&manager, &PlanApproval::from(&plan), &receipts)
            .expect("reinstall should apply");
        let serialized_receipt =
            serde_json::to_string(&receipt).expect("receipt should serialize safely");
        assert!(!serialized_receipt.contains(CANARY));
        assert!(!serialized_receipt.contains("Authorization"));
        let debug_receipt = format!("{receipt:?}");
        assert!(!debug_receipt.contains(CANARY));
        assert!(!debug_receipt.contains("Authorization"));
        let installed = fs::read_to_string(&config).expect("installed config");
        assert_eq!(
            installed
                .parse::<DocumentMut>()
                .expect("installed TOML")
                .get("mcp_servers")
                .and_then(Item::as_table_like)
                .and_then(|servers| servers.get("compresso"))
                .and_then(Item::as_table_like)
                .and_then(|server| server.get("command"))
                .and_then(Item::as_str),
            binary.to_str(),
            "reinstall should write the exact resolved command value"
        );
        assert!(installed.contains(&format!(
            "[mcp_servers.cubic] # preserve this sibling exactly\ncommand = \"cubic\"\nhttp_headers = {{ Authorization = \"Bearer {CANARY}\" }}"
        )));
        let mut original_without_owned = original.parse::<DocumentMut>().expect("original TOML");
        remove_owned_table(&mut original_without_owned).expect("original owned table removable");
        let mut installed_without_owned = installed.parse::<DocumentMut>().expect("installed TOML");
        remove_owned_table(&mut installed_without_owned).expect("installed owned table removable");
        assert_eq!(
            installed_without_owned.to_string(),
            original_without_owned.to_string(),
            "all unrelated TOML bytes must remain exact"
        );
    }

    #[test]
    fn test_inline_mcp_servers_round_trip_preserves_unrelated_inline_values() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        let original = concat!(
            "# inline config stays inline\n",
            "model = \"gpt-5\"\n",
            "mcp_servers = { other = { command = \"/opt/other\", args = [\"one\"] } } # keep\n",
        );
        fs::write(&config, original).expect("inline config");
        let install_manager = PlanManager::new("codex-inline-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = install(&adapter, &binary, &install_manager, &receipts);

        let installed = fs::read_to_string(&config).expect("installed config");
        assert!(installed.contains("# inline config stays inline"));
        assert!(installed.contains("# keep"));
        let installed_document = installed.parse::<DocumentMut>().expect("valid TOML");
        assert!(is_exact_server_entry(
            owned_table(&installed_document).expect("owned inline table")
        ));
        assert_eq!(
            installed_document["mcp_servers"]["other"]["command"].as_str(),
            Some("/opt/other")
        );

        let uninstall_manager = PlanManager::new("codex-inline-uninstall").expect("manager");
        let plan = adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("uninstall plan");
        adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall");
        assert_eq!(
            fs::read_to_string(config).expect("restored inline"),
            original
        );
    }

    #[test]
    fn test_scalar_mcp_servers_is_typed_unsupported_without_rewrite() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        let original = "mcp_servers = \"externally-managed\"\n";
        fs::write(&config, original).expect("scalar config");
        let manager = PlanManager::new("codex-scalar-parent").expect("manager");
        let error = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("scalar parent is unsupported");
        assert!(matches!(error, IntegrationError::Unsupported { .. }));
        assert_eq!(fs::read_to_string(config).expect("unchanged"), original);
    }

    #[test]
    fn test_malformed_config_rejects_without_rewrite() {
        const CANARY: &str = "MALFORMED_CUBIC_BEARER_CANARY_84c5d210";

        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        let malformed = format!(
            "[mcp_servers.cubic]\nhttp_headers = {{ Authorization = \"Bearer {CANARY}\" }} invalid\n"
        );
        fs::write(&config, &malformed).expect("malformed config");
        let manager = PlanManager::new("codex-malformed").expect("manager");
        let error = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("malformed config rejected");
        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        let rendered_error = error.to_string();
        assert!(!rendered_error.contains(CANARY));
        assert!(!rendered_error.contains("Authorization"));
        assert_eq!(fs::read_to_string(config).expect("unchanged"), malformed);
    }

    #[test]
    fn test_apply_rejects_full_file_drift() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        fs::write(&config, "model = \"gpt-5\"\n").expect("base config");
        let manager = PlanManager::new("codex-drift").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let plan = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect("install plan");
        fs::write(&config, "model = \"gpt-5\"\n# concurrent edit\n").expect("drifted config");
        let error = adapter
            .install(&manager, &PlanApproval::from(&plan), &receipts)
            .expect_err("drift rejected");
        assert!(matches!(error, IntegrationError::ConfigDrift { .. }));
        assert!(
            fs::read_to_string(config)
                .expect("config remains")
                .contains("concurrent edit")
        );
    }

    #[test]
    fn test_disable_then_uninstall_retains_modified_table_and_repeats_safely() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        let install_manager = PlanManager::new("codex-disable-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = install(&adapter, &binary, &install_manager, &receipts);

        let mut document = fs::read_to_string(&config)
            .expect("installed config")
            .parse::<DocumentMut>()
            .expect("valid TOML");
        document["mcp_servers"]["compresso"]["enabled"] = value(false);
        fs::write(&config, document.to_string()).expect("user disabled entry");

        let uninstall_manager = PlanManager::new("codex-disable-uninstall").expect("manager");
        let plan = adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("uninstall plan");
        let report = adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("safe uninstall");
        assert_eq!(report.retained_keys, [OWNED_PATH]);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("key_modified"))
        );
        assert_eq!(report.warnings, [format!("key_modified:{OWNED_PATH}")]);
        assert_eq!(
            fs::read_to_string(&config)
                .expect("retained config")
                .parse::<DocumentMut>()
                .expect("valid TOML")["mcp_servers"]["compresso"]["enabled"]
                .as_bool(),
            Some(false)
        );

        let repeat_manager = PlanManager::new("codex-disable-repeat").expect("manager");
        let repeat = adapter
            .plan_uninstall(&repeat_manager, &receipt)
            .expect("repeat plan");
        let repeated = adapter
            .uninstall(&repeat_manager, &PlanApproval::from(&repeat), &receipts)
            .expect("repeat safe uninstall");
        assert!(repeated.already_applied);
        assert_eq!(repeated.retained_keys, [OWNED_PATH]);
    }

    #[test]
    fn test_clean_uninstall_removes_only_owned_table_and_repeat_is_idempotent() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        let original = "[mcp_servers.other]\ncommand = \"/opt/other\"\n";
        fs::write(&config, original).expect("base config");
        let install_manager = PlanManager::new("codex-remove-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = install(&adapter, &binary, &install_manager, &receipts);

        let uninstall_manager = PlanManager::new("codex-remove-uninstall").expect("manager");
        let plan = adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("uninstall plan");
        let report = adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall");
        assert_eq!(report.removed_keys, [OWNED_PATH]);
        assert_eq!(
            fs::read_to_string(&config).expect("restored config"),
            original
        );

        let stored = receipts
            .by_plan_hash(&receipt.plan_hash)
            .expect("receipt lookup")
            .expect("stored receipt");
        assert!(stored.uninstalled_at.is_some());
        let repeat_manager = PlanManager::new("codex-remove-repeat").expect("manager");
        let repeat = adapter
            .plan_uninstall(&repeat_manager, &stored)
            .expect("repeat plan");
        let repeated = adapter
            .uninstall(&repeat_manager, &PlanApproval::from(&repeat), &receipts)
            .expect("repeat uninstall");
        assert!(repeated.already_applied);
        assert_eq!(
            fs::read_to_string(config).expect("still restored"),
            original
        );
    }

    #[test]
    fn test_missing_config_uninstall_is_typed_and_does_not_recreate_target() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        let install_manager = PlanManager::new("codex-missing-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = install(&adapter, &binary, &install_manager, &receipts);
        fs::remove_file(&config).expect("simulate user removing config");

        let uninstall_manager = PlanManager::new("codex-missing-uninstall").expect("manager");
        let error = adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect_err("missing uninstall target rejected");
        assert!(matches!(
            error,
            IntegrationError::UninstallTargetMissing { .. }
        ));
        assert!(!config.exists());
        let sidecars = fs::read_dir(config.parent().expect("config parent"))
            .expect("config directory")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.contains("compresso-backup") || name.contains("compresso-journal"))
            .collect::<Vec<_>>();
        assert!(sidecars.is_empty());
    }

    #[test]
    fn test_unsupported_scope_and_platform_are_typed_noops() {
        let (home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        let project = home.path().join("project");
        assert!(matches!(
            adapter.detect(&Scope::Project(project.clone())),
            Detection::Unsupported { .. }
        ));
        let manager = PlanManager::new("codex-project-unsupported").expect("manager");
        let project_error = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary.clone(),
                    scope: Scope::Project(project),
                    session_recall: false,
                },
            )
            .expect_err("project scope is unsupported");
        assert!(matches!(
            project_error,
            IntegrationError::Unsupported { .. }
        ));
        assert!(!config.exists());

        let unsupported = CodexAdapter::with_platform(
            home.path(),
            Vec::new(),
            None,
            "0.1.0",
            CodexPlatform::Unsupported("plan9".to_owned()),
        )
        .expect("unsupported adapter still has a valid home");
        assert!(matches!(
            unsupported.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
        assert!(!unsupported.verify(&Scope::User).verified);
        let platform_error = unsupported
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("platform is unsupported");
        assert!(matches!(
            platform_error,
            IntegrationError::Unsupported { .. }
        ));
        assert!(!config.exists());
    }

    #[test]
    fn test_detection_is_bounded_and_requires_parseable_version_evidence() {
        let home = tempdir().expect("temporary home");
        let search = (0..=MAX_SEARCH_DIRECTORIES)
            .map(|index| home.path().join(format!("bin-{index}")))
            .collect::<Vec<_>>();
        let outside_bound = search[MAX_SEARCH_DIRECTORIES].join("codex");
        executable(&outside_bound);
        let bounded = CodexAdapter::with_platform(
            home.path(),
            search,
            Some(
                CodexVersionEvidence::from_output(&outside_bound, "codex 0.42.0")
                    .expect("version evidence"),
            ),
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("bounded adapter");
        assert_eq!(bounded.detect(&Scope::User), Detection::NotFound);

        let first = home.path().join("first");
        executable(&first.join("codex"));
        let malformed = CodexAdapter::with_platform(
            home.path(),
            vec![first],
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("adapter without evidence");
        assert!(matches!(
            malformed.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
    }

    #[test]
    fn test_strict_semver_rejects_adversarial_evidence() {
        for valid in ["0.0.0", "1.2.3", "1.2.3-alpha.1+build.5", "codex v9.8.7"] {
            assert!(
                parse_version_evidence(valid).is_some(),
                "expected valid evidence: {valid}"
            );
        }
        for invalid in [
            "1.2",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-01",
            "1.2.3.4",
            "1.2.3+",
            "1.2.3-",
            "1.2.3_unsafe",
            "１.２.３",
        ] {
            assert_eq!(
                parse_version_evidence(invalid),
                None,
                "expected invalid evidence: {invalid}"
            );
        }
    }

    #[test]
    fn test_version_evidence_is_bound_to_resolved_codex_artifact() {
        let home = tempdir().expect("temporary home");
        let first_dir = home.path().join("first");
        let second_dir = home.path().join("second");
        let first = first_dir.join("codex");
        let second = second_dir.join("codex");
        executable(&first);
        executable(&second);
        let evidence = CodexVersionEvidence::from_output(&second, "codex 1.2.3").expect("evidence");
        let adapter = CodexAdapter::with_platform(
            home.path(),
            vec![first_dir, second_dir],
            Some(evidence),
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("adapter");

        assert!(matches!(
            adapter.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
    }

    #[test]
    fn test_version_evidence_rejects_replaced_codex_artifact() {
        let home = tempdir().expect("temporary home");
        let bin = home.path().join("bin");
        let codex = bin.join("codex");
        executable(&codex);
        let evidence = CodexVersionEvidence::from_output(&codex, "codex 1.2.3").expect("evidence");
        fs::write(&codex, b"replaced-codex-test-binary-with-another-size")
            .expect("artifact replaced");
        let adapter = CodexAdapter::with_platform(
            home.path(),
            vec![bin],
            Some(evidence),
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("adapter");

        assert!(matches!(
            adapter.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
    }

    #[test]
    fn test_semver_ancestor_is_not_accepted_as_artifact_evidence() {
        let home = tempdir().expect("temporary home");
        let bin = home.path().join("1.2.3/bin");
        executable(&bin.join("codex"));
        let adapter = CodexAdapter::with_platform(
            home.path(),
            vec![bin],
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("adapter");

        assert!(matches!(
            adapter.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
    }

    #[test]
    fn test_detection_requires_version_output_even_with_package_metadata() {
        let home = tempdir().expect("temporary home");
        let package = home.path().join("node_modules/@openai/codex");
        let bin = package.join("bin");
        executable(&bin.join("codex"));
        fs::write(
            package.join("package.json"),
            br#"{"name":"@openai/codex","version":"3.2.1-beta.1"}"#,
        )
        .expect("package metadata");
        let adapter = CodexAdapter::with_platform(
            home.path(),
            vec![bin],
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("package-bound adapter");
        assert!(matches!(
            adapter.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
    }

    #[test]
    fn test_package_metadata_name_must_match_recognized_layout() {
        let home = tempdir().expect("temporary home");
        let package = home.path().join("node_modules/@openai/codex");
        let bin = package.join("bin");
        executable(&bin.join("codex"));
        fs::write(
            package.join("package.json"),
            br#"{"name":"codex","version":"3.2.1"}"#,
        )
        .expect("package metadata");
        let adapter = CodexAdapter::with_platform(
            home.path(),
            vec![bin],
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("adapter");

        assert!(matches!(
            adapter.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_install_rejects_non_normalized_absolute_launcher_paths() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        let parent = binary.parent().expect("bin directory");
        let parent_text = parent.to_str().expect("UTF-8 bin directory");
        let nested = parent.join("nested");
        fs::create_dir(&nested).expect("nested directory created");
        let cases = [
            PathBuf::from(format!("{parent_text}/./compresso")),
            PathBuf::from(format!("{parent_text}//compresso")),
            nested.join("../compresso"),
        ];
        let manager = PlanManager::new("codex-non-normalized-launcher").expect("manager");

        for path in cases {
            let error = adapter
                .plan_install(
                    &manager,
                    &InstallCtx {
                        compresso_bin: path,
                        scope: Scope::User,
                        session_recall: false,
                    },
                )
                .expect_err("non-normalized launcher path rejected");
            assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        }
        assert!(!config.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_install_rejects_non_executable_without_config_side_effects() {
        use std::os::unix::fs::PermissionsExt;

        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o600))
            .expect("remove executable mode");
        let manager = PlanManager::new("codex-non-executable").expect("manager");
        let error = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("non-executable command rejected");
        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        assert!(!config.exists());
    }

    #[test]
    fn test_windows_install_rejects_non_exe_regular_file() {
        let (_home, adapter, _binary, config) = fixture(CodexPlatform::Windows);
        let text_command = config
            .parent()
            .expect("config parent")
            .parent()
            .expect("home")
            .join("bin/compresso.txt");
        executable(&text_command);
        let manager = PlanManager::new("codex-windows-extension").expect("manager");
        let error = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: text_command,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("non-exe Windows command rejected");
        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        assert!(!config.exists());
    }

    #[test]
    fn test_relative_and_parent_component_homes_are_rejected() {
        let relative = CodexAdapter::with_platform(
            "relative-home",
            Vec::new(),
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect_err("relative home rejected");
        assert!(matches!(relative, IntegrationError::InvalidRequest { .. }));

        let root = tempdir().expect("temporary root");
        let traversing = root.path().join("child/../child");
        let parent_component = CodexAdapter::with_platform(
            traversing,
            Vec::new(),
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect_err("parent component rejected");
        assert!(matches!(
            parent_component,
            IntegrationError::InvalidRequest { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_home_and_config_targets_are_rejected_without_writes() {
        use std::os::unix::fs::symlink;

        let root = tempdir().expect("temporary root");
        let real_home = root.path().join("real-home");
        fs::create_dir(&real_home).expect("real home");
        let linked_home = root.path().join("linked-home");
        symlink(&real_home, &linked_home).expect("home symlink");
        let home_error = CodexAdapter::with_platform(
            &linked_home,
            Vec::new(),
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect_err("symlinked home rejected");
        assert!(matches!(home_error, IntegrationError::Unsupported { .. }));

        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        let outside = root.path().join("outside-dir");
        fs::create_dir(&outside).expect("outside directory");
        symlink(&outside, config.parent().expect("config directory"))
            .expect("config directory symlink");
        let manager = PlanManager::new("codex-symlinked-directory").expect("manager");
        let error = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("symlinked config directory rejected");
        assert!(matches!(error, IntegrationError::Unsupported { .. }));
        assert!(
            fs::read_dir(&outside)
                .expect("outside readable")
                .next()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_config_file_symlink_and_post_plan_directory_swap_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempdir().expect("temporary root");
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("config parent");
        let outside_file = root.path().join("outside.toml");
        let outside_original = "model = \"outside\"\n";
        fs::write(&outside_file, outside_original).expect("outside config");
        symlink(&outside_file, &config).expect("config symlink");
        let manager = PlanManager::new("codex-symlinked-file").expect("manager");
        let error = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: binary.clone(),
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("symlinked config file rejected");
        assert!(matches!(error, IntegrationError::Unsupported { .. }));
        assert_eq!(
            fs::read_to_string(&outside_file).expect("outside unchanged"),
            outside_original
        );

        fs::remove_file(&config).expect("remove config symlink");
        fs::remove_dir(config.parent().expect("config parent")).expect("remove safe directory");
        let plan_manager = PlanManager::new("codex-post-plan-swap").expect("manager");
        let plan = adapter
            .plan_install(
                &plan_manager,
                &InstallCtx {
                    compresso_bin: binary,
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect("safe plan");
        let outside_dir = root.path().join("outside-after-plan");
        fs::create_dir(&outside_dir).expect("outside directory");
        symlink(&outside_dir, config.parent().expect("config parent"))
            .expect("swap in directory symlink");
        let receipts = MemoryReceiptStore::default();
        let error = adapter
            .install(&plan_manager, &PlanApproval::from(&plan), &receipts)
            .expect_err("post-plan symlink swap rejected");
        assert!(matches!(error, IntegrationError::Unsupported { .. }));
        assert!(
            fs::read_dir(outside_dir)
                .expect("outside readable")
                .next()
                .is_none()
        );
    }

    #[test]
    fn test_owned_table_header_comment_is_retained_as_user_modification() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        let install_manager = PlanManager::new("codex-header-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = install(&adapter, &binary, &install_manager, &receipts);
        let installed = fs::read_to_string(&config).expect("installed config");
        let modified = installed.replacen(
            "[mcp_servers.compresso]",
            "[mcp_servers.compresso] # user-owned note",
            1,
        );
        fs::write(&config, &modified).expect("header comment added");

        let uninstall_manager = PlanManager::new("codex-header-uninstall").expect("manager");
        let plan = adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("safe retained plan");
        let report = adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("safe retained uninstall");
        assert_eq!(report.retained_keys, [OWNED_PATH]);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("key_modified"))
        );
        assert_eq!(
            fs::read_to_string(config).expect("comment retained"),
            modified
        );
    }

    #[test]
    fn test_verify_fails_binary_resolves_for_missing_command() {
        let (_home, adapter, _binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        fs::write(
            &config,
            "[mcp_servers.compresso]\ncommand = \"/missing/compresso\"\nargs = [\"mcp\", \"serve\"]\n",
        )
        .expect("config written");
        let report = adapter.verify(&Scope::User);
        assert!(!report.verified);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "binary_resolves" && !check.passed)
        );
    }

    #[test]
    fn test_verify_requires_exact_args_and_trusted_verification_path() {
        let (_home, mut adapter, binary, config) = fixture(CodexPlatform::Linux);
        fs::create_dir_all(config.parent().expect("config parent")).expect("parent created");
        fs::write(
            &config,
            format!(
                "[mcp_servers.compresso]\ncommand = {:?}\nargs = [\"serve\", \"mcp\"]\n",
                binary.to_string_lossy()
            ),
        )
        .expect("config written");
        let wrong_shape = adapter.verify(&Scope::User);
        assert!(!wrong_shape.verified);
        assert!(
            wrong_shape
                .checks
                .iter()
                .any(|check| check.name == "server_entry" && !check.passed)
        );

        fs::write(
            &config,
            format!(
                "[mcp_servers.compresso]\ncommand = {:?}\nargs = [\"mcp\", \"serve\"]\n",
                binary.to_string_lossy()
            ),
        )
        .expect("config rewritten");
        adapter.verification_evidence = None;
        let missing_evidence = adapter.verify(&Scope::User);
        assert!(!missing_evidence.verified);
        assert!(
            missing_evidence
                .checks
                .iter()
                .any(|check| { check.name == "client_verification_evidence" && !check.passed })
        );

        let current = fs::read(&config).expect("current config bytes");
        let alternate_command = binary.with_file_name("compresso-alternate");
        executable(&alternate_command);
        adapter.verification_evidence = Some(
            CodexVerificationEvidence::mcp_list(&alternate_command, &current, true)
                .expect("mismatched command evidence"),
        );
        let wrong_command_binding = adapter.verify(&Scope::User);
        assert!(!wrong_command_binding.verified);
        assert!(
            wrong_command_binding
                .checks
                .iter()
                .any(|check| { check.name == "client_verification_evidence" && !check.passed })
        );

        adapter.verification_evidence = Some(
            CodexVerificationEvidence::mcp_list(&binary, &current, true)
                .expect("bound MCP-list evidence"),
        );
        let confirmed = adapter.verify(&Scope::User);
        assert!(confirmed.verified);
        assert!(
            confirmed
                .checks
                .iter()
                .any(|check| check.name == "mcp_list" && check.passed)
        );

        fs::write(
            &config,
            format!(
                "# changed after evidence\n[mcp_servers.compresso]\ncommand = {:?}\nargs = [\"mcp\", \"serve\"]\n",
                binary.to_string_lossy()
            ),
        )
        .expect("config changed after evidence");
        let stale = adapter.verify(&Scope::User);
        assert!(!stale.verified);
        assert!(
            stale
                .checks
                .iter()
                .any(|check| { check.name == "client_verification_evidence" && !check.passed })
        );
    }

    #[test]
    fn test_live_initialize_is_the_only_mcp_list_fallback() {
        let (_home, adapter, binary, config) = fixture(CodexPlatform::Linux);
        let manager = PlanManager::new("codex-live-fallback-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        install(&adapter, &binary, &manager, &receipts);
        let config_bytes = fs::read(&config).expect("current config bytes");

        let passed = adapter
            .clone()
            .with_verification_evidence(
                CodexVerificationEvidence::live_initialize(&binary, &config_bytes, true)
                    .expect("live evidence"),
            )
            .verify(&Scope::User);
        assert!(passed.verified);
        assert!(
            passed
                .checks
                .iter()
                .any(|check| check.name == "live_initialize" && check.passed)
        );

        let failed = adapter
            .with_verification_evidence(
                CodexVerificationEvidence::live_initialize(&binary, &config_bytes, false)
                    .expect("failed live evidence"),
            )
            .verify(&Scope::User);
        assert!(!failed.verified);
        assert!(
            failed
                .checks
                .iter()
                .any(|check| check.name == "live_initialize" && !check.passed)
        );
    }
}
