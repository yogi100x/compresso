use std::collections::BTreeMap;
use std::io::Read;

/// Seconds a source's modification time may trail the cache stamp and still
/// be trusted without reading the source.
pub const FRESHNESS_SLACK_SECONDS: i64 = 2;
/// Bytes read per step when a source must be digested in full.
pub const FRESHNESS_CHUNK_BYTES: usize = 64 * 1024;

/// One rendered job report and what its source looked like when it was cached.
pub struct CachedReport {
    pub job_id: u64,
    pub body: String,
    pub source_length: u64,
    pub source_digest: String,
    pub cached_at: i64,
}

/// Rendered reports by job id.
pub struct ReportCache {
    pub reports: BTreeMap<u64, CachedReport>,
}

/// What the disk says about one report source right now.
pub trait SourceDisk {
    fn length(&self, job_id: u64) -> Option<u64>;
    fn modified(&self, job_id: u64) -> Option<i64>;
    fn open(&self, job_id: u64) -> Option<Box<dyn Read>>;
}

enum Verdict {
    Changed,
    Unchanged,
    NeedsDigest,
}

/// Returns one cached report and whether its source changed since caching.
pub fn report_current(cache: &ReportCache, disk: &dyn SourceDisk, job_id: u64) -> Option<(String, bool)> {
    let cached = cache.reports.get(&job_id)?;
    let stale = !source_unchanged(cached, disk);
    Some((cached.body.clone(), stale))
}

/// Reports whether any cached report in the catalog no longer matches its source.
pub fn catalog_stale(cache: &ReportCache, disk: &dyn SourceDisk) -> bool {
    cache
        .reports
        .values()
        .any(|cached| !source_unchanged(cached, disk))
}

/// Decides from metadata alone whether a source must be digested: a length
/// that differs is a change, and a source untouched since before it was
/// cached is trusted.
fn metadata_verdict(length: u64, modified: Option<i64>, cached: &CachedReport) -> Verdict {
    if length != cached.source_length {
        return Verdict::Changed;
    }
    match modified {
        Some(modified) if modified + FRESHNESS_SLACK_SECONDS <= cached.cached_at => Verdict::Unchanged,
        _ => Verdict::NeedsDigest,
    }
}

/// Whether the source still matches the cached copy, reading it only when
/// metadata cannot decide.
fn source_unchanged(cached: &CachedReport, disk: &dyn SourceDisk) -> bool {
    let Some(length) = disk.length(cached.job_id) else {
        return false;
    };
    match metadata_verdict(length, disk.modified(cached.job_id), cached) {
        Verdict::Changed => false,
        Verdict::Unchanged => true,
        Verdict::NeedsDigest => match disk.open(cached.job_id) {
            Some(mut reader) => digest_matches(&mut reader, cached),
            None => false,
        },
    }
}

/// Digests the source through one fixed buffer and stops once it is longer
/// than the cached length, so a growing source cannot extend the read.
fn digest_matches(reader: &mut dyn Read, cached: &CachedReport) -> bool {
    let mut digest = 0_u64;
    let mut buffer = vec![0_u8; FRESHNESS_CHUNK_BYTES];
    let mut total = 0_u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => return false,
        };
        total += read as u64;
        if total > cached.source_length {
            return false;
        }
        for byte in &buffer[..read] {
            digest = digest.wrapping_mul(31).wrapping_add(u64::from(*byte));
        }
    }
    total == cached.source_length && format!("{digest:016x}") == cached.source_digest
}

/// Stores a freshly rendered report for a job.
pub fn store_report(cache: &mut ReportCache, report: CachedReport) {
    cache.reports.insert(report.job_id, report);
}

/// Drops the cached report of a job that was deleted.
pub fn evict_report(cache: &mut ReportCache, job_id: u64) -> bool {
    cache.reports.remove(&job_id).is_some()
}

/// Total bytes of rendered report text held by the cache.
pub fn cached_report_bytes(cache: &ReportCache) -> usize {
    cache.reports.values().map(|report| report.body.len()).sum()
}

/// Job ids whose cached report is larger than `limit` bytes.
pub fn large_reports(cache: &ReportCache, limit: usize) -> Vec<u64> {
    cache
        .reports
        .values()
        .filter(|report| report.body.len() > limit)
        .map(|report| report.job_id)
        .collect()
}
