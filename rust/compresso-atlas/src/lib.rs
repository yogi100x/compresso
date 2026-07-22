//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod indexer;
pub mod parsers;
pub mod queries;
pub mod scanner;

pub use indexer::{IndexError, IndexProgress, IndexReport, Indexer};
pub use queries::{
    BoundedRead, ContextPlan, ContextSymbolBody, ImpactEntry, ImpactReport, Outline, QueryError,
    RepoMap, SearchHit, SymbolIdentity, UnfoldResult, impact, outline, plan_context, read_bounded,
    repo_map, search, unfold,
};
pub use scanner::{
    AtlasError, ScanError, ScanOptions, ScanResult, SkipReason, scan, scan_with_options,
};
