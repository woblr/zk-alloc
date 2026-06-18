//! # zk-alloc
//!
//! High-performance Linux allocator for ZK provers — keeps large buffers resident across proof rounds to eliminate page-fault overhead.
//!
//! General-purpose allocators are built for long-lived, mixed workloads. A
//! prover is the opposite: a handful of worker threads churn through enormous,
//! short-lived, power-of-two buffers — FFT/NTT coefficient vectors, MSM bucket
//! arrays, trace columns — allocating and freeing gigabytes every proof round.
//! The default allocator hands those freed pages back to the kernel, so the
//! next round re-faults and re-zeroes them: a page-fault storm on the critical
//! path.
//!
//! zk-alloc keeps them instead. Large allocations are rounded to a size class
//! and parked, resident, in a sharded cache; the next request of that class
//! reuses warm pages with no syscall and no zeroing. Regions are backed by
//! transparent huge pages to ease TLB pressure on the big arrays. Small
//! allocations are forwarded to the system allocator, which already handles
//! them well.
//!
//! ## As a global allocator
//!
//! ```no_run
//! use zk_alloc::ZkAlloc;
//!
//! #[global_allocator]
//! static GLOBAL: ZkAlloc = ZkAlloc::new();
//!
//! fn main() {
//!     // Vec, Box, etc. now route through zk-alloc.
//!     let coeffs = vec![0u64; 1 << 20];
//!     std::hint::black_box(&coeffs);
//! }
//! ```
//!
//! Set this in your **binary or benchmark, not in a library**. A library that
//! sets `#[global_allocator]` forces the choice on every downstream crate, and
//! the global allocator can only be set once per build.
//!
//! ## Explicit tools
//!
//! Two primitives are useful on their own, with or without the global
//! allocator installed:
//!
//! - [`Arena`] — a scoped bump allocator for a whole proof phase that you reset
//!   in O(1) when the phase ends.
//! - [`SecretBuf`] — a swap-locked, guard-fenced, wipe-on-drop buffer for
//!   witness data.
//!
//! ## Observability and tuning
//!
//! [`stats`] reports the cache hit rate. The cache holds up to `min(RAM/8,
//! 1 GiB)` by default (conservative, so it does no harm on workloads it cannot
//! help); override with the `ZK_ALLOC_CACHE_BYTES` environment variable, and
//! call [`release_cache`] to hand the cached address space back between
//! unrelated jobs.
//!
//! ## Platform
//!
//! Linux only, for now. The allocator leans on `mmap`/`madvise` semantics
//! (`MADV_HUGEPAGE`, `MADV_FREE`) that have no portable equivalent.

#![cfg(target_os = "linux")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod cache;
mod config;
mod error;
mod allocator;
mod size_class;
mod stats;
mod sys;

pub mod arena;
pub mod secure;

pub use arena::Arena;
pub use error::MapFailed;
pub use allocator::ZkAlloc;
pub use secure::SecretBuf;
pub use stats::{stats, Stats};

/// Drop every region currently held in the large-allocation cache, returning
/// the address space to the kernel. Returns the number of bytes released.
///
/// Live allocations are untouched. Use this between unrelated jobs in a
/// long-lived process to trade the warm cache for a lower resident footprint.
pub fn release_cache() -> usize {
    cache::release_all()
}

/// Hint that the cached regions may be reclaimed by the kernel under memory
/// pressure, while keeping them in the cache. Returns the bytes affected.
///
/// This is the gentler sibling of [`release_cache`]: the cache stays warm, RSS
/// drops only if the system actually needs the memory, and a region reused
/// before reclamation costs nothing. Good to call when a prover goes idle but
/// may run again soon.
pub fn soften_cache() -> usize {
    cache::soften_all()
}

/// Bytes currently parked in the large-allocation cache.
#[must_use]
pub fn cached_bytes() -> usize {
    cache::cached_bytes()
}
