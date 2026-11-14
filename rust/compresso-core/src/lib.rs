//! Deterministic context graph, token accounting, compression, and safety primitives.

pub mod config;
pub mod engine;
pub mod graph;
pub mod pricing;
pub mod provider_conversation;
pub mod safety;
pub mod segment;
pub mod strategies;
pub mod strategy;
pub mod tokens;
pub mod trace;
pub mod transcript;

pub use config::{
    CompressionCfg, Config, ConfigError, ConfigExplanation, ConfigOverrides, ConfigSource,
    EffectiveValue, IndexCfg, McpCfg, PrivacyCfg, ProxyCfg, RuntimeCfg, RuntimePaths,
};
pub use engine::{CompressionResult, Engine, MAX_COMPRESSION_INPUT_BYTES};
pub use graph::{
    ContextEdge, ContextGraph, ContextNode, EdgeKind, GraphError, NodeKind, ProviderId,
    ProviderIdentifierError, ProviderTurn, ProviderTurnId, Source, ToolCallId,
};
pub use pricing::{
    CostBasis, MAX_MODEL_ID_BYTES, ModelPrice, PRICE_TABLE_SCHEMA_VERSION, PriceTable,
    PriceTableError, SAVINGS_RATE_APPLIED, is_plain_model_id,
};
pub use provider_conversation::{
    DefaultProviderConversationValidator, ProviderConversationError, ProviderConversationValidator,
};
pub use safety::{SafetyCheck, run_all, run_all_with_removals};
pub use segment::{SegmentError, segment_graph};
pub use strategies::{CollapseLogs, CompactJson, DedupExact, PackBudget};
pub use strategy::{
    CompressionBudget, CompressionStrategy, Estimate, Lossiness, Mode, RemovalKind, RemovalRecord,
    StrategyError, StrategyExecution, StrategyOutput, StrategyRegistry,
};
pub use tokens::{
    CALIBRATION, TokenCounter, TokenProvider, TokenizerInitialization, anthropic_calibrated_count,
    counter_for, initialize_tokenizers,
};
pub use trace::{OperationIdentity, StrategyResult, Trace, operation_identity};
pub use transcript::{
    COMPRESSO_TOOL_NAMES, LedgerOperationSavings, MAX_TRANSCRIPT_TOOL_CALLS, ModelSavings,
    TranscriptCall, TranscriptCallKind, TranscriptSavings, TranscriptScan, TranscriptScanner,
    UNKNOWN_MODEL, scan_transcript, summarize_transcript,
};
