//! The large-region cache: the piece that wins on prover workloads.
//!
//! Every allocation at or above [`LARGE_THRESHOLD`] is rounded to a size class
//! and served from, or returned to, a cache of resident mappings. Reusing a
//! mapping that is already faulted in is the whole point — it skips the kernel
//! page-fault-and-zero storm that the default allocator pays each time it
//! re-acquires a freed multi-megabyte buffer for the next proof round.
//!
//! ## Layout
//!
//! Free regions are tracked with an intrusive list: while a region sits in the
//! cache, its first bytes hold a [`FreeNode`]. That means the cache needs no
//! heap of its own — important, because this code runs *as* the global
//! allocator and cannot call back into itself for bookkeeping.
//!
//! The lists are split across [`num_shards`] shards, picked per thread, so
//! concurrent provers rarely contend. Regions are fungible: a buffer freed on
//! one thread can be reused on any other, so a foreign free just lands in the
//! freeing thread's shard with no cross-thread handoff machinery.
//!
//! ## Classification
//!
//! `dealloc` is handed the original `Layout`, so the size class is recomputed
//! from `layout.size()` exactly as on `alloc` — no per-allocation header and no
//! global address table. A request below [`LARGE_THRESHOLD`] never reaches this
//! module; it goes to the system allocator.

use std::ptr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Mutex;

use crate::config::{
    cache_ceiling, num_shards, CACHE_MAX_REGION, HUGE_PAGE, MAX_CLASSES, MAX_SHARDS,
};
use crate::size_class::{class_of, region_align};
use crate::stats::STATS;
use crate::sys;

/// Stored at the base of a region while it is cached. The region is free
/// memory we own, so it is sound to scribble this in and read it back on pop.
#[repr(C)]
struct FreeNode {
    next: *mut FreeNode,
    total: usize,
}

struct ShardCache {
    heads: [*mut FreeNode; MAX_CLASSES],
}

// The raw pointers only ever name regions this allocator owns, and every access
// goes through the shard mutex, so sharing across threads is sound.
unsafe impl Send for ShardCache {}

struct Shard(Mutex<ShardCache>);

impl Shard {
    const fn new() -> Self {
        Shard(Mutex::new(ShardCache {
            heads: [ptr::null_mut(); MAX_CLASSES],
        }))
    }

    fn pop(&self, index: usize) -> *mut u8 {
        let mut cache = lock(&self.0);
        let head = cache.heads[index];
        if head.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `head` is a region we cached, large enough to hold a FreeNode.
        cache.heads[index] = unsafe { (*head).next };
        head as *mut u8
    }

    fn push(&self, index: usize, region: *mut u8, total: usize) {
        let node = region as *mut FreeNode;
        let mut cache = lock(&self.0);
        // SAFETY: `region` is ours and at least `total` (>= a FreeNode) bytes.
        unsafe {
            (*node).next = cache.heads[index];
            (*node).total = total;
        }
        cache.heads[index] = node;
    }

    /// Advise every cached region as `MADV_FREE`: the regions stay in the cache
    /// (warm), but the kernel may reclaim their physical pages under pressure.
    /// If a region is reused before that happens, the first write cancels the
    /// advice and no re-fault occurs. Returns bytes softened.
    fn soften(&self) -> usize {
        let cache = lock(&self.0);
        let mut bytes = 0usize;
        for &head in cache.heads.iter() {
            let mut node = head;
            while !node.is_null() {
                // SAFETY: cached region we own; read link/size before advising.
                let (next, total) = unsafe { ((*node).next, (*node).total) };
                sys::advise_free(node as *mut u8, total);
                bytes += total;
                node = next;
            }
        }
        bytes
    }

    /// Unmap everything held in this shard, returning bytes released.
    fn drain(&self) -> usize {
        let mut cache = lock(&self.0);
        let mut freed = 0usize;
        for head in cache.heads.iter_mut() {
            let mut node = *head;
            while !node.is_null() {
                // SAFETY: cached region; read its size and link before unmapping.
                let (next, total) = unsafe { ((*node).next, (*node).total) };
                sys::unmap(node as *mut u8, total);
                STATS.unmapped.fetch_add(1, Relaxed);
                freed += total;
                node = next;
            }
            *head = ptr::null_mut();
        }
        freed
    }
}

static SHARDS: [Shard; MAX_SHARDS] = [const { Shard::new() }; MAX_SHARDS];

/// Ignore mutex poisoning: a panic while holding a shard lock must not wedge the
/// global allocator, and the cache invariants survive an unwind.
#[inline]
fn lock(m: &Mutex<ShardCache>) -> std::sync::MutexGuard<'_, ShardCache> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Pick this thread's shard from the address of a thread-local anchor — unique
/// per thread, costs no allocation, and never re-enters the allocator.
#[inline]
fn shard() -> &'static Shard {
    thread_local!(static ANCHOR: u8 = const { 0 });
    let id = ANCHOR.with(|a| a as *const u8 as usize);
    &SHARDS[(id >> 6) & (num_shards() - 1)]
}

/// Serve a large request (`size >= LARGE_THRESHOLD`), reusing a cached region
/// when one of the right class is available. Returns null on OOM.
pub fn alloc(size: usize, align: usize) -> *mut u8 {
    acquire(size, align).0
}

/// Like [`alloc`] but zero-initialized. A freshly mapped region is already zero
/// (the kernel zero-fills anonymous pages on fault), so nothing is done for it.
/// A reused region still holds its previous tenant's bytes, so its pages are
/// dropped with `MADV_DONTNEED`: the next read of each page returns zero
/// lazily, exactly like `calloc`. That is far cheaper than eagerly memsetting a
/// large region the caller may only touch sparsely — the case where `calloc`
/// would otherwise leave a custom allocator badly behind.
pub fn alloc_zeroed(size: usize, align: usize) -> *mut u8 {
    let (ptr, reused) = acquire(size, align);
    if !ptr.is_null() && reused {
        sys::advise_dontneed(ptr, class_of(size).total);
    }
    ptr
}

/// Returns the region and whether it came from the cache (and so may be dirty).
fn acquire(size: usize, align: usize) -> (*mut u8, bool) {
    let class = class_of(size);
    let ra = region_align(class.total);

    // Too large to retain, or aligned beyond what a pooled region guarantees:
    // map it directly and don't track it in the cache.
    if class.total > CACHE_MAX_REGION || align > HUGE_PAGE {
        return (map_fresh(class.total, align.max(ra)), false);
    }

    // A pooled region of this class is exactly `ra`-aligned, so it can only
    // satisfy requests whose alignment it already meets.
    if align <= ra {
        let region = shard().pop(class.index);
        if !region.is_null() {
            STATS.hits.fetch_add(1, Relaxed);
            STATS.cached_bytes.fetch_sub(class.total, Relaxed);
            return (region, true);
        }
    }

    (map_fresh(class.total, align.max(ra)), false)
}

/// Return a large region. Caches it for reuse unless that would push the cache
/// past its byte ceiling, in which case the pages go back to the kernel.
pub fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    let class = class_of(size);

    if class.total > CACHE_MAX_REGION || align > HUGE_PAGE {
        sys::unmap(ptr, class.total);
        STATS.unmapped.fetch_add(1, Relaxed);
        return;
    }

    // Soft ceiling: a slight overshoot across racing threads is fine.
    if STATS.cached_bytes.load(Relaxed) + class.total <= cache_ceiling() {
        shard().push(class.index, ptr, class.total);
        STATS.cached_bytes.fetch_add(class.total, Relaxed);
    } else {
        sys::unmap(ptr, class.total);
        STATS.unmapped.fetch_add(1, Relaxed);
    }
}

#[inline]
fn map_fresh(total: usize, align: usize) -> *mut u8 {
    let p = sys::map(total, align);
    if !p.is_null() {
        STATS.misses.fetch_add(1, Relaxed);
        STATS.mapped.fetch_add(1, Relaxed);
    }
    p
}

/// Drop every cached region, returning the address space to the kernel.
///
/// Useful between unrelated jobs in a long-lived process: it trades the warm
/// cache for a lower resident footprint. Live allocations are untouched.
pub fn release_all() -> usize {
    let n = num_shards();
    let mut freed = 0;
    for shard in SHARDS.iter().take(n) {
        freed += shard.drain();
    }
    STATS
        .cached_bytes
        .fetch_sub(freed.min(STATS.cached_bytes.load(Relaxed)), Relaxed);
    freed
}

/// Advise every cached region reclaimable under memory pressure while keeping
/// it in the cache. Returns bytes softened.
pub fn soften_all() -> usize {
    let n = num_shards();
    SHARDS.iter().take(n).map(Shard::soften).sum()
}

/// Total bytes currently parked in the cache.
pub fn cached_bytes() -> usize {
    STATS.cached_bytes.load(Relaxed)
}
