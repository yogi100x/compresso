//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod indexer;
pub mod parsers;
pub mod scanner;

pub use indexer::{IndexError, IndexProgress, IndexReport, Indexer};
pub use scanner::{
    AtlasError, ScanError, ScanOptions, ScanResult, SkipReason, scan, scan_with_options,
};
