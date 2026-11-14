use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::thread;

/// Default ceiling for source bytes buffered by in-flight parallel work.
pub const DEFAULT_MEMORY_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// Bounds for parallel scanning and indexing work.
///
/// `jobs` never exceeds the CPUs available to the process, and
/// `memory_budget_bytes` caps the bytes that in-flight workers may buffer.
/// Parallel and serial runs produce identical results; only wall time differs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Concurrency {
    jobs: NonZeroUsize,
    memory_budget_bytes: u64,
}

impl Concurrency {
    /// One worker on the calling thread: the exact serial behavior.
    pub const fn serial() -> Self {
        Self {
            jobs: NonZeroUsize::MIN,
            memory_budget_bytes: DEFAULT_MEMORY_BUDGET_BYTES,
        }
    }

    /// Requests `jobs` workers, clamped to `1..=available CPUs`, under a byte budget.
    pub fn bounded(jobs: usize, memory_budget_bytes: u64) -> Self {
        Self::exact(jobs.min(available_cpus()), memory_budget_bytes)
    }

    /// Uses exactly `jobs` workers (at least one) regardless of the CPU count.
    pub(crate) fn exact(jobs: usize, memory_budget_bytes: u64) -> Self {
        Self {
            jobs: NonZeroUsize::new(jobs).unwrap_or(NonZeroUsize::MIN),
            memory_budget_bytes: memory_budget_bytes.max(1),
        }
    }

    pub const fn jobs(&self) -> usize {
        self.jobs.get()
    }

    pub const fn memory_budget_bytes(&self) -> u64 {
        self.memory_budget_bytes
    }

    /// Workers for `items` units of work that each pin up to `per_worker_bytes`.
    pub(crate) fn workers_for(&self, items: usize, per_worker_bytes: u64) -> usize {
        let by_memory = usize::try_from(self.memory_budget_bytes / per_worker_bytes.max(1))
            .unwrap_or(usize::MAX)
            .max(1);
        self.jobs().min(by_memory).min(items.max(1))
    }
}

impl Default for Concurrency {
    fn default() -> Self {
        Self::bounded(available_cpus(), DEFAULT_MEMORY_BUDGET_BYTES)
    }
}

fn available_cpus() -> usize {
    thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

/// In-order admission control for read-ahead work awaiting an ordered commit.
///
/// Bytes are charged at dispatch and released only at commit, so results that
/// queue behind one slow item still count against the budget.
#[derive(Debug)]
pub(crate) struct ReadAhead {
    budget_bytes: u64,
    window: usize,
    in_flight_bytes: u64,
    in_flight: VecDeque<u64>,
}

impl ReadAhead {
    pub(crate) fn new(budget_bytes: u64, window: usize) -> Self {
        Self {
            budget_bytes,
            window: window.max(1),
            in_flight_bytes: 0,
            in_flight: VecDeque::new(),
        }
    }

    /// Admits the next item unless the window or byte budget is exhausted.
    /// A lone item is always admitted so an over-budget file cannot stall.
    pub(crate) fn try_admit(&mut self, cost_bytes: u64) -> bool {
        if self.in_flight.len() >= self.window {
            return false;
        }
        if !self.in_flight.is_empty()
            && self.in_flight_bytes.saturating_add(cost_bytes) > self.budget_bytes
        {
            return false;
        }
        self.in_flight.push_back(cost_bytes);
        self.in_flight_bytes = self.in_flight_bytes.saturating_add(cost_bytes);
        true
    }

    /// Releases the oldest admitted item after its ordered commit.
    pub(crate) fn release_oldest(&mut self) {
        if let Some(cost_bytes) = self.in_flight.pop_front() {
            self.in_flight_bytes = self.in_flight_bytes.saturating_sub(cost_bytes);
        }
    }

    #[cfg(test)]
    pub(crate) const fn in_flight_bytes(&self) -> u64 {
        self.in_flight_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bounded_concurrency_never_exceeds_available_cpus() {
        let cpus = available_cpus();
        assert_eq!(Concurrency::bounded(usize::MAX, 1024).jobs(), cpus);
        assert_eq!(Concurrency::bounded(0, 1024).jobs(), 1);
        assert_eq!(Concurrency::default().jobs(), cpus);
        assert_eq!(Concurrency::serial().jobs(), 1);
    }

    #[test]
    fn test_workers_are_limited_by_memory_budget_and_item_count() {
        let concurrency = Concurrency::exact(8, 4_000);
        assert_eq!(concurrency.workers_for(100, 1_000), 4);
        assert_eq!(concurrency.workers_for(100, 10_000), 1);
        assert_eq!(concurrency.workers_for(3, 1), 3);
        assert_eq!(concurrency.workers_for(0, 1), 1);
    }

    #[test]
    fn test_read_ahead_holds_budget_until_ordered_release() {
        let mut read_ahead = ReadAhead::new(100, 8);
        assert!(read_ahead.try_admit(60));
        assert!(read_ahead.try_admit(40));
        assert!(!read_ahead.try_admit(1));
        assert_eq!(read_ahead.in_flight_bytes(), 100);
        read_ahead.release_oldest();
        assert_eq!(read_ahead.in_flight_bytes(), 40);
        assert!(read_ahead.try_admit(60));
    }

    #[test]
    fn test_read_ahead_admits_one_over_budget_item_alone() {
        let mut read_ahead = ReadAhead::new(10, 8);
        assert!(read_ahead.try_admit(500));
        assert!(!read_ahead.try_admit(1));
        read_ahead.release_oldest();
        assert!(read_ahead.try_admit(1));
    }

    #[test]
    fn test_read_ahead_window_bounds_zero_cost_items() {
        let mut read_ahead = ReadAhead::new(10, 2);
        assert!(read_ahead.try_admit(0));
        assert!(read_ahead.try_admit(0));
        assert!(!read_ahead.try_admit(0));
    }
}
