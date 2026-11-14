//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod concurrency;
pub mod indexer;
pub mod parsers;
pub mod queries;
pub mod scanner;
pub mod search;

pub use concurrency::{Concurrency, DEFAULT_MEMORY_BUDGET_BYTES};
pub use indexer::{IndexError, IndexProgress, IndexReport, Indexer};
pub use queries::{
    BoundedRead, ContextPlan, ContextSymbolBody, CurrentOutline, ImpactEntry, ImpactOptions,
    ImpactReport, Outline, QueryError, RepoMap, SearchHit, SymbolIdentity, UnfoldResult, impact,
    impact_with_options, outline, outline_current, plan_context, read_bounded, repo_map,
    repo_map_focused, repository_stale, search, unfold,
};
pub use scanner::{
    AtlasError, ScanError, ScanOptions, ScanResult, SkipReason, is_scanner_excluded, scan,
    scan_with_concurrency, scan_with_options,
};
pub use search::{
    SEARCH_MIN_QUERY_CHARS, SearchDiagnostic, SearchDiagnosticCode, SearchOptions, SearchReport,
    search_with_diagnostics, validate_search_query,
};
