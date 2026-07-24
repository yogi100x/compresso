use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args, ValueEnum};
use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenProvider, Trace,
    counter_for,
};
use compresso_store::{DetailedStatsSummary, OperationRecord, StatsFilter, Store};
use serde_json::{Value, json};

use super::{OutputArgs, RootCommand};

const DEFAULT_TARGET_TOKENS: usize = 4_000;
const MAX_TARGET_TOKENS: usize = 100_000;
const DEFAULT_MAX_LATENCY_MS: u64 = 200;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliCompressionMode {
    Safe,
    Passthrough,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliNodeKind {
    #[value(name = "system_instruction")]
    SystemInstruction,
    #[value(name = "user_instruction")]
    UserInstruction,
    #[value(name = "assistant_response")]
    AssistantResponse,
    #[value(name = "assistant_reasoning")]
    AssistantReasoning,
    #[value(name = "tool_definition")]
    ToolDefinition,
    #[value(name = "tool_request")]
    ToolRequest,
    #[value(name = "tool_response")]
    ToolResponse,
    #[value(name = "source_file")]
    SourceFile,
    #[value(name = "code_symbol")]
    CodeSymbol,
    #[value(name = "search_result")]
    SearchResult,
    #[value(name = "compiler_output")]
    CompilerOutput,
    #[value(name = "test_output")]
    TestOutput,
    #[value(name = "repo_map")]
    RepoMap,
    Summary,
    Decision,
    #[value(name = "memory_fact")]
    MemoryFact,
}

#[derive(Args, Debug)]
pub struct CompressArgs {
    /// Read UTF-8 input from a file instead of stdin.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "tool_response")]
    pub kind: CliNodeKind,
    #[arg(long, value_enum, default_value = "safe")]
    pub mode: CliCompressionMode,
    #[arg(long, default_value_t = DEFAULT_TARGET_TOKENS)]
    pub target_tokens: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("selector")
        .required(true)
        .multiple(false)
        .args(["operation_id", "last"])
))]
pub struct ExplainArgs {
    pub operation_id: Option<String>,
    #[arg(long)]
    pub last: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug, Default)]
pub struct StatsArgs {
    /// Maximum number of newest per-operation rows to display.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

pub(crate) struct Rendered {
    pub json: Value,
    pub human: String,
}

pub(crate) struct EngineCommandError {
    pub code: &'static str,
    pub message: String,
}

pub(crate) fn execute(
    command: RootCommand,
    store: &Store,
    input: &mut dyn Read,
) -> Result<Rendered, EngineCommandError> {
    match command {
        RootCommand::Compress(arguments) => execute_compress(store, input, arguments),
        RootCommand::Explain(arguments) => execute_explain(store, arguments),
        RootCommand::Stats(arguments) => execute_stats(store, arguments),
        _ => Err(EngineCommandError::internal()),
    }
}

fn execute_compress(
    store: &Store,
    input: &mut dyn Read,
    arguments: CompressArgs,
) -> Result<Rendered, EngineCommandError> {
    if !(1..=MAX_TARGET_TOKENS).contains(&arguments.target_tokens) {
        return Err(EngineCommandError::invalid(
            "target tokens must be between 1 and 100000",
        ));
    }
    let source = read_input(input, arguments.file.as_deref())?;
    if source.is_empty() {
        return Err(EngineCommandError::invalid(
            "compression input must not be empty",
        ));
    }

    let mode = arguments.mode.core_mode();
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let graph = ContextGraph::from_text(&source, arguments.kind.core_kind(), counter);
    let budget = CompressionBudget {
        max_tokens: arguments.target_tokens,
        max_latency_ms: DEFAULT_MAX_LATENCY_MS,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    };
    let result = Engine::new(mode, counter).compress(graph, &budget);
    let content = result
        .output
        .nodes
        .iter()
        .map(|node| node.content.as_str())
        .collect::<String>();
    if content.is_empty() {
        return Err(EngineCommandError::internal());
    }
    let trace_json =
        serde_json::to_string(&result.trace).map_err(|_| EngineCommandError::internal())?;
    store
        .record_operation(&OperationRecord {
            id: result.trace.operation_id.clone(),
            session_id: None,
            tool: "engine_compress".to_owned(),
            mode,
            observed_tokens: result.before_tokens,
            applied_tokens: result.after_tokens,
            compression_overhead: 0,
            fallback_used: result.fallback_used,
            tokens_served: None,
            retrieval: None,
            trace_json: Some(trace_json),
            created_at: result.trace.created_at,
        })
        .map_err(|_| EngineCommandError::internal())?;

    let data = json!({
        "operation_id": result.trace.operation_id,
        "content": content.clone(),
        "before_tokens": result.before_tokens,
        "after_tokens": result.after_tokens,
        "token_provider": result.trace.token_provider,
        "fallback_used": result.fallback_used,
        "fallback_reason": result.fallback_reason,
    });
    Ok(rendered(data, content))
}

fn execute_explain(store: &Store, arguments: ExplainArgs) -> Result<Rendered, EngineCommandError> {
    let operation = if arguments.last {
        store.latest_compression_operation()
    } else {
        let operation_id = arguments
            .operation_id
            .as_deref()
            .expect("clap requires an explain selector");
        if operation_id.trim().is_empty() {
            return Err(EngineCommandError::invalid(
                "operation id must not be empty",
            ));
        }
        store.operation_by_id(operation_id)
    }
    .map_err(|_| EngineCommandError::internal())?
    .ok_or_else(EngineCommandError::not_found)?;
    let trace_json = operation
        .trace_json
        .as_deref()
        .ok_or_else(EngineCommandError::not_found)?;
    let trace: Trace =
        serde_json::from_str(trace_json).map_err(|_| EngineCommandError::internal())?;
    let trace = serde_json::to_value(trace).map_err(|_| EngineCommandError::internal())?;
    let data = json!({
        "operation_id": operation.id,
        "trace": trace,
    });
    let human = serde_json::to_string_pretty(&data).map_err(|_| EngineCommandError::internal())?;
    Ok(rendered(data, format!("{human}\n")))
}

fn execute_stats(store: &Store, arguments: StatsArgs) -> Result<Rendered, EngineCommandError> {
    if !(1..=1_000).contains(&arguments.limit) {
        return Err(EngineCommandError::invalid(
            "stats limit must be between 1 and 1000",
        ));
    }
    let stats = store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: None,
            operation_limit: Some(arguments.limit),
        })
        .map_err(|_| EngineCommandError::internal())?;
    let summary = &stats.summary;
    let data = stats_json(&stats);
    let mut human = format!(
        "operations: {}\n\ncompression savings (applied):\n  observed tokens: {}\n  applied tokens: {}\n  removed tokens: {}\n  compression overhead: {}\n  net compression savings: {}\n  fallback rate: {:.2}%\n\nretrieval savings (modeled baseline):\n  retrieval pack tokens: {}\n  estimated raw-source tokens: {}\n  retrieval tokens avoided: {}\n  follow-up calls avoided: {}\n\ntotal effective savings: {} (applied compression + modeled retrieval)\nmodeled cost saved: ${:.6} ({})\n",
        summary.operations,
        summary.observed_tokens,
        summary.applied_tokens,
        summary.removed_tokens,
        summary.compression_overhead,
        summary.net_tokens_saved,
        summary.fallback_rate * 100.0,
        summary.retrieval_pack_tokens,
        summary.estimated_raw_source_tokens,
        summary.retrieval_tokens_avoided,
        summary.follow_up_calls_avoided,
        summary.total_effective_savings,
        summary.modeled_cost_saved.amount_usd,
        summary.modeled_cost_saved.label,
    );
    human.push_str("\nper repository:\n");
    if stats.by_repository.is_empty() {
        human.push_str("  none recorded\n");
    }
    for row in &stats.by_repository {
        human.push_str(&format!(
            "  {}: operations {}, pack {}, raw estimate {}, avoided {}, follow-ups {}, effective {}\n",
            row.repository,
            row.operations,
            row.retrieval_pack_tokens,
            row.estimated_raw_source_tokens,
            row.retrieval_tokens_avoided,
            row.follow_up_calls_avoided,
            row.total_effective_savings,
        ));
    }
    human.push_str(&format!(
        "\nper operation (newest {}, limit {}):\n",
        stats.by_operation.len(),
        arguments.limit
    ));
    if stats.by_operation.is_empty() {
        human.push_str("  none recorded\n");
    }
    for row in &stats.by_operation {
        human.push_str(&format!(
            "  {} {} [{}]: compression {}, pack {}, raw estimate {}, avoided {}, follow-ups {}, effective {}\n",
            row.operation_id,
            row.tool,
            row.repository.as_deref().unwrap_or("no repository"),
            row.compression_savings,
            row.retrieval_pack_tokens,
            row.estimated_raw_source_tokens,
            row.retrieval_tokens_avoided,
            row.follow_up_calls_avoided,
            row.total_effective_savings,
        ));
    }
    Ok(rendered(data, human))
}

fn read_input(input: &mut dyn Read, file: Option<&Path>) -> Result<String, EngineCommandError> {
    if let Some(file) = file {
        return fs::read_to_string(file).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                EngineCommandError::invalid("compression file must contain UTF-8 text")
            } else {
                EngineCommandError::invalid("compression file could not be read")
            }
        });
    }
    let mut source = String::new();
    input.read_to_string(&mut source).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            EngineCommandError::invalid("compression input must contain UTF-8 text")
        } else {
            EngineCommandError::internal()
        }
    })?;
    Ok(source)
}

fn stats_json(stats: &DetailedStatsSummary) -> Value {
    let summary = &stats.summary;
    json!({
        "operations": summary.operations,
        "observed_tokens": summary.observed_tokens,
        "applied_tokens": summary.applied_tokens,
        "removed_tokens": summary.removed_tokens,
        "compression_overhead": summary.compression_overhead,
        "net_tokens_saved": summary.net_tokens_saved,
        "modeled_cost_saved": {
            "amount_usd": summary.modeled_cost_saved.amount_usd,
            "label": summary.modeled_cost_saved.label,
        },
        "fallback_rate": summary.fallback_rate,
        "compression_savings": {
            "removed_tokens": summary.removed_tokens,
            "compression_overhead": summary.compression_overhead,
            "net_tokens_saved": summary.net_tokens_saved,
            "basis": "applied"
        },
        "retrieval_savings": {
            "retrieval_pack_tokens": summary.retrieval_pack_tokens,
            "estimated_raw_source_tokens": summary.estimated_raw_source_tokens,
            "retrieval_tokens_avoided": summary.retrieval_tokens_avoided,
            "follow_up_calls_avoided": summary.follow_up_calls_avoided,
            "basis": "modeled"
        },
        "total_effective_savings": summary.total_effective_savings,
        "by_tool": stats.by_tool.iter().map(|row| json!({
            "tool": row.tool,
            "operations": row.operations,
            "net_tokens_saved": row.net_tokens_saved,
            "retrieval_tokens_avoided": row.retrieval_tokens_avoided
        })).collect::<Vec<_>>(),
        "by_repository": stats.by_repository.iter().map(|row| json!({
            "repository": row.repository,
            "operations": row.operations,
            "retrieval_pack_tokens": row.retrieval_pack_tokens,
            "estimated_raw_source_tokens": row.estimated_raw_source_tokens,
            "retrieval_tokens_avoided": row.retrieval_tokens_avoided,
            "follow_up_calls_avoided": row.follow_up_calls_avoided,
            "total_effective_savings": row.total_effective_savings
        })).collect::<Vec<_>>(),
        "by_operation": stats.by_operation.iter().map(|row| json!({
            "operation_id": row.operation_id,
            "repository": row.repository,
            "tool": row.tool,
            "compression_savings": row.compression_savings,
            "retrieval_pack_tokens": row.retrieval_pack_tokens,
            "estimated_raw_source_tokens": row.estimated_raw_source_tokens,
            "retrieval_tokens_avoided": row.retrieval_tokens_avoided,
            "follow_up_calls_avoided": row.follow_up_calls_avoided,
            "total_effective_savings": row.total_effective_savings,
            "created_at": row.created_at
        })).collect::<Vec<_>>()
    })
}

fn rendered(data: Value, human: String) -> Rendered {
    Rendered {
        json: json!({ "schema_version": 1, "data": data }),
        human,
    }
}

impl CliCompressionMode {
    const fn core_mode(self) -> Mode {
        match self {
            Self::Safe => Mode::Safe,
            Self::Passthrough => Mode::Passthrough,
        }
    }
}

impl CliNodeKind {
    const fn core_kind(self) -> NodeKind {
        match self {
            Self::SystemInstruction => NodeKind::SystemInstruction,
            Self::UserInstruction => NodeKind::UserInstruction,
            Self::AssistantResponse => NodeKind::AssistantResponse,
            Self::AssistantReasoning => NodeKind::AssistantReasoning,
            Self::ToolDefinition => NodeKind::ToolDefinition,
            Self::ToolRequest => NodeKind::ToolRequest,
            Self::ToolResponse => NodeKind::ToolResponse,
            Self::SourceFile => NodeKind::SourceFile,
            Self::CodeSymbol => NodeKind::CodeSymbol,
            Self::SearchResult => NodeKind::SearchResult,
            Self::CompilerOutput => NodeKind::CompilerOutput,
            Self::TestOutput => NodeKind::TestOutput,
            Self::RepoMap => NodeKind::RepoMap,
            Self::Summary => NodeKind::Summary,
            Self::Decision => NodeKind::Decision,
            Self::MemoryFact => NodeKind::MemoryFact,
        }
    }
}

impl EngineCommandError {
    fn invalid(message: &str) -> Self {
        Self {
            code: "invalid_arguments",
            message: message.to_owned(),
        }
    }

    fn not_found() -> Self {
        Self {
            code: "not_found",
            message: "compression operation was not found".to_owned(),
        }
    }

    fn internal() -> Self {
        Self {
            code: "internal",
            message: "local compression command failed".to_owned(),
        }
    }
}
