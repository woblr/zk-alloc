//! Lightweight counters for the large-region cache.
//!
//! These are plain relaxed atomics — they exist to answer "is the cache
//! actually being reused?" during tuning, not to be a precise accounting
//! system. Reading them never blocks an allocation.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

pub(crate) struct Counters {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub mapped: AtomicU64,
    pub unmapped: AtomicU64,
    pub cached_bytes: AtomicUsize,
}

pub(crate) static STATS: Counters = Counters {
    hits: AtomicU64::new(0),
    misses: AtomicU64::new(0),
    mapped: AtomicU64::new(0),
    unmapped: AtomicU64::new(0),
    cached_bytes: AtomicUsize::new(0),
};

/// A snapshot of large-region cache activity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Large allocations served from a cached, already-resident region.
    pub cache_hits: u64,
    /// Large allocations that had to ask the kernel for a fresh mapping.
    pub cache_misses: u64,
    /// Regions mapped from the kernel over the process lifetime.
    pub regions_mapped: u64,
    /// Regions returned to the kernel over the process lifetime.
    pub regions_unmapped: u64,
    /// Bytes currently held resident in the cache.
    pub bytes_cached: usize,
}

impl Stats {
    /// Fraction of large allocations satisfied without a syscall, in `0.0..=1.0`.
    /// Returns 0.0 before any large allocation has happened.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }
}

/// Read the current cache counters.
#[must_use]
pub fn stats() -> Stats {
    Stats {
        cache_hits: STATS.hits.load(Relaxed),
        cache_misses: STATS.misses.load(Relaxed),
        regions_mapped: STATS.mapped.load(Relaxed),
        regions_unmapped: STATS.unmapped.load(Relaxed),
        bytes_cached: STATS.cached_bytes.load(Relaxed),
    }
}
