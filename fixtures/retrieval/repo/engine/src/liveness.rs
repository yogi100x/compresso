pub struct Liveness {
    pub grace_ms: u64,
}

impl Liveness {
    pub fn new(grace_ms: u64) -> Self {
        Self { grace_ms }
    }

    /// Reports whether a node last seen at `seen_ms` still counts as alive.
    pub fn is_alive(&self, seen_ms: u64, now_ms: u64) -> bool {
        // Hosts disagree on time: allow for clock skew, so a beat stamped a
        // little in the future still counts instead of underflowing.
        let elapsed = now_ms.saturating_sub(seen_ms);
        elapsed <= self.grace_ms
    }
}
