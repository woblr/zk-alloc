//! The [`ZkAlloc`] global allocator.
//!
//! It is a thin router. Small requests go to the system allocator, which is
//! already good at them and which we never out-think on a `Box<u8>`. Large
//! requests — the prover's FFT coefficient vectors, MSM bucket arrays, trace
//! columns — go to the resident size-class cache in [`crate::cache`], where the
//! real speedup lives.
//!
//! Routing is by `layout.size()` alone, and `dealloc` sees the same layout it
//! was allocated with, so a freed pointer is classified without any lookup.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr::copy_nonoverlapping;

use crate::cache;
use crate::config::LARGE_THRESHOLD;
use crate::size_class::class_of;

/// A `#[global_allocator]` tuned for zero-knowledge provers on Linux.
///
/// ```no_run
/// use zk_alloc::ZkAlloc;
///
/// #[global_allocator]
/// static GLOBAL: ZkAlloc = ZkAlloc::new();
/// ```
///
/// Set this in your **binary or benchmark**, never in a library: a crate that
/// sets `#[global_allocator]` forces the choice on everything that depends on
/// it, and the allocator can only be set once in a build.
#[derive(Clone, Copy, Debug, Default)]
pub struct ZkAlloc;

impl ZkAlloc {
    /// Construct the allocator. `const`, so it can initialize a `static`.
    #[must_use]
    pub const fn new() -> Self {
        ZkAlloc
    }
}

unsafe impl GlobalAlloc for ZkAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= LARGE_THRESHOLD {
            cache::alloc(layout.size(), layout.align())
        } else {
            System.alloc(layout)
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.size() >= LARGE_THRESHOLD {
            cache::dealloc(ptr, layout.size(), layout.align());
        } else {
            System.dealloc(ptr, layout);
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= LARGE_THRESHOLD {
            cache::alloc_zeroed(layout.size(), layout.align())
        } else {
            System.alloc_zeroed(layout)
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();

        // Both sides small: System may grow in place, so let it try.
        if old_size < LARGE_THRESHOLD && new_size < LARGE_THRESHOLD {
            return System.realloc(ptr, layout, new_size);
        }

        // Both sides large and in the same size class: the existing region
        // already spans the new request, so hand the same pointer back.
        if old_size >= LARGE_THRESHOLD
            && new_size >= LARGE_THRESHOLD
            && class_of(old_size).total == class_of(new_size).total
        {
            return ptr;
        }

        // Anything else (crossing the threshold, or changing class): allocate
        // fresh, copy the overlap, free the old block.
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let new_ptr = self.alloc(new_layout);
        if !new_ptr.is_null() {
            copy_nonoverlapping(ptr, new_ptr, old_size.min(new_size));
            self.dealloc(ptr, layout);
        }
        new_ptr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise the allocator directly without installing it globally, so the
    // test process keeps its normal allocator.
    const ALLOC: ZkAlloc = ZkAlloc::new();

    #[test]
    fn small_roundtrip() {
        let layout = Layout::from_size_align(128, 16).unwrap();
        unsafe {
            let p = ALLOC.alloc(layout);
            assert!(!p.is_null());
            p.write_bytes(0xA5, 128);
            assert_eq!(p.read(), 0xA5);
            ALLOC.dealloc(p, layout);
        }
    }

    #[test]
    fn large_roundtrip_and_reuse() {
        let layout = Layout::from_size_align(2 * 1024 * 1024, 64).unwrap();
        unsafe {
            let a = ALLOC.alloc(layout);
            assert!(!a.is_null());
            assert_eq!(a as usize % 64, 0);
            *a = 1;
            *a.add(layout.size() - 1) = 2;
            ALLOC.dealloc(a, layout);

            // The next same-class request should reuse the very region we freed.
            let b = ALLOC.alloc(layout);
            assert_eq!(a, b, "large free should be cached and reused");
            ALLOC.dealloc(b, layout);
        }
    }

    #[test]
    fn large_alloc_zeroed_clears_reused_region() {
        let layout = Layout::from_size_align(2 * 1024 * 1024, 64).unwrap();
        unsafe {
            let a = ALLOC.alloc(layout);
            std::ptr::write_bytes(a, 0xFF, layout.size());
            ALLOC.dealloc(a, layout);

            let z = ALLOC.alloc_zeroed(layout);
            assert_eq!(z, a, "should reuse the dirtied region");
            for i in 0..layout.size() {
                assert_eq!(*z.add(i), 0, "byte {i} not zeroed");
            }
            ALLOC.dealloc(z, layout);
        }
    }

    #[test]
    fn realloc_same_class_is_in_place() {
        let layout = Layout::from_size_align(1_000_000, 8).unwrap();
        unsafe {
            let p = ALLOC.alloc(layout);
            // 1_000_000 and 1_010_000 round to the same class.
            let q = ALLOC.realloc(p, layout, 1_010_000);
            assert_eq!(p, q);
            ALLOC.dealloc(q, Layout::from_size_align(1_010_000, 8).unwrap());
        }
    }
}
