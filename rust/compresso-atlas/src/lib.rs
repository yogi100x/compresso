//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod cards;
pub mod concurrency;
pub mod indexer;
pub mod parsers;
pub mod queries;
mod rust_imports;
pub mod scanner;
pub mod search;

pub use cards::{
    CardOptions, CardSymbol, DEFAULT_MAX_CARDS, MAX_CARDS, ModuleCard, ModuleCards, module_cards,
};
pub use concurrency::{Concurrency, DEFAULT_MEMORY_BUDGET_BYTES};
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
