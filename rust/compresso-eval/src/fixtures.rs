use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use compresso_core::graph::CONTEXT_GRAPH_SCHEMA_VERSION;
use compresso_core::{
    ContextGraph, ContextNode, GraphError, Mode, NodeKind, Source, TokenProvider, counter_for,
};
use jsonschema::{Draft, Validator};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

const CONTEXT_GRAPH_SCHEMA: &str = include_str!("../../../schemas/context-graph/v1.json");
const BENCHMARK_DIRECTORY: &str = "benchmarks";
const INTEGRATION_DIRECTORY: &str = "integrations";
const CATEGORIES: [&str; 6] = [
    "adversarial",
    "code",
    "conversations",
    "debugging",
    "logs",
    "provider-payloads",
];

/// A deterministically ordered collection of validated evaluation fixtures.
#[derive(Clone, Debug)]
pub struct FixtureSet {
    pub fixtures: Vec<Fixture>,
}

/// One fully materialized fixture and its evaluation contract.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub name: String,
    pub category: FixtureCategory,
    pub description: String,
    pub eligible: bool,
    pub mode: Mode,
    pub budget: FixtureBudget,
    pub input: FixtureInput,
    pub expectations: Expectations,
    pub recall_probes: Vec<RecallProbe>,
    pub graph: ContextGraph,
    pub directory: PathBuf,
    pub manifest_path: PathBuf,
}

/// The six fixture categories defined by the corpus contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum FixtureCategory {
    Adversarial,
    Code,
    Conversations,
    Debugging,
    Logs,
    ProviderPayloads,
}

impl FixtureCategory {
    fn directory_name(self) -> &'static str {
        match self {
            Self::Adversarial => "adversarial",
            Self::Code => "code",
            Self::Conversations => "conversations",
            Self::Debugging => "debugging",
            Self::Logs => "logs",
            Self::ProviderPayloads => "provider-payloads",
        }
    }
}

/// Per-fixture execution budget.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FixtureBudget {
    pub max_tokens: usize,
    pub max_latency_ms: u64,
}

/// Validated source form for a fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FixtureInput {
    Graph { path: PathBuf },
    Assembled,
    Repository { path: PathBuf },
}

/// Assertions applied to an engine or Atlas fixture.
#[derive(Clone, Debug, PartialEq)]
pub struct Expectations {
    pub min_reduction_pct: f64,
    pub must_preserve: Vec<String>,
    pub must_not_contain: Vec<String>,
    pub must_not_contain_in_trace: Vec<String>,
    pub protected_node_ids: Vec<String>,
    pub expect_fallback: bool,
    pub atlas: Option<AtlasExpectations>,
}

/// Assertions applied only to repository-backed Atlas fixtures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtlasExpectations {
    pub min_files: usize,
    pub min_symbols: usize,
    pub required_edges: Vec<RequiredEdge>,
}

/// One required repository edge in an Atlas fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredEdge {
    pub from_path: String,
    pub to_path: String,
    pub kind: String,
}

/// A task-recall assertion evaluated after compression.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallProbe {
    #[serde(default)]
    pub question: Option<String>,
    pub answer_substring: String,
    #[serde(default)]
    pub protected: bool,
}

/// Errors produced at the fixture ingestion boundary.
#[derive(Debug, Error)]
pub enum EvalError {
    #[error("failed to read fixture path {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid fixture manifest {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("invalid fixture {path}: {message}")]
    InvalidFixture { path: PathBuf, message: String },
    #[error("invalid fixture graph {path}: {message}")]
    InvalidGraph { path: PathBuf, message: String },
    #[error("invalid fixture graph {path}: {source}")]
    GraphValidation {
        path: PathBuf,
        #[source]
        source: GraphError,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureManifest {
    name: String,
    category: FixtureCategory,
    description: String,
    eligible: bool,
    mode: Mode,
    budget: FixtureBudget,
    input: InputManifest,
    expectations: ExpectationsManifest,
    #[serde(default)]
    recall_probes: Vec<RecallProbe>,
}

#[derive(Debug)]
struct InputManifest {
    graph: Option<String>,
    repo: Option<String>,
    assembled_declared: bool,
    entries: Vec<InputEntryManifest>,
}

#[derive(Debug)]
enum InputEntryManifest {
    File(FileNodeManifest),
    Inline(InlineNodeManifest),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInputManifest {
    graph: Option<String>,
    repo: Option<String>,
    files: Option<Vec<toml::Spanned<FileNodeManifest>>>,
    nodes: Option<Vec<toml::Spanned<InlineNodeManifest>>>,
}

impl<'de> Deserialize<'de> for InputManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawInputManifest::deserialize(deserializer)?;
        let assembled_declared = raw.files.is_some() || raw.nodes.is_some();
        let mut entries = Vec::new();

        for file in raw.files.into_iter().flatten() {
            entries.push((
                file.span().start,
                InputEntryManifest::File(file.into_inner()),
            ));
        }
        for node in raw.nodes.into_iter().flatten() {
            entries.push((
                node.span().start,
                InputEntryManifest::Inline(node.into_inner()),
            ));
        }
        entries.sort_by_key(|(start, _)| *start);

        Ok(Self {
            graph: raw.graph,
            repo: raw.repo,
            assembled_declared,
            entries: entries.into_iter().map(|(_, entry)| entry).collect(),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileNodeManifest {
    path: String,
    kind: NodeKind,
    node_id: Option<String>,
    protected: Option<bool>,
    priority: Option<f32>,
    source_tool: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineNodeManifest {
    node_id: String,
    kind: NodeKind,
    protected: Option<bool>,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectationsManifest {
    min_reduction_pct: f64,
    #[serde(default)]
    must_preserve: Vec<String>,
    #[serde(default)]
    must_not_contain: Vec<String>,
    #[serde(default)]
    must_not_contain_in_trace: Vec<String>,
    #[serde(default)]
    protected_node_ids: Vec<String>,
    #[serde(default)]
    expect_fallback: bool,
    atlas: Option<AtlasExpectationsManifest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AtlasExpectationsManifest {
    min_files: usize,
    min_symbols: usize,
    #[serde(default)]
    required_edges: Vec<[String; 3]>,
}

impl From<ExpectationsManifest> for Expectations {
    fn from(value: ExpectationsManifest) -> Self {
        Self {
            min_reduction_pct: value.min_reduction_pct,
            must_preserve: value.must_preserve,
            must_not_contain: value.must_not_contain,
            must_not_contain_in_trace: value.must_not_contain_in_trace,
            protected_node_ids: value.protected_node_ids,
            expect_fallback: value.expect_fallback,
            atlas: value.atlas.map(|atlas| AtlasExpectations {
                min_files: atlas.min_files,
                min_symbols: atlas.min_symbols,
                required_edges: atlas
                    .required_edges
                    .into_iter()
                    .map(|[from_path, to_path, kind]| RequiredEdge {
                        from_path,
                        to_path,
                        kind,
                    })
                    .collect(),
            }),
        }
    }
}

impl FixtureSet {
    /// Loads and validates every `<category>/<name>/fixture.toml` under `root`.
    pub fn load(root: &Path) -> Result<Self, EvalError> {
        let validator = context_graph_validator()?;
        let mut manifest_paths = fixture_manifest_paths(root)?;
        manifest_paths.sort();

        let fixtures = manifest_paths
            .into_iter()
            .map(|manifest_path| load_fixture(&manifest_path, &validator))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { fixtures })
    }
}

fn context_graph_validator() -> Result<Validator, EvalError> {
    let schema_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/context-graph/v1.json");
    let schema: Value =
        serde_json::from_str(CONTEXT_GRAPH_SCHEMA).map_err(|error| EvalError::InvalidGraph {
            path: schema_path.clone(),
            message: error.to_string(),
        })?;
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .map_err(|error| EvalError::InvalidGraph {
            path: schema_path,
            message: error.to_string(),
        })
}

fn fixture_manifest_paths(root: &Path) -> Result<Vec<PathBuf>, EvalError> {
    let mut manifests = Vec::new();
    let category_entries = read_directory(root, root)?;
    for category_entry in category_entries {
        let file_type = category_entry
            .file_type()
            .map_err(|source| EvalError::Read {
                path: category_entry.path(),
                source,
            })?;
        if !file_type.is_dir() {
            continue;
        }

        let category_path = category_entry.path();
        let category_name = utf8_file_name(&category_path, root)?;
        if matches!(
            category_name.as_str(),
            BENCHMARK_DIRECTORY | INTEGRATION_DIRECTORY
        ) {
            continue;
        }
        if !CATEGORIES.contains(&category_name.as_str()) {
            return Err(invalid_fixture(
                &category_path,
                format!("unknown fixture category directory {category_name}"),
            ));
        }

        for fixture_entry in read_directory(&category_path, &category_path)? {
            let file_type = fixture_entry
                .file_type()
                .map_err(|source| EvalError::Read {
                    path: fixture_entry.path(),
                    source,
                })?;
            if !file_type.is_dir() {
                continue;
            }
            let manifest_path = fixture_entry.path().join("fixture.toml");
            if !manifest_path.is_file() {
                return Err(invalid_fixture(
                    &manifest_path,
                    "fixture directory is missing fixture.toml",
                ));
            }
            manifests.push(manifest_path);
        }
    }
    Ok(manifests)
}

fn read_directory(directory: &Path, error_path: &Path) -> Result<Vec<fs::DirEntry>, EvalError> {
    let entries = fs::read_dir(directory).map_err(|source| EvalError::Read {
        path: error_path.to_path_buf(),
        source,
    })?;
    entries
        .map(|entry| {
            entry.map_err(|source| EvalError::Read {
                path: error_path.to_path_buf(),
                source,
            })
        })
        .collect()
}

fn load_fixture(manifest_path: &Path, validator: &Validator) -> Result<Fixture, EvalError> {
    let source = fs::read_to_string(manifest_path).map_err(|source| EvalError::Read {
        path: manifest_path.to_path_buf(),
        source,
    })?;
    let manifest: FixtureManifest =
        toml::from_str(&source).map_err(|source| EvalError::Manifest {
            path: manifest_path.to_path_buf(),
            source: Box::new(source),
        })?;
    let directory = manifest_path.parent().ok_or_else(|| {
        invalid_fixture(manifest_path, "fixture manifest has no parent directory")
    })?;

    validate_identity(&manifest, directory, manifest_path)?;
    validate_manifest_values(&manifest, manifest_path)?;
    let expectations: Expectations = manifest.expectations.into();
    let (input, mut graph) =
        materialize_input(&manifest.input, directory, manifest_path, validator)?;
    validate_eligibility(
        manifest.eligible,
        manifest.mode,
        manifest.category,
        &input,
        &expectations,
        manifest_path,
    )?;
    enforce_protection_and_recount(&mut graph, &expectations, manifest_path)?;
    validate_materialized_graph(&graph, manifest_path, validator)?;

    Ok(Fixture {
        name: manifest.name,
        category: manifest.category,
        description: manifest.description,
        eligible: manifest.eligible,
        mode: manifest.mode,
        budget: manifest.budget,
        input,
        expectations,
        recall_probes: manifest.recall_probes,
        graph,
        directory: directory.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
    })
}

fn validate_identity(
    manifest: &FixtureManifest,
    directory: &Path,
    manifest_path: &Path,
) -> Result<(), EvalError> {
    let directory_name = utf8_file_name(directory, manifest_path)?;
    if manifest.name != directory_name {
        return Err(invalid_fixture(
            manifest_path,
            format!(
                "fixture name {:?} does not match directory {directory_name:?}",
                manifest.name
            ),
        ));
    }
    if !is_kebab_case(&manifest.name) {
        return Err(invalid_fixture(
            manifest_path,
            "fixture name must be non-empty kebab-case",
        ));
    }

    let category_directory = directory
        .parent()
        .ok_or_else(|| invalid_fixture(manifest_path, "fixture has no category directory"))?;
    let category_name = utf8_file_name(category_directory, manifest_path)?;
    if manifest.category.directory_name() != category_name {
        return Err(invalid_fixture(
            manifest_path,
            format!(
                "fixture category {:?} does not match directory {category_name:?}",
                manifest.category.directory_name()
            ),
        ));
    }
    Ok(())
}

fn validate_manifest_values(
    manifest: &FixtureManifest,
    manifest_path: &Path,
) -> Result<(), EvalError> {
    if manifest.description.trim().is_empty() {
        return Err(invalid_fixture(
            manifest_path,
            "fixture description must not be empty",
        ));
    }
    if manifest.budget.max_tokens == 0 || manifest.budget.max_latency_ms == 0 {
        return Err(invalid_fixture(
            manifest_path,
            "fixture budget values must be greater than zero",
        ));
    }
    if !matches!(manifest.mode, Mode::Safe | Mode::Passthrough) {
        return Err(invalid_fixture(
            manifest_path,
            "balanced and aggressive fixture modes are reserved outside the current slice",
        ));
    }
    let reduction = manifest.expectations.min_reduction_pct;
    if !reduction.is_finite() || !(0.0..=100.0).contains(&reduction) {
        return Err(invalid_fixture(
            manifest_path,
            "expectations.min_reduction_pct must be between 0 and 100",
        ));
    }
    for probe in &manifest.recall_probes {
        if probe.answer_substring.is_empty() {
            return Err(invalid_fixture(
                manifest_path,
                "recall probe answer_substring must not be empty",
            ));
        }
    }
    Ok(())
}

fn materialize_input(
    input: &InputManifest,
    directory: &Path,
    manifest_path: &Path,
    validator: &Validator,
) -> Result<(FixtureInput, ContextGraph), EvalError> {
    let form_count = usize::from(input.graph.is_some())
        + usize::from(input.repo.is_some())
        + usize::from(input.assembled_declared);
    if form_count != 1 {
        return Err(invalid_fixture(
            manifest_path,
            "fixture must declare exactly one input form: graph, files-or-nodes, or repo",
        ));
    }

    if let Some(relative) = &input.graph {
        let path = validated_input_path(directory, relative, InputPathKind::File, manifest_path)?;
        let graph = load_graph(&path, validator)?;
        return Ok((FixtureInput::Graph { path }, graph));
    }
    if let Some(relative) = &input.repo {
        let path =
            validated_input_path(directory, relative, InputPathKind::Directory, manifest_path)?;
        return Ok((
            FixtureInput::Repository { path },
            ContextGraph {
                schema_version: CONTEXT_GRAPH_SCHEMA_VERSION,
                nodes: Vec::new(),
                edges: Vec::new(),
            },
        ));
    }

    let mut nodes = Vec::new();
    for entry in &input.entries {
        match entry {
            InputEntryManifest::File(file) => {
                nodes.push(materialize_file_node(file, directory, manifest_path)?);
            }
            InputEntryManifest::Inline(node) => nodes.push(materialize_inline_node(node)),
        }
    }
    if nodes.is_empty() {
        return Err(invalid_fixture(
            manifest_path,
            "files-or-nodes input form must contain at least one node",
        ));
    }
    Ok((
        FixtureInput::Assembled,
        ContextGraph {
            schema_version: CONTEXT_GRAPH_SCHEMA_VERSION,
            nodes,
            edges: Vec::new(),
        },
    ))
}

fn load_graph(path: &Path, validator: &Validator) -> Result<ContextGraph, EvalError> {
    let source = fs::read_to_string(path).map_err(|source| EvalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let value: Value = serde_json::from_str(&source).map_err(|error| EvalError::InvalidGraph {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    validator
        .validate(&value)
        .map_err(|error| EvalError::InvalidGraph {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    serde_json::from_value(value).map_err(|error| EvalError::InvalidGraph {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn materialize_inline_node(node: &InlineNodeManifest) -> ContextNode {
    let provider = TokenProvider::OpenAiO200k;
    ContextNode {
        id: node.node_id.clone(),
        kind: node.kind,
        content: node.content.clone(),
        source: Source::Synthetic,
        token_count: 0,
        token_provider: provider,
        priority: 0.5,
        protected: node
            .protected
            .unwrap_or_else(|| node.kind.requires_protection()),
        recency: None,
        metadata: Map::new(),
        provider_turn: None,
    }
}

fn materialize_file_node(
    file: &FileNodeManifest,
    directory: &Path,
    manifest_path: &Path,
) -> Result<ContextNode, EvalError> {
    let path = validated_input_path(directory, &file.path, InputPathKind::File, manifest_path)?;
    let content = fs::read_to_string(&path).map_err(|source| EvalError::Read {
        path: path.clone(),
        source,
    })?;
    let priority = file.priority.unwrap_or(0.5);
    if !priority.is_finite() || !(0.0..=1.0).contains(&priority) {
        return Err(invalid_fixture(
            manifest_path,
            format!(
                "input file {:?} priority must be between 0 and 1",
                file.path
            ),
        ));
    }
    if file.kind == NodeKind::SourceFile && file.source_tool.is_some() {
        return Err(invalid_fixture(
            manifest_path,
            format!(
                "source_file input {:?} cannot declare source_tool",
                file.path
            ),
        ));
    }

    let source = if file.kind == NodeKind::SourceFile {
        Source::Repository {
            path: file.path.clone(),
        }
    } else {
        Source::Tool {
            name: file
                .source_tool
                .clone()
                .unwrap_or_else(|| "fixture".to_owned()),
        }
    };
    Ok(ContextNode {
        id: file.node_id.clone().unwrap_or_else(|| file.path.clone()),
        kind: file.kind,
        content,
        source,
        token_count: 0,
        token_provider: TokenProvider::OpenAiO200k,
        priority,
        protected: file
            .protected
            .unwrap_or_else(|| file.kind.requires_protection()),
        recency: None,
        metadata: Map::new(),
        provider_turn: None,
    })
}

fn validate_eligibility(
    eligible: bool,
    mode: Mode,
    category: FixtureCategory,
    input: &FixtureInput,
    expectations: &Expectations,
    manifest_path: &Path,
) -> Result<(), EvalError> {
    let is_repository = matches!(input, FixtureInput::Repository { .. });
    if is_repository {
        if category != FixtureCategory::Code {
            return Err(invalid_fixture(
                manifest_path,
                "repo input is only valid for code fixtures",
            ));
        }
        if expectations.atlas.is_none() {
            return Err(invalid_fixture(
                manifest_path,
                "repo input requires expectations.atlas",
            ));
        }
    } else if expectations.atlas.is_some() {
        return Err(invalid_fixture(
            manifest_path,
            "expectations.atlas requires repo input",
        ));
    }

    let excluded_category = matches!(
        category,
        FixtureCategory::Adversarial | FixtureCategory::ProviderPayloads
    );
    if eligible
        && (mode != Mode::Safe
            || expectations.expect_fallback
            || expectations.min_reduction_pct <= 0.0
            || excluded_category
            || is_repository)
    {
        return Err(invalid_fixture(
            manifest_path,
            "fixture violates §4 eligibility rules: eligible fixtures require safe mode, no expected fallback, positive reduction, a compression input, and a non-adversarial/non-provider category",
        ));
    }
    Ok(())
}

fn enforce_protection_and_recount(
    graph: &mut ContextGraph,
    expectations: &Expectations,
    manifest_path: &Path,
) -> Result<(), EvalError> {
    graph.force_ingress_protection();

    let mut expected_ids = HashSet::new();
    for id in &expectations.protected_node_ids {
        if !expected_ids.insert(id.as_str()) {
            return Err(invalid_fixture(
                manifest_path,
                format!("duplicate protected_node_id {id:?}"),
            ));
        }
        let node = graph.nodes.iter_mut().find(|node| node.id == *id);
        let Some(node) = node else {
            return Err(invalid_fixture(
                manifest_path,
                format!("protected_node_id {id:?} does not exist in the fixture graph"),
            ));
        };
        node.protected = true;
    }

    for node in &mut graph.nodes {
        let counter = counter_for(node.token_provider);
        node.token_count = counter.count(&node.content);
        node.token_provider = counter.provider();
    }
    Ok(())
}

fn validate_materialized_graph(
    graph: &ContextGraph,
    manifest_path: &Path,
    validator: &Validator,
) -> Result<(), EvalError> {
    graph
        .validate()
        .map_err(|source| EvalError::GraphValidation {
            path: manifest_path.to_path_buf(),
            source,
        })?;
    let value = serde_json::to_value(graph).map_err(|error| EvalError::InvalidGraph {
        path: manifest_path.to_path_buf(),
        message: error.to_string(),
    })?;
    validator
        .validate(&value)
        .map_err(|error| EvalError::InvalidGraph {
            path: manifest_path.to_path_buf(),
            message: error.to_string(),
        })
}

#[derive(Clone, Copy)]
enum InputPathKind {
    File,
    Directory,
}

fn validated_input_path(
    directory: &Path,
    relative: &str,
    kind: InputPathKind,
    manifest_path: &Path,
) -> Result<PathBuf, EvalError> {
    let relative_path = Path::new(relative);
    let components_are_normal = relative_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if relative.is_empty()
        || relative.contains('\\')
        || has_windows_drive_prefix(relative)
        || relative_path.is_absolute()
        || !components_are_normal
    {
        return Err(invalid_fixture(
            manifest_path,
            format!("input path {relative:?} must be a fixture-relative forward-slash path"),
        ));
    }

    let fixture_root = fs::canonicalize(directory).map_err(|source| EvalError::Read {
        path: directory.to_path_buf(),
        source,
    })?;
    let requested = directory.join(relative_path);
    let canonical = fs::canonicalize(&requested).map_err(|source| EvalError::Read {
        path: requested,
        source,
    })?;
    if !canonical.starts_with(&fixture_root) {
        return Err(invalid_fixture(
            manifest_path,
            format!("input path {relative:?} escapes the fixture directory"),
        ));
    }
    let has_expected_type = match kind {
        InputPathKind::File => canonical.is_file(),
        InputPathKind::Directory => canonical.is_dir(),
    };
    if !has_expected_type {
        return Err(invalid_fixture(
            manifest_path,
            format!("input path {relative:?} has the wrong file type"),
        ));
    }
    Ok(canonical)
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn utf8_file_name(path: &Path, error_path: &Path) -> Result<String, EvalError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid_fixture(error_path, "fixture paths must be valid UTF-8"))
}

fn is_kebab_case(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn invalid_fixture(path: &Path, message: impl Into<String>) -> EvalError {
    EvalError::InvalidFixture {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use compresso_core::{TokenProvider, counter_for};
    use tempfile::TempDir;

    use super::*;

    fn committed_fixtures_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
    }

    fn fixture_dir(root: &Path, category: &str, name: &str) -> PathBuf {
        let directory = root.join(category).join(name);
        fs::create_dir_all(&directory).expect("fixture directory should be created");
        directory
    }

    fn write_manifest(directory: &Path, source: &str) {
        fs::write(directory.join("fixture.toml"), source)
            .expect("fixture manifest should be written");
    }

    fn basic_manifest(name: &str, category: &str) -> String {
        format!(
            r#"name = "{name}"
category = "{category}"
description = "Synthetic loader test fixture."
eligible = false
mode = "safe"

[budget]
max_tokens = 100
max_latency_ms = 200

[[input.nodes]]
node_id = "node-1"
kind = "summary"
content = "fixture content"

[expectations]
min_reduction_pct = 0.0
"#,
        )
    }

    fn assembled_manifest(name: &str, entries: &str) -> String {
        format!(
            r#"name = "{name}"
category = "logs"
description = "Exercises ordered assembled input."
eligible = false
mode = "safe"

[budget]
max_tokens = 100
max_latency_ms = 200

{entries}

[expectations]
min_reduction_pct = 0.0
"#,
        )
    }

    fn manifest_with_file_path(name: &str, path: &str) -> String {
        assembled_manifest(
            name,
            &format!(
                r#"[[input.files]]
path = "{path}"
kind = "summary"
node_id = "file-node"
"#,
            ),
        )
    }

    #[test]
    fn test_load_all_committed_fixtures() {
        let set = FixtureSet::load(&committed_fixtures_root())
            .expect("all committed fixtures should load");

        assert!(set.fixtures.len() >= 6);
        let categories = set
            .fixtures
            .iter()
            .map(|fixture| fixture.category)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            categories,
            BTreeSet::from([
                FixtureCategory::Adversarial,
                FixtureCategory::Code,
                FixtureCategory::Conversations,
                FixtureCategory::Debugging,
                FixtureCategory::Logs,
                FixtureCategory::ProviderPayloads,
            ])
        );
    }

    #[test]
    fn test_only_named_non_quality_directories_are_skipped() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        fs::create_dir(temporary.path().join(INTEGRATION_DIRECTORY))
            .expect("integration fixture directory should be created");
        fs::write(
            temporary
                .path()
                .join(INTEGRATION_DIRECTORY)
                .join("client.json"),
            "{}\n",
        )
        .expect("integration test data should be written");

        let set = FixtureSet::load(temporary.path())
            .expect("named non-quality integration data should be skipped");
        assert!(set.fixtures.is_empty());

        let unknown = temporary.path().join("unregistered-data");
        fs::create_dir(&unknown).expect("unknown directory should be created");
        let error = FixtureSet::load(temporary.path())
            .expect_err("unregistered top-level directories must still fail");
        assert!(error.to_string().contains("unknown fixture category"));
        assert!(error.to_string().contains(&unknown.display().to_string()));
    }

    #[test]
    fn test_assembled_file_before_node_preserves_declaration_order() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "file-before-node");
        fs::write(directory.join("input.txt"), "file content")
            .expect("input file should be written");
        write_manifest(
            &directory,
            &assembled_manifest(
                "file-before-node",
                r#"[[input.files]]
path = "input.txt"
kind = "summary"
node_id = "file-node"

[[input.nodes]]
node_id = "inline-node"
kind = "summary"
content = "inline content"
"#,
            ),
        );

        let set = FixtureSet::load(temporary.path()).expect("fixture should load");
        let ids = set.fixtures[0]
            .graph
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["file-node", "inline-node"]);
    }

    #[test]
    fn test_assembled_node_before_file_preserves_declaration_order() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "node-before-file");
        fs::write(directory.join("input.txt"), "file content")
            .expect("input file should be written");
        write_manifest(
            &directory,
            &assembled_manifest(
                "node-before-file",
                r#"[[input.nodes]]
node_id = "inline-node"
kind = "summary"
content = "inline content"

[[input.files]]
path = "input.txt"
kind = "summary"
node_id = "file-node"
"#,
            ),
        );

        let set = FixtureSet::load(temporary.path()).expect("fixture should load");
        let ids = set.fixtures[0]
            .graph
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["inline-node", "file-node"]);
    }

    #[test]
    fn test_input_paths_reject_absolute_parent_and_windows_drive_prefixes() {
        for (name, path) in [
            ("absolute-path", "/outside.txt"),
            ("parent-path", "../outside.txt"),
            ("windows-drive-path", "C:/outside.txt"),
        ] {
            let temporary = TempDir::new().expect("temporary directory should be created");
            let directory = fixture_dir(temporary.path(), "logs", name);
            write_manifest(&directory, &manifest_with_file_path(name, path));

            let error = FixtureSet::load(temporary.path())
                .expect_err("unsafe input path should be rejected");
            assert!(matches!(
                error,
                EvalError::InvalidFixture { message, .. }
                    if message.contains("fixture-relative forward-slash path")
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_input_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "symlink-escape");
        let outside = temporary.path().join("outside.txt");
        fs::write(&outside, "outside fixture data").expect("outside file should be written");
        symlink(&outside, directory.join("input.txt")).expect("symlink should be created");
        write_manifest(
            &directory,
            &manifest_with_file_path("symlink-escape", "input.txt"),
        );

        let error =
            FixtureSet::load(temporary.path()).expect_err("symlink escape should be rejected");
        assert!(matches!(
            error,
            EvalError::InvalidFixture { message, .. } if message.contains("escapes")
        ));
    }

    #[test]
    fn test_authoritative_fixture_toml_example_is_structurally_valid() {
        let docs = include_str!("../../../docs/plan/06-fixtures.md");
        let schema_section = docs
            .split_once("## 2. `fixture.toml` schema")
            .expect("fixture docs should contain the schema section")
            .1;
        let example = schema_section
            .split_once("```toml\n")
            .expect("fixture schema should contain a TOML block")
            .1
            .split_once("\n```")
            .expect("fixture TOML block should be closed")
            .0;

        let manifest: FixtureManifest =
            toml::from_str(example).expect("authoritative fixture example should parse strictly");
        assert!(!manifest.expectations.expect_fallback);
        assert_eq!(manifest.recall_probes.len(), 1);
    }

    #[test]
    fn test_malformed_fixture_rejected() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "missing-expectations");
        write_manifest(
            &directory,
            r#"name = "missing-expectations"
category = "logs"
description = "Missing the required expectations table."
eligible = false
mode = "safe"

[budget]
max_tokens = 100
max_latency_ms = 200

[[input.nodes]]
node_id = "node-1"
kind = "summary"
content = "fixture content"
"#,
        );

        let error = FixtureSet::load(temporary.path())
            .expect_err("missing expectations should be rejected")
            .to_string();
        assert!(error.contains(&directory.join("fixture.toml").display().to_string()));
        assert!(error.contains("expectations"));
    }

    #[test]
    fn test_eligibility_parsed() {
        let set = FixtureSet::load(&committed_fixtures_root())
            .expect("all committed fixtures should load");

        let adversarial = set
            .fixtures
            .iter()
            .find(|fixture| fixture.name == "prompt-injection-repo")
            .expect("adversarial fixture should exist");
        let log = set
            .fixtures
            .iter()
            .find(|fixture| fixture.name == "repeated-npm-install")
            .expect("log fixture should exist");
        assert!(!adversarial.eligible);
        assert!(log.eligible);
    }

    #[test]
    fn test_recall_probes_parsed() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "recall-probes");
        let mut manifest = basic_manifest("recall-probes", "logs");
        manifest.push_str(
            r#"
[[recall_probes]]
question = "Which value must survive?"
answer_substring = "fixture content"

[[recall_probes]]
answer_substring = "protected fact"
protected = true
"#,
        );
        write_manifest(&directory, &manifest);

        let set = FixtureSet::load(temporary.path()).expect("recall probes should parse");
        let probes = &set.fixtures[0].recall_probes;
        assert_eq!(probes.len(), 2);
        assert_eq!(
            probes[0].question.as_deref(),
            Some("Which value must survive?")
        );
        assert_eq!(probes[0].answer_substring, "fixture content");
        assert!(!probes[0].protected);
        assert_eq!(probes[1].question, None);
        assert_eq!(probes[1].answer_substring, "protected fact");
        assert!(probes[1].protected);
    }

    #[test]
    fn test_eligibility_rule_violation_rejected() {
        let cases = [
            ("passthrough", "logs", "mode = \"passthrough\"", false),
            ("fallback", "logs", "mode = \"safe\"", true),
            ("zero-reduction", "logs", "mode = \"safe\"", false),
            (
                "adversarial-category",
                "adversarial",
                "mode = \"safe\"",
                false,
            ),
            (
                "provider-category",
                "provider-payloads",
                "mode = \"safe\"",
                false,
            ),
        ];

        for (name, category, mode, expect_fallback) in cases {
            let temporary = TempDir::new().expect("temporary directory should be created");
            let directory = fixture_dir(temporary.path(), category, name);
            let reduction = if name == "zero-reduction" { 0.0 } else { 10.0 };
            let mut manifest = basic_manifest(name, category)
                .replace("eligible = false", "eligible = true")
                .replace("mode = \"safe\"", mode)
                .replace(
                    "min_reduction_pct = 0.0",
                    &format!("min_reduction_pct = {reduction:.1}"),
                );
            if expect_fallback {
                manifest.push_str("expect_fallback = true\n");
            }
            write_manifest(&directory, &manifest);

            let error = FixtureSet::load(temporary.path())
                .expect_err("invalid eligibility should be rejected")
                .to_string();
            assert!(error.contains("§4 eligibility rules"));
            assert!(error.contains(&directory.join("fixture.toml").display().to_string()));
        }

        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "code", "eligible-atlas");
        fs::create_dir(directory.join("repo")).expect("repository input should be created");
        write_manifest(
            &directory,
            r#"name = "eligible-atlas"
category = "code"
description = "Atlas fixtures cannot enter the reduction gate."
eligible = true
mode = "safe"

[budget]
max_tokens = 100
max_latency_ms = 200

[input]
repo = "repo"

[expectations]
min_reduction_pct = 10.0

[expectations.atlas]
min_files = 1
min_symbols = 1
"#,
        );
        let error = FixtureSet::load(temporary.path())
            .expect_err("eligible Atlas fixtures should be rejected")
            .to_string();
        assert!(error.contains("§4 eligibility rules"));
    }

    #[test]
    fn test_reserved_fixture_modes_rejected() {
        for mode in ["balanced", "aggressive"] {
            let temporary = TempDir::new().expect("temporary directory should be created");
            let directory = fixture_dir(temporary.path(), "logs", "reserved-mode");
            write_manifest(
                &directory,
                &basic_manifest("reserved-mode", "logs")
                    .replace("mode = \"safe\"", &format!("mode = \"{mode}\"")),
            );

            let error = FixtureSet::load(temporary.path())
                .expect_err("reserved fixture modes should be rejected")
                .to_string();
            assert!(error.contains("reserved"));
            assert!(error.contains(&directory.join("fixture.toml").display().to_string()));
        }
    }

    #[test]
    fn test_fixture_graph_validates() {
        let set = FixtureSet::load(&committed_fixtures_root())
            .expect("all committed fixtures should load");

        for fixture in set.fixtures {
            fixture
                .graph
                .validate()
                .unwrap_or_else(|error| panic!("{} graph should validate: {error}", fixture.name));
        }
    }

    #[test]
    fn test_unknown_toml_key_rejected() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "unknown-key");
        write_manifest(
            &directory,
            &basic_manifest("unknown-key", "logs").replace(
                "max_latency_ms = 200",
                "max_latency_ms = 200\nunsupported_budget_key = true",
            ),
        );

        let error = FixtureSet::load(temporary.path())
            .expect_err("unknown keys should be rejected")
            .to_string();
        assert!(error.contains("unsupported_budget_key"));
        assert!(error.contains(&directory.join("fixture.toml").display().to_string()));
    }

    #[test]
    fn test_multiple_input_forms_rejected() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "conversations", "multiple-inputs");
        fs::write(
            directory.join("graph.json"),
            r#"{"schema_version":1,"nodes":[],"edges":[]}"#,
        )
        .expect("graph fixture should be written");
        write_manifest(
            &directory,
            &basic_manifest("multiple-inputs", "conversations").replace(
                "[[input.nodes]]",
                "[input]\ngraph = \"graph.json\"\n\n[[input.nodes]]",
            ),
        );

        let error = FixtureSet::load(temporary.path())
            .expect_err("multiple input forms should be rejected")
            .to_string();
        assert!(error.contains("exactly one input form"));
    }

    #[test]
    fn test_name_and_category_directory_mismatches_rejected() {
        for (declared_name, declared_category, expected) in [
            ("different-name", "logs", "name"),
            ("matching-name", "debugging", "category"),
        ] {
            let temporary = TempDir::new().expect("temporary directory should be created");
            let directory = fixture_dir(temporary.path(), "logs", "matching-name");
            write_manifest(
                &directory,
                &basic_manifest(declared_name, declared_category),
            );

            let error = FixtureSet::load(temporary.path())
                .expect_err("directory mismatches should be rejected")
                .to_string();
            assert!(error.contains(expected));
            assert!(error.contains(&directory.join("fixture.toml").display().to_string()));
        }
    }

    #[test]
    fn test_graph_counts_recomputed_and_protection_enforced() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "conversations", "graph-boundary");
        fs::write(
            directory.join("graph.json"),
            r#"{
  "schema_version": 1,
  "nodes": [{
    "id": "sys-1",
    "kind": "system_instruction",
    "content": "Count this exact content.",
    "source": {"type": "client"},
    "token_count": 999,
    "priority": 1.0,
    "protected": false
  }],
  "edges": []
}"#,
        )
        .expect("graph fixture should be written");
        write_manifest(
            &directory,
            r#"name = "graph-boundary"
category = "conversations"
description = "Exercises graph ingress validation."
eligible = false
mode = "safe"

[budget]
max_tokens = 100
max_latency_ms = 200

[input]
graph = "graph.json"

[expectations]
min_reduction_pct = 0.0
"#,
        );

        let set = FixtureSet::load(temporary.path()).expect("valid graph should load");
        let node = &set.fixtures[0].graph.nodes[0];
        let counter = counter_for(TokenProvider::OpenAiO200k);
        assert_eq!(node.token_count, counter.count(&node.content));
        assert_eq!(node.token_provider, counter.provider());
        assert!(node.protected);
    }

    #[test]
    fn test_atlas_expectations_and_repository_form_parsed() {
        let set = FixtureSet::load(&committed_fixtures_root())
            .expect("all committed fixtures should load");
        let fixture = set
            .fixtures
            .iter()
            .find(|fixture| fixture.name == "mini-monorepo")
            .expect("atlas fixture should exist");

        assert!(matches!(fixture.input, FixtureInput::Repository { .. }));
        let atlas = fixture
            .expectations
            .atlas
            .as_ref()
            .expect("atlas expectations should be exposed");
        assert_eq!(atlas.min_files, 5);
        assert_eq!(atlas.min_symbols, 10);
        assert_eq!(atlas.required_edges.len(), 2);
        assert_eq!(atlas.required_edges[0].kind, "imports");
    }

    #[test]
    fn test_full_expectations_parsed() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "full-expectations");
        write_manifest(
            &directory,
            r#"name = "full-expectations"
category = "logs"
description = "Exercises every compression expectation field."
eligible = false
mode = "safe"

[budget]
max_tokens = 100
max_latency_ms = 200

[[input.nodes]]
node_id = "node-1"
kind = "summary"
content = "keep this"

[expectations]
min_reduction_pct = 12.5
must_preserve = ["keep"]
must_not_contain = ["drop"]
must_not_contain_in_trace = ["secret"]
protected_node_ids = ["node-1"]
expect_fallback = true
"#,
        );

        let set = FixtureSet::load(temporary.path()).expect("expectations should parse");
        let fixture = &set.fixtures[0];
        assert_eq!(fixture.expectations.min_reduction_pct, 12.5);
        assert_eq!(fixture.expectations.must_preserve, ["keep"]);
        assert_eq!(fixture.expectations.must_not_contain, ["drop"]);
        assert_eq!(fixture.expectations.must_not_contain_in_trace, ["secret"]);
        assert_eq!(fixture.expectations.protected_node_ids, ["node-1"]);
        assert!(fixture.expectations.expect_fallback);
        assert!(fixture.graph.nodes[0].protected);
    }

    #[test]
    fn test_recall_probe_requires_answer_substring() {
        let temporary = TempDir::new().expect("temporary directory should be created");
        let directory = fixture_dir(temporary.path(), "logs", "bad-recall-probe");
        let mut manifest = basic_manifest("bad-recall-probe", "logs");
        manifest.push_str(
            r#"
[[recall_probes]]
question = "This probe has no answer."
"#,
        );
        write_manifest(&directory, &manifest);

        let error = FixtureSet::load(temporary.path())
            .expect_err("answer_substring is required")
            .to_string();
        assert!(error.contains("answer_substring"));
        assert!(error.contains(&directory.join("fixture.toml").display().to_string()));
    }
}
