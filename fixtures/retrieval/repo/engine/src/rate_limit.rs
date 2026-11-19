/// Token bucket that throttles how fast one tenant may enqueue.
pub struct TokenBucket {
    capacity: u32,
    available: u32,
    refill_per_second: u32,
    last_refill: u64,
}

impl TokenBucket {
    pub fn new(capacity: u32, refill_per_second: u32) -> Self {
        Self {
            capacity,
            available: capacity,
            refill_per_second,
            last_refill: 0,
        }
    }

    fn refill(&mut self, now: u64) {
        let elapsed = now.saturating_sub(self.last_refill);
        let added = elapsed.saturating_mul(u64::from(self.refill_per_second));
        self.available = self
            .capacity
            .min(self.available.saturating_add(added.min(u64::from(u32::MAX)) as u32));
        self.last_refill = now;
    }

    /// Takes one token, or reports that the request is throttled.
    pub fn try_acquire(&mut self, now: u64) -> bool {
        self.refill(now);
        if self.available == 0 {
            return false;
        }
        self.available -= 1;
        true
    }
}
