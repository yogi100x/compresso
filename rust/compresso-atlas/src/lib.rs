//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod cards;
pub mod concurrency;
pub mod grep;
pub mod indexer;
pub mod parsers;
pub mod queries;
mod rust_imports;
pub mod scanner;
pub mod search;
mod task_identifiers;
pub mod trace;

pub use cards::{
    CardOptions, CardSymbol, DEFAULT_MAX_CARDS, MAX_CARDS, ModuleCard, ModuleCards, module_cards,
};
pub use concurrency::{Concurrency, DEFAULT_MEMORY_BUDGET_BYTES};
pub use grep::{
    GREP_MAX_CONTEXT_LINES, GREP_MAX_FILES_LIMIT, GREP_MAX_MATCHES_PER_FILE_LIMIT,
    GREP_MAX_PATTERN_BYTES, GREP_MAX_PATTERNS, GREP_REGEX_MAX_BYTES_PER_PATTERN, GrepContextLine,
    GrepError, GrepFile, GrepLine, GrepOptions, GrepPatternResult, GrepReport, grep, validate_grep,
};
pub use indexer::{IndexError, IndexProgress, IndexReport, Indexer};
pub use queries::{
    BoundedRead, ContextPlan, ContextSymbolBody, CurrentOutline, EXACT_TOKEN_COUNT_MAX_BYTES,
    ImpactEntry, ImpactOptions, ImpactReport, MAX_READ_FILE_BYTES, Outline, QueryError, RepoMap,
    SearchHit, SymbolIdentity, UnfoldResult, file_line_count, impact, impact_with_options, outline,
    outline_current, plan_context, read_bounded, repo_map, repo_map_focused, repository_stale,
    search, unfold,
};
pub use scanner::{
    AtlasError, ScanError, ScanOptions, ScanResult, SkipReason, is_scanner_excluded, scan,
    scan_with_concurrency, scan_with_options,
};
pub use search::{
    SEARCH_MIN_QUERY_CHARS, SearchDiagnostic, SearchDiagnosticCode, SearchOptions, SearchReport,
    search_with_diagnostics, validate_search_query,
};
pub use trace::{
    CallResolution, TRACE_DEFAULT_NEIGHBOR_DEPTH, TRACE_DEFAULT_PATH_DEPTH,
    TRACE_MAX_CALL_SITE_ROWS, TRACE_MAX_DEPTH, TraceCallSite, TraceEnd, TraceHop, TraceNeighbor,
    TraceOptions, TraceReport, TraceResult, TraceSymbol, UnfollowedCall, trace,
};
