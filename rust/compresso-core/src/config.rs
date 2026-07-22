use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Mode;

const CONFIG_KEYS: [&str; 14] = [
    "compression.mode",
    "compression.target_tokens",
    "compression.max_latency_ms",
    "index.enabled",
    "index.respect_gitignore",
    "index.max_file_bytes",
    "privacy.store_raw_content",
    "privacy.telemetry",
    "mcp.transport",
    "proxy.enabled",
    "proxy.bind",
    "proxy.compress_system",
    "runtime.bypass",
    "runtime.log",
];

const KNOWN_ENVIRONMENT: [&str; 10] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "APPDATA",
    "LOCALAPPDATA",
    "COMPRESSO_MODE",
    "COMPRESSO_TARGET_TOKENS",
    "COMPRESSO_TELEMETRY",
    "COMPRESSO_BYPASS",
    "COMPRESSO_LOG",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    Default,
    File,
    Env,
    Flag,
}

impl ConfigSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Env => "env",
            Self::Flag => "flag",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveValue<T> {
    pub value: T,
    pub source: ConfigSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigExplanation {
    pub key: String,
    pub value: String,
    pub source: ConfigSource,
    pub annotation: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Config {
    pub compression: CompressionCfg,
    pub index: IndexCfg,
    pub privacy: PrivacyCfg,
    pub mcp: McpCfg,
    pub proxy: ProxyCfg,
    pub runtime: RuntimeCfg,
    #[serde(skip)]
    sources: BTreeMap<&'static str, ConfigSource>,
    #[serde(skip)]
    telemetry_forced_off: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompressionCfg {
    pub mode: Mode,
    pub target_tokens: usize,
    pub max_latency_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IndexCfg {
    pub enabled: bool,
    pub respect_gitignore: bool,
    pub max_file_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrivacyCfg {
    pub store_raw_content: bool,
    pub telemetry: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpCfg {
    pub transport: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProxyCfg {
    pub enabled: bool,
    pub bind: String,
    pub compress_system: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeCfg {
    pub bypass: bool,
    pub log: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigOverrides {
    pub mode: Option<Mode>,
    pub target_tokens: Option<usize>,
    pub telemetry: Option<bool>,
    pub bypass: Option<bool>,
    pub log: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub database_file: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not resolve the Compresso configuration directory")]
    ConfigDirectoryUnavailable,
    #[error("could not resolve the Compresso data directory")]
    DataDirectoryUnavailable,
    #[error("environment variable {name} is not valid UTF-8")]
    NonUnicodeEnvironment { name: &'static str },
    #[error("failed to read configuration file {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse configuration file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid configuration value for {key}: {reason}")]
    InvalidValue {
        key: &'static str,
        reason: &'static str,
    },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            compression: CompressionCfg {
                mode: Mode::Safe,
                target_tokens: 4_000,
                max_latency_ms: 200,
            },
            index: IndexCfg {
                enabled: true,
                respect_gitignore: true,
                max_file_bytes: 1_000_000,
            },
            privacy: PrivacyCfg {
                store_raw_content: false,
                telemetry: false,
            },
            mcp: McpCfg {
                transport: "stdio".to_owned(),
            },
            proxy: ProxyCfg {
                enabled: false,
                bind: "127.0.0.1:7410".to_owned(),
                compress_system: false,
            },
            runtime: RuntimeCfg {
                bypass: false,
                log: "info".to_owned(),
            },
            sources: CONFIG_KEYS
                .into_iter()
                .map(|key| (key, ConfigSource::Default))
                .collect(),
            telemetry_forced_off: false,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with_flags(ConfigOverrides::default())
    }

    pub fn load_with_flags(flags: ConfigOverrides) -> Result<Self, ConfigError> {
        let environment = system_environment()?;
        let paths = RuntimePaths::from_environment(&environment)?;
        Self::load_from(&paths.config_file, &environment, flags)
    }

    pub fn load_from(
        path: &Path,
        environment: &BTreeMap<String, String>,
        flags: ConfigOverrides,
    ) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        if let Some(file) = read_file_config(path)? {
            config.apply_file(file)?;
        }
        config.apply_environment(environment)?;
        config.apply_flags(flags)?;
        config.privacy.telemetry = false;
        Ok(config)
    }

    pub fn explain(&self) -> Vec<(String, String, ConfigSource)> {
        self.explanation_rows()
            .into_iter()
            .map(|row| {
                let value = row.annotation.map_or(row.value.clone(), |annotation| {
                    format!("{} ({annotation})", row.value)
                });
                (row.key, value, row.source)
            })
            .collect()
    }

    pub fn explanation_rows(&self) -> Vec<ConfigExplanation> {
        CONFIG_KEYS
            .into_iter()
            .map(|key| ConfigExplanation {
                key: key.to_owned(),
                value: self.value_for(key),
                source: self.sources[key],
                annotation: (key == "privacy.telemetry" && self.telemetry_forced_off)
                    .then(|| "forced off (D6)".to_owned()),
            })
            .collect()
    }

    fn apply_file(&mut self, file: FileConfig) -> Result<(), ConfigError> {
        if let Some(compression) = file.compression {
            if let Some(value) = compression.mode {
                self.set_mode(parse_mode(&value)?, ConfigSource::File);
            }
            if let Some(value) = compression.target_tokens {
                self.set_target_tokens(value, ConfigSource::File)?;
            }
            if let Some(value) = compression.max_latency_ms {
                ensure_positive("compression.max_latency_ms", value)?;
                self.compression.max_latency_ms = value;
                self.mark("compression.max_latency_ms", ConfigSource::File);
            }
        }
        if let Some(index) = file.index {
            apply_value(self, "index.enabled", index.enabled, |config, value| {
                config.index.enabled = value
            });
            apply_value(
                self,
                "index.respect_gitignore",
                index.respect_gitignore,
                |config, value| config.index.respect_gitignore = value,
            );
            if let Some(value) = index.max_file_bytes {
                ensure_positive("index.max_file_bytes", value)?;
                self.index.max_file_bytes = value;
                self.mark("index.max_file_bytes", ConfigSource::File);
            }
        }
        if let Some(privacy) = file.privacy {
            apply_value(
                self,
                "privacy.store_raw_content",
                privacy.store_raw_content,
                |config, value| config.privacy.store_raw_content = value,
            );
            if let Some(value) = privacy.telemetry {
                self.set_telemetry(value, ConfigSource::File);
            }
        }
        if let Some(mcp) = file.mcp
            && let Some(value) = mcp.transport
        {
            if value != "stdio" {
                return Err(invalid("mcp.transport", "must be stdio in this build"));
            }
            self.mcp.transport = value;
            self.mark("mcp.transport", ConfigSource::File);
        }
        if let Some(proxy) = file.proxy {
            apply_value(self, "proxy.enabled", proxy.enabled, |config, value| {
                config.proxy.enabled = value
            });
            if let Some(value) = proxy.bind {
                ensure_nonempty("proxy.bind", &value)?;
                self.proxy.bind = value;
                self.mark("proxy.bind", ConfigSource::File);
            }
            apply_value(
                self,
                "proxy.compress_system",
                proxy.compress_system,
                |config, value| config.proxy.compress_system = value,
            );
        }
        if let Some(runtime) = file.runtime {
            if let Some(value) = runtime.bypass {
                self.runtime.bypass = value;
                self.mark("runtime.bypass", ConfigSource::File);
            }
            if let Some(value) = runtime.log {
                self.set_log(value, ConfigSource::File)?;
            }
        }
        Ok(())
    }

    fn apply_environment(
        &mut self,
        environment: &BTreeMap<String, String>,
    ) -> Result<(), ConfigError> {
        if let Some(value) = environment.get("COMPRESSO_MODE") {
            self.set_mode(parse_mode(value)?, ConfigSource::Env);
        }
        if let Some(value) = environment.get("COMPRESSO_TARGET_TOKENS") {
            self.set_target_tokens(
                value.parse().map_err(|_| {
                    invalid(
                        "compression.target_tokens",
                        "COMPRESSO_TARGET_TOKENS must be a positive integer",
                    )
                })?,
                ConfigSource::Env,
            )?;
        }
        if let Some(value) = environment.get("COMPRESSO_TELEMETRY") {
            self.set_telemetry(parse_bool("privacy.telemetry", value)?, ConfigSource::Env);
        }
        if let Some(value) = environment.get("COMPRESSO_BYPASS") {
            self.runtime.bypass = parse_bool("runtime.bypass", value)?;
            self.mark("runtime.bypass", ConfigSource::Env);
        }
        if let Some(value) = environment.get("COMPRESSO_LOG") {
            self.set_log(value.clone(), ConfigSource::Env)?;
        }
        Ok(())
    }

    fn apply_flags(&mut self, flags: ConfigOverrides) -> Result<(), ConfigError> {
        if let Some(value) = flags.mode {
            self.set_mode(value, ConfigSource::Flag);
        }
        if let Some(value) = flags.target_tokens {
            self.set_target_tokens(value, ConfigSource::Flag)?;
        }
        if let Some(value) = flags.telemetry {
            self.set_telemetry(value, ConfigSource::Flag);
        }
        if let Some(value) = flags.bypass {
            self.runtime.bypass = value;
            self.mark("runtime.bypass", ConfigSource::Flag);
        }
        if let Some(value) = flags.log {
            self.set_log(value, ConfigSource::Flag)?;
        }
        Ok(())
    }

    fn set_mode(&mut self, value: Mode, source: ConfigSource) {
        self.compression.mode = value;
        self.mark("compression.mode", source);
    }

    fn set_target_tokens(&mut self, value: usize, source: ConfigSource) -> Result<(), ConfigError> {
        ensure_positive("compression.target_tokens", value)?;
        self.compression.target_tokens = value;
        self.mark("compression.target_tokens", source);
        Ok(())
    }

    fn set_telemetry(&mut self, requested: bool, source: ConfigSource) {
        self.telemetry_forced_off = requested;
        self.privacy.telemetry = false;
        self.mark("privacy.telemetry", source);
    }

    fn set_log(&mut self, value: String, source: ConfigSource) -> Result<(), ConfigError> {
        ensure_nonempty("runtime.log", &value)?;
        self.runtime.log = value;
        self.mark("runtime.log", source);
        Ok(())
    }

    fn mark(&mut self, key: &'static str, source: ConfigSource) {
        self.sources.insert(key, source);
    }

    fn value_for(&self, key: &str) -> String {
        match key {
            "compression.mode" => mode_name(self.compression.mode).to_owned(),
            "compression.target_tokens" => self.compression.target_tokens.to_string(),
            "compression.max_latency_ms" => self.compression.max_latency_ms.to_string(),
            "index.enabled" => self.index.enabled.to_string(),
            "index.respect_gitignore" => self.index.respect_gitignore.to_string(),
            "index.max_file_bytes" => self.index.max_file_bytes.to_string(),
            "privacy.store_raw_content" => self.privacy.store_raw_content.to_string(),
            "privacy.telemetry" => self.privacy.telemetry.to_string(),
            "mcp.transport" => self.mcp.transport.clone(),
            "proxy.enabled" => self.proxy.enabled.to_string(),
            "proxy.bind" => self.proxy.bind.clone(),
            "proxy.compress_system" => self.proxy.compress_system.to_string(),
            "runtime.bypass" => self.runtime.bypass.to_string(),
            "runtime.log" => self.runtime.log.clone(),
            _ => String::new(),
        }
    }
}

impl RuntimePaths {
    pub fn system() -> Result<Self, ConfigError> {
        Self::from_environment(&system_environment()?)
    }

    pub fn from_environment(environment: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        #[cfg(not(target_os = "windows"))]
        let (config_root, data_root) = {
            let home = environment
                .get("HOME")
                .map(PathBuf::from)
                .or_else(dirs::home_dir);
            let config = nonempty_path(environment.get("XDG_CONFIG_HOME"))
                .or_else(|| home.as_ref().map(|home| home.join(".config")))
                .ok_or(ConfigError::ConfigDirectoryUnavailable)?;
            let data = nonempty_path(environment.get("XDG_DATA_HOME"))
                .or_else(|| home.map(|home| home.join(".local/share")))
                .ok_or(ConfigError::DataDirectoryUnavailable)?;
            (config, data)
        };

        #[cfg(target_os = "windows")]
        let (config_root, data_root) = {
            let config = nonempty_path(environment.get("APPDATA"))
                .or_else(dirs::config_dir)
                .ok_or(ConfigError::ConfigDirectoryUnavailable)?;
            let data = nonempty_path(environment.get("LOCALAPPDATA"))
                .or_else(dirs::data_local_dir)
                .ok_or(ConfigError::DataDirectoryUnavailable)?;
            (config, data)
        };

        let data_dir = data_root.join("compresso");
        Ok(Self {
            config_file: config_root.join("compresso/config.toml"),
            database_file: data_dir.join("compresso.db"),
            data_dir,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    compression: Option<FileCompression>,
    index: Option<FileIndex>,
    privacy: Option<FilePrivacy>,
    mcp: Option<FileMcp>,
    proxy: Option<FileProxy>,
    runtime: Option<FileRuntime>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCompression {
    mode: Option<String>,
    target_tokens: Option<usize>,
    max_latency_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIndex {
    enabled: Option<bool>,
    respect_gitignore: Option<bool>,
    max_file_bytes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePrivacy {
    store_raw_content: Option<bool>,
    telemetry: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMcp {
    transport: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProxy {
    enabled: Option<bool>,
    bind: Option<String>,
    compress_system: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRuntime {
    bypass: Option<bool>,
    log: Option<String>,
}

fn read_file_config(path: &Path) -> Result<Option<FileConfig>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(source) => toml::from_str(&source)
            .map(Some)
            .map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn system_environment() -> Result<BTreeMap<String, String>, ConfigError> {
    let mut environment = BTreeMap::new();
    for name in KNOWN_ENVIRONMENT {
        let Some(value) = std::env::var_os(name) else {
            continue;
        };
        let value = value
            .into_string()
            .map_err(|_| ConfigError::NonUnicodeEnvironment { name })?;
        environment.insert(name.to_owned(), value);
    }
    Ok(environment)
}

fn apply_value<T>(
    config: &mut Config,
    key: &'static str,
    value: Option<T>,
    apply: impl FnOnce(&mut Config, T),
) {
    if let Some(value) = value {
        apply(config, value);
        config.mark(key, ConfigSource::File);
    }
}

fn parse_mode(value: &str) -> Result<Mode, ConfigError> {
    match value {
        "safe" => Ok(Mode::Safe),
        "balanced" => Ok(Mode::Balanced),
        "aggressive" => Ok(Mode::Aggressive),
        "passthrough" => Ok(Mode::Passthrough),
        _ => Err(invalid(
            "compression.mode",
            "must be safe, balanced, aggressive, or passthrough",
        )),
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Safe => "safe",
        Mode::Balanced => "balanced",
        Mode::Aggressive => "aggressive",
        Mode::Passthrough => "passthrough",
    }
}

fn parse_bool(key: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(invalid(key, "must be a boolean")),
    }
}

fn ensure_positive<T>(key: &'static str, value: T) -> Result<(), ConfigError>
where
    T: PartialEq + From<u8>,
{
    if value == T::from(0) {
        Err(invalid(key, "must be greater than zero"))
    } else {
        Ok(())
    }
}

fn ensure_nonempty(key: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        Err(invalid(key, "must not be empty"))
    } else {
        Ok(())
    }
}

fn invalid(key: &'static str, reason: &'static str) -> ConfigError {
    ConfigError::InvalidValue { key, reason }
}

fn nonempty_path(value: Option<&String>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_precedence_env_over_file_and_flag_over_env() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[compression]\nmode = \"safe\"\ntarget_tokens = 1000\n",
        )
        .expect("configuration should be written");
        let environment = BTreeMap::from([
            ("COMPRESSO_MODE".to_owned(), "passthrough".to_owned()),
            ("COMPRESSO_TARGET_TOKENS".to_owned(), "2000".to_owned()),
        ]);

        let from_environment = Config::load_from(&path, &environment, ConfigOverrides::default())
            .expect("configuration should load");
        assert_eq!(from_environment.compression.mode, Mode::Passthrough);
        assert_eq!(
            from_environment.sources["compression.mode"],
            ConfigSource::Env
        );

        let from_flag = Config::load_from(
            &path,
            &environment,
            ConfigOverrides {
                mode: Some(Mode::Balanced),
                target_tokens: Some(3_000),
                ..ConfigOverrides::default()
            },
        )
        .expect("flag overrides should load");
        assert_eq!(from_flag.compression.mode, Mode::Balanced);
        assert_eq!(from_flag.compression.target_tokens, 3_000);
        assert_eq!(from_flag.sources["compression.mode"], ConfigSource::Flag);
    }

    #[test]
    fn test_telemetry_forced_false_and_annotated() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let environment = BTreeMap::from([("COMPRESSO_TELEMETRY".to_owned(), "true".to_owned())]);

        let config = Config::load_from(
            &directory.path().join("missing.toml"),
            &environment,
            ConfigOverrides::default(),
        )
        .expect("telemetry override should be accepted");

        assert!(!config.privacy.telemetry);
        let row = config
            .explanation_rows()
            .into_iter()
            .find(|row| row.key == "privacy.telemetry")
            .expect("telemetry row should exist");
        assert_eq!(row.value, "false");
        assert_eq!(row.source, ConfigSource::Env);
        assert_eq!(row.annotation.as_deref(), Some("forced off (D6)"));
    }

    #[test]
    fn test_malformed_config_returns_typed_parse_error() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("config.toml");
        fs::write(&path, "mode = [").expect("bad configuration should be written");

        assert!(matches!(
            Config::load_from(&path, &BTreeMap::new(), ConfigOverrides::default()),
            Err(ConfigError::Parse { path: error_path, .. }) if error_path == path
        ));
    }

    #[test]
    fn test_runtime_paths_honor_xdg_overrides() {
        let environment = BTreeMap::from([
            ("HOME".to_owned(), "/home/example".to_owned()),
            ("XDG_CONFIG_HOME".to_owned(), "/tmp/config".to_owned()),
            ("XDG_DATA_HOME".to_owned(), "/tmp/data".to_owned()),
        ]);

        let paths = RuntimePaths::from_environment(&environment).expect("paths should resolve");

        assert_eq!(
            paths.config_file,
            PathBuf::from("/tmp/config/compresso/config.toml")
        );
        assert_eq!(paths.data_dir, PathBuf::from("/tmp/data/compresso"));
        assert_eq!(
            paths.database_file,
            PathBuf::from("/tmp/data/compresso/compresso.db")
        );
    }

    #[test]
    fn test_explain_lists_every_effective_key() {
        let config = Config::default();
        let rows = config.explanation_rows();

        assert_eq!(rows.len(), CONFIG_KEYS.len());
        assert!(rows.iter().all(|row| row.source == ConfigSource::Default));
        assert_eq!(rows[0].key, "compression.mode");
    }
}
