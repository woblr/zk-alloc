//! Parallel large-allocation throughput, the prover's per-thread FFT/MSM
//! pattern under contention. Each thread repeatedly allocates a large buffer,
//! faults every page, and frees it; we sweep the thread count and report
//! aggregate throughput. Above glibc's ~32 MB mmap threshold, general
//! allocators munmap and re-fault every buffer — a sharded resident cache does
//! not.
//!
//! Build one binary per allocator and compare (default is glibc):
//!   cargo run --release --example contention                            # glibc
//!   cargo run --release --example contention --features bench-jemalloc  # jemalloc
//!   cargo run --release --example contention --features bench-mimalloc  # mimalloc
//!   cargo run --release --example contention --features bench-global    # zk-alloc
//!
//! Optional args: <buffer_MB> <window_seconds>, e.g. `... -- 64 3`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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

fn allocator() -> &'static str {
    if cfg!(feature = "bench-global") {
        "zk-alloc"
    } else if cfg!(feature = "bench-jemalloc") {
        "jemalloc"
    } else if cfg!(feature = "bench-mimalloc") {
        "mimalloc"
    } else {
        "glibc"
    }
}

#[inline(never)]
fn work(words: usize, tag: u64) -> u64 {
    let mut v: Vec<u64> = vec![tag; words];
    let mut acc = 0u64;
    let mut i = 0;
    while i < words {
        v[i] = v[i].wrapping_add(i as u64);
        acc = acc.wrapping_add(v[i]);
        i += 512; // one write per 4 KiB page
    }
    std::hint::black_box(&v);
    acc
}

fn main() {
    let mb: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(64);
    let secs: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let words = mb * 1024 * 1024 / 8;
    let cores = thread::available_parallelism().map(|n| n.get()).unwrap_or(8);

    println!("allocator={} buffer={mb}MB window={secs}s cores={cores}", allocator());
    println!("{:>8}  {:>12}  {:>9}", "threads", "ops/s", "GB/s");

    for &threads in &[1usize, 2, 4, 8, cores] {
        if threads > cores {
            continue;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let mut ops = 0u64;
                    let mut sink = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        sink = sink.wrapping_add(work(words, t as u64 + 1));
                        ops += 1;
                    }
                    std::hint::black_box(sink);
                    ops
                })
            })
            .collect();
        thread::sleep(Duration::from_secs_f64(secs));
        stop.store(true, Ordering::Relaxed);

        let ops: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        let ops_s = ops as f64 / start.elapsed().as_secs_f64();
        println!("{threads:>8}  {ops_s:>12.0}  {:>9.2}", ops_s * mb as f64 / 1024.0);
    }
}
