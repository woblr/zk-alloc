//! Synthetic benchmarks that mimic how a prover hits the allocator, so we can
//! compare zk-alloc against the system allocator without pulling in a whole
//! proving stack.
//!
//! The patterns are drawn from real prover code:
//!
//! - **fft_rounds**: a power-of-two coefficient vector allocated, touched, and
//!   freed every round — the FFT/NTT loop. This is where keeping the buffer
//!   resident across rounds should pay off.
//! - **msm_buckets**: many medium `2^c` bucket arrays per round, allocated
//!   across threads, as Pippenger MSM does.
//! - **mixed_phase**: a burst of large buffers allocated then all freed,
//!   modelling a proof phase boundary.
//!
//! Run the system-allocator baseline with the default allocator, then the
//! zk-alloc numbers by setting it global (see the `zk_alloc_global` cfg at the
//! bottom). The crate's own allocator is *not* installed by default here so the
//! baseline is honest.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

// Exactly one global allocator is installed, chosen by feature. The `not(...)`
// guards make the choice unambiguous even if several features are on at once
// (zk-alloc takes precedence), so `cargo build --all-features` still compiles.
#[cfg(feature = "bench-global")]
#[global_allocator]
static GLOBAL: zk_alloc::ZkAlloc = zk_alloc::ZkAlloc::new();

#[cfg(all(feature = "bench-jemalloc", not(feature = "bench-global")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(
    feature = "bench-mimalloc",
    not(feature = "bench-global"),
    not(feature = "bench-jemalloc")
))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const FIELD: usize = 32; // BN254 scalar, bytes

/// One FFT-sized coefficient vector per round, written end-to-end and dropped.
fn fft_rounds(c: &mut Criterion) {
    let mut group = c.benchmark_group("fft_rounds");
    for log_n in [18usize, 20, 22] {
        let n = 1usize << log_n;
        let bytes = n * FIELD;
        group.throughput(Throughput::Bytes(bytes as u64));
        let words = bytes / 8;
        group.bench_with_input(BenchmarkId::from_parameter(log_n), &words, |b, &words| {
            b.iter(|| {
                // Fill with a non-zero pattern: this faults every page (like a
                // prover filling a coefficient vector) and, unlike `vec![0; n]`,
                // gets no free zero-pages from `calloc`, so the comparison turns
                // on page residency, not on who skips the memset.
                let v = vec![0x5a5a_5a5a_5a5a_5a5au64; words];
                black_box(&v);
            });
        });
    }
    group.finish();
}

/// Pippenger-style bucket arrays: several `2^c` allocations per round.
fn msm_buckets(c: &mut Criterion) {
    let mut group = c.benchmark_group("msm_buckets");
    for c_bits in [10usize, 14, 16] {
        let buckets = 1usize << c_bits;
        let windows = 254 / c_bits + 1;
        group.bench_with_input(
            BenchmarkId::from_parameter(c_bits),
            &buckets,
            |b, &buckets| {
                b.iter(|| {
                    let mut sets: Vec<Vec<[u8; 96]>> = Vec::with_capacity(windows);
                    for _ in 0..windows {
                        let mut bucket = vec![[0u8; 96]; buckets];
                        bucket[0][0] = 1;
                        bucket[buckets - 1][0] = 2;
                        sets.push(bucket);
                    }
                    black_box(&sets);
                });
            },
        );
    }
    group.finish();
}

/// A phase that allocates a burst of large buffers and frees them together.
fn mixed_phase(c: &mut Criterion) {
    c.bench_function("mixed_phase", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let mut bufs: Vec<Vec<u64>> = Vec::new();
                for k in 0..16 {
                    let n = (1usize << 18) + k * (1 << 15);
                    let mut v = vec![0u64; n];
                    v[0] = 1;
                    v[n - 1] = 2;
                    bufs.push(v);
                }
                black_box(&bufs);
                drop(bufs); // phase boundary: everything dies at once
                total += start.elapsed();
            }
            total
        });
    });
}

criterion_group!(benches, fft_rounds, msm_buckets, mixed_phase);
criterion_main!(benches);
