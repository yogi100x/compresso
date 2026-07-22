//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod indexer;
pub mod parsers;
pub mod queries;
pub mod scanner;

pub use indexer::{IndexError, IndexProgress, IndexReport, Indexer};
pub use queries::{
    BoundedRead, ContextPlan, ContextSymbolBody, CurrentOutline, ImpactEntry, ImpactOptions,
    ImpactReport, Outline, QueryError, RepoMap, SearchHit, SymbolIdentity, UnfoldResult, impact,
    impact_with_options, outline, outline_current, plan_context, read_bounded, repo_map,
    repo_map_focused, repository_stale, search, unfold,
};
pub use scanner::{
    AtlasError, ScanError, ScanOptions, ScanResult, SkipReason, scan, scan_with_options,
};
