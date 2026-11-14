use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use compresso_core::Mode;
use serde::Deserialize;

use super::adapter::{
    AdapterError, SuiteAdapter, SuiteDescriptor, TaskContext, TaskMeasurement, TaskSpec,
};

pub const SUITE_MANIFEST_FILE: &str = "suite.json";
pub const SUITE_SCHEMA_VERSION: u32 = 1;

const CANCEL_POLL: Duration = Duration::from_millis(5);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteManifest {
    schema_version: u32,
    suite: String,
    version: String,
    #[serde(default = "default_mode")]
    mode: Mode,
    tasks: Vec<ScriptedTask>,
}

fn default_mode() -> Mode {
    Mode::Balanced
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptedTask {
    id: String,
    resolved: bool,
    baseline_tokens: usize,
    compressed_tokens: usize,
    /// Attempts `1..=transient_failures` end in an adapter error.
    #[serde(default)]
    transient_failures: u32,
    /// Simulated work before the verdict; honors cancellation.
    #[serde(default)]
    delay_ms: u64,
}

/// A local suite whose task outcomes are scripted in a committed
/// `suite.json`. It performs no network or process I/O and exists to exercise
/// the campaign runner deterministically.
#[derive(Debug)]
pub struct FixtureSuiteAdapter {
    descriptor: SuiteDescriptor,
    tasks: BTreeMap<String, ScriptedTask>,
    invocations: Mutex<BTreeMap<String, u32>>,
}

impl FixtureSuiteAdapter {
    /// Loads `<directory>/suite.json`.
    pub fn load(directory: &Path) -> Result<Self, AdapterError> {
        let path = directory.join(SUITE_MANIFEST_FILE);
        let text = fs::read_to_string(&path).map_err(|error| AdapterError::Suite {
            message: format!("failed to read {}: {error}", path.display()),
        })?;
        Self::from_json(&text)
    }

    pub fn from_json(text: &str) -> Result<Self, AdapterError> {
        let invalid = |message: String| AdapterError::Suite { message };
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64);
        if version != Some(u64::from(SUITE_SCHEMA_VERSION)) {
            return Err(invalid(format!(
                "unsupported suite schema_version (expected {SUITE_SCHEMA_VERSION})"
            )));
        }
        let manifest: SuiteManifest =
            serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
        debug_assert_eq!(manifest.schema_version, SUITE_SCHEMA_VERSION);
        if !is_identifier(&manifest.suite) || manifest.version.is_empty() {
            return Err(invalid(
                "suite and version must be non-empty identifiers".to_owned(),
            ));
        }
        if manifest.tasks.is_empty() {
            return Err(invalid("suite has no tasks".to_owned()));
        }
        let mut seen = BTreeSet::new();
        for task in &manifest.tasks {
            if !is_identifier(&task.id) {
                return Err(invalid(format!("invalid task identifier {:?}", task.id)));
            }
            if !seen.insert(task.id.clone()) {
                return Err(invalid(format!("duplicate task identifier {}", task.id)));
            }
        }
        Ok(Self {
            descriptor: SuiteDescriptor {
                id: manifest.suite,
                version: manifest.version,
                mode: manifest.mode,
            },
            tasks: manifest
                .tasks
                .into_iter()
                .map(|task| (task.id.clone(), task))
                .collect(),
            invocations: Mutex::new(BTreeMap::new()),
        })
    }

    /// How many attempts this adapter instance has started per task.
    pub fn invocations(&self) -> BTreeMap<String, u32> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

impl SuiteAdapter for FixtureSuiteAdapter {
    fn descriptor(&self) -> SuiteDescriptor {
        self.descriptor.clone()
    }

    fn tasks(&self) -> Result<Vec<TaskSpec>, AdapterError> {
        Ok(self
            .tasks
            .keys()
            .map(|id| TaskSpec { id: id.clone() })
            .collect())
    }

    fn run_task(
        &self,
        task: &TaskSpec,
        context: &TaskContext,
    ) -> Result<TaskMeasurement, AdapterError> {
        let scripted = self.tasks.get(&task.id).ok_or_else(|| AdapterError::Task {
            task: task.id.clone(),
            message: "task is not part of this suite".to_owned(),
        })?;
        *self
            .invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(task.id.clone())
            .or_default() += 1;

        let deadline = Instant::now() + Duration::from_millis(scripted.delay_ms);
        loop {
            if context.is_cancelled() {
                return Err(AdapterError::Cancelled {
                    task: task.id.clone(),
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::sleep(remaining.min(CANCEL_POLL));
        }

        if context.attempt <= scripted.transient_failures {
            return Err(AdapterError::Task {
                task: task.id.clone(),
                message: format!("scripted transient failure on attempt {}", context.attempt),
            });
        }
        Ok(TaskMeasurement {
            resolved: scripted.resolved,
            baseline_tokens: scripted.baseline_tokens,
            compressed_tokens: scripted.compressed_tokens,
        })
    }
}
