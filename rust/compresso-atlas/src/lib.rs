//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod indexer;
pub mod parsers;
pub mod queries;
pub mod scanner;

pub use indexer::{IndexError, IndexProgress, IndexReport, Indexer};
pub use queries::{
    BoundedRead, Outline, QueryError, RepoMap, SearchHit, SymbolIdentity, UnfoldResult, outline,
    read_bounded, repo_map, search, unfold,
};
pub use scanner::{
    AtlasError, ScanError, ScanOptions, ScanResult, SkipReason, scan, scan_with_options,
};
