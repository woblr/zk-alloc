# Changelog

All notable changes to `zk-alloc` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-06-08

A ground-up rewrite. The 0.2 line was a three-arena bump allocator that never
reclaimed individual frees — as a global allocator it exhausted its arenas and
fell through to the system allocator under any allocate/free churn. 1.0 replaces
that with a design that actually reclaims, and that targets the one thing a
prover does that a general allocator handles poorly: re-acquiring large buffers
round after round.

### Added
- **Large-region cache.** Allocations of 64 KiB and up are rounded to a size
  class and cached resident in sharded, intrusive free lists. Reuse skips the
  kernel page-fault-and-zero cost that dominates large-buffer churn — 4–5×
  faster than glibc and jemalloc on multi-megabyte FFT-round patterns, and on
  par with or ahead of mimalloc, in the bundled benchmarks (which can run each
  allocator head-to-head via the `bench-jemalloc` / `bench-mimalloc` features).
- **Lazy zeroing for `alloc_zeroed`.** A reused region is re-zeroed with
  `MADV_DONTNEED` rather than an eager memset, so untouched pages never fault —
  matching `calloc`'s laziness on sparse, zero-initialized buffers.
- **Transparent huge pages** for large regions via `madvise(MADV_HUGEPAGE)`,
  with no hugetlb pool setup required.
- **`ZkAlloc`** global allocator: routes small allocations to the system
  allocator and large ones to the cache, classifying frees from the layout
  alone (no header, no global table).
- **`Arena`**: a scoped, huge-page-backed bump allocator with O(1) `reset` and a
  secure-wipe variant.
- **`SecretBuf`**: a swap-locked (`mlock`), guard-fenced, wipe-on-drop buffer
  for witness data.
- **Observability and tuning**: `stats()` (cache hit rate), `cached_bytes()`,
  `release_cache()` (return address space), `soften_cache()` (`MADV_FREE` warm
  release), and the `ZK_ALLOC_CACHE_BYTES` environment override (default: half
  of physical RAM).

### Changed
- **Linux only.** The allocator now depends on `mmap`/`madvise` semantics with
  no portable equivalent; the macOS and Windows backends from 0.2 are dropped.

### Notes
- Verified race-free under ThreadSanitizer (concurrent allocate/free and
  cross-thread free).
