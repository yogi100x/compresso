//! Repository scanning, parsing, indexing, and structural query primitives.

pub mod parsers;
pub mod scanner;

pub use scanner::{
    AtlasError, ScanError, ScanOptions, ScanResult, SkipReason, scan, scan_with_options,
};
