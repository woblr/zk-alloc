//! Tunable parameters and lazily-resolved runtime values.
//!
//! The compile-time constants describe the shape of the large-region cache;
//! the runtime values (shard count, cache ceiling) depend on the machine and
//! are resolved once on first use without touching the heap.

use std::sync::OnceLock;

/// Base page size. Linux on x86-64/aarch64 uses 4 KiB; we assume that here and
/// only ever round *up* to it, so a larger real page never breaks correctness.
pub const PAGE: usize = 4096;

/// Transparent-huge-page size (2 MiB). Large regions are aligned to this so the
/// kernel can back them with huge pages after `madvise(MADV_HUGEPAGE)`.
pub const HUGE_PAGE: usize = 2 * 1024 * 1024;

/// Requests of at least this size take the cached large-region path; anything
/// smaller is forwarded to the system allocator. 64 KiB sits below glibc's
/// mmap threshold, so the medium FFT/MSM buffers that the libc allocator would
/// otherwise bounce off `mmap`/`munmap` land in our resident cache instead.
pub const LARGE_THRESHOLD: usize = 64 * 1024;

/// Regions larger than this are mapped and unmapped on demand rather than
/// retained — holding multi-hundred-MB buffers resident rarely pays off and
/// the address space is better returned.
pub const CACHE_MAX_REGION: usize = 256 * 1024 * 1024;

/// Number of size classes between [`LARGE_THRESHOLD`] and [`CACHE_MAX_REGION`].
/// Four classes per power of two from 2^16 up to 2^28 is 52; rounded up for
/// headroom so the index never escapes the fixed shard arrays.
pub const MAX_CLASSES: usize = 56;

/// Upper bound on cache shards. The real count is the CPU count rounded to a
/// power of two, capped here.
pub const MAX_SHARDS: usize = 64;

/// Environment override for the cache ceiling, in bytes.
pub const CACHE_BYTES_ENV: &str = "ZK_ALLOC_CACHE_BYTES";

/// How many shards to spread the cache across — CPU count rounded up to a power
/// of two so a thread can pick its shard with a single mask.
pub fn num_shards() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        let cpus = if cpus < 1 { 1 } else { cpus as usize };
        cpus.next_power_of_two().min(MAX_SHARDS)
    })
}

/// Default cache ceiling when physical RAM is unknown, and the hard cap on the
/// fraction-of-RAM default. Kept deliberately conservative: a real prover
/// allocates many different sizes across phases, and an over-eager cache
/// retains all of them, inflating RSS for no speed benefit on compute-bound
/// proofs. Raise it with `ZK_ALLOC_CACHE_BYTES` when proving is genuinely
/// allocation-bound and the memory is available.
const DEFAULT_CEILING_CAP: usize = 1024 * 1024 * 1024;

/// Total bytes the large-region cache may hold resident before freed regions
/// are returned to the kernel instead of cached. Defaults to `min(RAM/8, 1 GiB)`;
/// override with `ZK_ALLOC_CACHE_BYTES`.
pub fn cache_ceiling() -> usize {
    static C: OnceLock<usize> = OnceLock::new();
    *C.get_or_init(|| {
        if let Some(v) = std::env::var_os(CACHE_BYTES_ENV) {
            if let Some(bytes) = v.to_str().and_then(|s| s.trim().parse::<usize>().ok()) {
                return bytes;
            }
        }
        let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
        let page = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) };
        if pages > 0 && page > 0 {
            let eighth = (pages as usize / 8).saturating_mul(page as usize);
            eighth.min(DEFAULT_CEILING_CAP)
        } else {
            DEFAULT_CEILING_CAP / 2
        }
    })
}
