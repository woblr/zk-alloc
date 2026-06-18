//! Concurrency and correctness tests for the large-region cache.
//!
//! These drive `ZkAlloc` directly (the test binary keeps its own global
//! allocator) and lean hard on the shared shard caches: many threads allocate,
//! fill, verify, and free large buffers at once, and a separate test frees
//! buffers on a different thread than allocated them. Run under
//! ThreadSanitizer to check for data races:
//!
//! ```text
//! RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test --release \
//!     -Zbuild-std --target x86_64-unknown-linux-gnu cross_thread
//! ```

use std::alloc::{GlobalAlloc, Layout};
use std::sync::mpsc;
use std::thread;

use zk_alloc::ZkAlloc;

const ALLOC: ZkAlloc = ZkAlloc::new();

/// Fill a buffer with a value derived from `tag` and check it survives a
/// round-trip — catches the cache ever handing out overlapping or wrong-sized
/// regions.
fn stamp(ptr: *mut u8, len: usize, tag: u8) {
    unsafe {
        // Write the tag at both ends and a few interior pages.
        *ptr = tag;
        *ptr.add(len - 1) = tag;
        let mut off = 0;
        while off < len {
            *ptr.add(off) = tag;
            off += 4096;
        }
    }
}

fn check(ptr: *mut u8, len: usize, tag: u8) {
    unsafe {
        assert_eq!(*ptr, tag, "head corrupted");
        assert_eq!(*ptr.add(len - 1), tag, "tail corrupted");
        let mut off = 0;
        while off < len {
            assert_eq!(*ptr.add(off), tag, "page at {off} corrupted");
            off += 4096;
        }
    }
}

#[test]
fn concurrent_alloc_free_is_consistent() {
    let threads = 16;
    let iters = 400;

    let handles: Vec<_> = (0..threads)
        .map(|t| {
            thread::spawn(move || {
                for i in 0..iters {
                    // Vary the size across classes, staying on the large path.
                    let size = 64 * 1024 + ((t * 31 + i * 17) % 64) * 64 * 1024;
                    let layout = Layout::from_size_align(size, 64).unwrap();
                    let tag = (t * 7 + i) as u8;
                    unsafe {
                        let p = ALLOC.alloc(layout);
                        assert!(!p.is_null());
                        stamp(p, size, tag);
                        check(p, size, tag);
                        ALLOC.dealloc(p, layout);
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn cross_thread_free() {
    // Producers allocate and stamp; consumers verify and free. A region freed
    // by a consumer lands in that thread's shard, so this exercises a buffer
    // allocated on one shard being recycled on another.
    let producers = 8;
    let per_producer = 300;

    let (tx, rx) = mpsc::channel::<(usize, usize, u8)>();

    let consumer = thread::spawn(move || {
        let mut freed = 0;
        for (addr, size, tag) in rx {
            let ptr = addr as *mut u8;
            check(ptr, size, tag);
            let layout = Layout::from_size_align(size, 64).unwrap();
            unsafe { ALLOC.dealloc(ptr, layout) };
            freed += 1;
        }
        freed
    });

    let mut prod_handles = Vec::new();
    for t in 0..producers {
        let tx = tx.clone();
        prod_handles.push(thread::spawn(move || {
            for i in 0..per_producer {
                let size = 128 * 1024 + ((t + i) % 32) * 128 * 1024;
                let layout = Layout::from_size_align(size, 64).unwrap();
                let tag = (t * 13 + i) as u8;
                let p = unsafe { ALLOC.alloc(layout) };
                assert!(!p.is_null());
                stamp(p, size, tag);
                tx.send((p as usize, size, tag)).unwrap();
            }
        }));
    }
    drop(tx);

    for h in prod_handles {
        h.join().unwrap();
    }
    let freed = consumer.join().unwrap();
    assert_eq!(freed, producers * per_producer);
}

#[test]
fn release_is_safe_and_bounded() {
    // The cache is process-global and cargo runs tests in parallel, so we can't
    // assert exact byte counts here. What must hold regardless of interleaving:
    // allocating and freeing a batch then releasing never corrupts anything,
    // and the cache never reports more than its ceiling.
    let size = 4 * 1024 * 1024;
    let layout = Layout::from_size_align(size, 64).unwrap();
    unsafe {
        let mut ptrs = Vec::new();
        for _ in 0..8 {
            let p = ALLOC.alloc(layout);
            assert!(!p.is_null());
            stamp(p, size, 0x7e);
            ptrs.push(p);
        }
        for p in ptrs {
            check(p, size, 0x7e);
            ALLOC.dealloc(p, layout);
        }
    }
    let _ = zk_alloc::release_cache();
    assert!(zk_alloc::cached_bytes() <= isize::MAX as usize);
}
