//! Installs zk-alloc as the global allocator and runs a few rounds of the
//! kind of large, repeated allocation a prover does, then prints the cache
//! hit rate. This is the end-to-end smoke test: the allocator is servicing
//! every `Vec` in the process here, not being called directly.
//!
//! ```text
//! cargo run --release --example global
//! ```

use zk_alloc::ZkAlloc;

#[global_allocator]
static GLOBAL: ZkAlloc = ZkAlloc::new();

fn main() {
    let rounds = 50;
    let n = 1usize << 22; // ~4M field elements

    for round in 0..rounds {
        // A fresh coefficient vector each round, written end-to-end and dropped
        // — the FFT loop. After the first round these reuse cached pages.
        let mut coeffs: Vec<u64> = vec![0; n];
        for (i, c) in coeffs.iter_mut().enumerate() {
            *c = (i as u64).wrapping_mul(round as u64 + 1);
        }
        std::hint::black_box(&coeffs);

        // A burst of MSM-bucket-sized arrays freed at the end of the round.
        let mut buckets: Vec<Vec<[u8; 96]>> = Vec::new();
        for _ in 0..8 {
            buckets.push(vec![[0u8; 96]; 1 << 14]);
        }
        std::hint::black_box(&buckets);
    }

    let s = zk_alloc::stats();
    println!("large allocations : {}", s.cache_hits + s.cache_misses);
    println!("cache hits        : {}", s.cache_hits);
    println!("cache misses      : {}", s.cache_misses);
    println!("hit rate          : {:.1}%", s.hit_rate() * 100.0);
    println!("regions mapped    : {}", s.regions_mapped);
    println!(
        "bytes cached now  : {} MiB",
        zk_alloc::cached_bytes() / (1024 * 1024)
    );

    let released = zk_alloc::release_cache();
    println!("released on exit  : {} MiB", released / (1024 * 1024));
}
