use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::safety::SafetyCheck;
use crate::strategy::{Mode, RemovalRecord};
use crate::tokens::TokenProvider;

const TRACE_SCHEMA_VERSION: u32 = 1;
const ULID_TIMESTAMP_MASK: u64 = (1_u64 << 48) - 1;
const CROCKFORD_BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyResult {
    pub strategy_id: String,
    pub applied: bool,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub removals: Vec<RemovalRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub schema_version: u32,
    pub operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub mode: Mode,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub token_provider: TokenProvider,
    pub fallback_used: bool,
    pub fallback_reason: Option<String>,
    pub strategy_results: Vec<StrategyResult>,
    pub safety_checks: Vec<SafetyCheck>,
    pub explanation: String,
    pub created_at: i64,
}

impl Trace {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        mode: Mode,
        before_tokens: usize,
        after_tokens: usize,
        token_provider: TokenProvider,
        fallback_used: bool,
        fallback_reason: Option<String>,
        strategy_results: Vec<StrategyResult>,
        safety_checks: Vec<SafetyCheck>,
        explanation: String,
    ) -> Self {
        let (operation_id, created_at) = operation_identity();
        Self {
            schema_version: TRACE_SCHEMA_VERSION,
            operation_id,
            session_id: None,
            mode,
            before_tokens,
            after_tokens,
            token_provider,
            fallback_used,
            fallback_reason,
            strategy_results,
            safety_checks,
            explanation,
            created_at,
        }
    }
}

fn operation_identity() -> (String, i64) {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let timestamp_ms = u64::try_from(elapsed.as_millis())
        .unwrap_or(u64::MAX)
        .min(ULID_TIMESTAMP_MASK);
    let counter = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&elapsed.as_nanos().to_be_bytes());
    hasher.update(&process::id().to_be_bytes());
    hasher.update(&counter.to_be_bytes());
    let digest = hasher.finalize();

    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&timestamp_ms.to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(&digest.as_bytes()[..10]);
    let created_at = i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX);
    (encode_ulid(bytes), created_at)
}

fn encode_ulid(bytes: [u8; 16]) -> String {
    let mut value = u128::from_be_bytes(bytes);
    let mut encoded = [b'0'; 26];
    for byte in encoded.iter_mut().rev() {
        *byte = CROCKFORD_BASE32[(value & 0x1f) as usize];
        value >>= 5;
    }
    encoded.into_iter().map(char::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_identity_is_valid_and_unique() {
        let first = operation_identity().0;
        let second = operation_identity().0;

        assert_ne!(first, second);
        for id in [first, second] {
            assert_eq!(id.len(), 26);
            assert!(matches!(id.as_bytes()[0], b'0'..=b'7'));
            assert!(id.bytes().all(|byte| CROCKFORD_BASE32.contains(&byte)));
        }
    }
}
