//! A scoped bump arena for the "allocate a whole proof phase, then throw it all
//! away" pattern.
//!
//! Unlike the global allocator, an [`Arena`] does not reclaim individual frees —
//! it hands out memory by bumping a cursor and reclaims everything at once with
//! [`Arena::reset`], which is O(1). That is exactly right for a witness-
//! generation or commitment phase whose scratch buffers all die together, and
//! it is the fastest allocation a thread can do: one atomic add.
//!
//! The backing region is one huge-page-advised mapping, so the arena's bytes
//! enjoy the same reduced TLB pressure as the large cache.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::{HUGE_PAGE, PAGE};
use crate::error::MapFailed;
use crate::sys;
use crate::sys::round_up;

/// A fixed-capacity bump allocator backed by a single mapping.
///
/// Allocation is lock-free and safe to share across threads; resetting is not —
/// it invalidates every outstanding pointer and must happen when no thread is
/// allocating from or using the arena.
pub struct Arena {
    base: *mut u8,
    capacity: usize,
    cursor: AtomicUsize,
}

impl Arena {
    /// Reserve an arena of at least `bytes`, rounded up to a huge-page boundary.
    pub fn with_capacity(bytes: usize) -> Result<Self, MapFailed> {
        let capacity = round_up(bytes.max(PAGE), HUGE_PAGE);
        let base = sys::map(capacity, HUGE_PAGE);
        if base.is_null() {
            return Err(MapFailed { bytes: capacity });
        }
        Ok(Self {
            base,
            capacity,
            cursor: AtomicUsize::new(0),
        })
    }

    /// Carve out `size` bytes aligned to `align` (a power of two). Returns null
    /// when the arena is full — there is no fallback, by design.
    #[inline]
    pub fn alloc(&self, size: usize, align: usize) -> *mut u8 {
        if size == 0 || !align.is_power_of_two() {
            return std::ptr::null_mut();
        }
        let base = self.base as usize;
        loop {
            let off = self.cursor.load(Ordering::Relaxed);
            let start = match base.checked_add(off) {
                Some(v) => v,
                None => return std::ptr::null_mut(),
            };
            let aligned = (start + (align - 1)) & !(align - 1);
            let new_off = match (aligned - base).checked_add(size) {
                Some(v) => v,
                None => return std::ptr::null_mut(),
            };
            if new_off > self.capacity {
                return std::ptr::null_mut();
            }
            if self
                .cursor
                .compare_exchange_weak(off, new_off, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    /// Allocate space for `count` values of `T`, aligned for `T`. The memory is
    /// uninitialized; the caller must write before reading.
    #[inline]
    pub fn alloc_array<T>(&self, count: usize) -> *mut T {
        match count.checked_mul(std::mem::size_of::<T>()) {
            Some(bytes) if bytes > 0 => self.alloc(bytes, std::mem::align_of::<T>()) as *mut T,
            _ => std::ptr::null_mut(),
        }
    }

    /// Drop every allocation at once by rewinding the cursor. O(1).
    ///
    /// # Safety
    /// Invalidates all pointers handed out since the last reset. No thread may
    /// be allocating from or referencing arena memory during this call.
    #[inline]
    pub unsafe fn reset(&self) {
        self.cursor.store(0, Ordering::Release);
    }

    /// Like [`reset`](Self::reset) but wipes the used bytes first with a
    /// compiler-resistant zero — for arenas that held secret data.
    ///
    /// # Safety
    /// Same as [`reset`](Self::reset).
    #[inline]
    pub unsafe fn secure_reset(&self) {
        let used = self.used();
        if used > 0 {
            sys::secure_zero(self.base, used);
        }
        self.reset();
    }

    /// Bytes handed out since the last reset.
    #[inline]
    #[must_use]
    pub fn used(&self) -> usize {
        self.cursor.load(Ordering::Relaxed)
    }

    /// Total capacity in bytes.
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bytes still available before the arena is full.
    #[inline]
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.capacity - self.used()
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        sys::unmap(self.base, self.capacity);
    }
}

// The cursor is atomic and `base`/`capacity` are immutable after construction.
unsafe impl Send for Arena {}
unsafe impl Sync for Arena {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_and_reset() {
        let arena = Arena::with_capacity(4 * 1024 * 1024).unwrap();
        assert!(arena.capacity() >= 4 * 1024 * 1024);

        let a = arena.alloc(1024, 64);
        assert!(!a.is_null());
        assert_eq!(a as usize % 64, 0);
        assert!(arena.used() >= 1024);

        unsafe { arena.reset() };
        assert_eq!(arena.used(), 0);

        let b = arena.alloc(1024, 64);
        assert_eq!(a, b, "reset should hand back the same memory");
    }

    #[test]
    fn exhaustion_returns_null() {
        let arena = Arena::with_capacity(HUGE_PAGE).unwrap();
        assert!(!arena.alloc(arena.capacity(), 1).is_null());
        assert!(arena.alloc(1, 1).is_null());
    }

    #[test]
    fn typed_array() {
        let arena = Arena::with_capacity(HUGE_PAGE).unwrap();
        let p = arena.alloc_array::<u64>(1000);
        assert!(!p.is_null());
        assert_eq!(p as usize % std::mem::align_of::<u64>(), 0);
        unsafe {
            for i in 0..1000 {
                p.add(i).write(i as u64);
            }
            assert_eq!(p.add(999).read(), 999);
        }
    }

    #[test]
    fn secure_reset_zeroes() {
        let arena = Arena::with_capacity(HUGE_PAGE).unwrap();
        let p = arena.alloc(4096, 8);
        unsafe {
            std::ptr::write_bytes(p, 0xCC, 4096);
            arena.secure_reset();
            // After reset the same bytes are handed back, now zeroed.
            let q = arena.alloc(4096, 8);
            assert_eq!(p, q);
            for i in 0..4096 {
                assert_eq!(*q.add(i), 0);
            }
        }
    }
}
