//! A hardened buffer for secret prover inputs (witnesses).
//!
//! [`SecretBuf`] owns a private mapping that is:
//!
//! - **locked into RAM** (`mlock`), so the secret is never paged to swap;
//! - **fenced by guard pages** on both sides, so an off-by-one read or write
//!   into neighbouring data faults instead of silently leaking or corrupting;
//! - **wiped on drop** with `explicit_bzero`, which the compiler may not elide.
//!
//! It does not protect copies you make elsewhere: a field element read into a
//! local variable lives in registers and on the stack, beyond this buffer's
//! reach. Keep secrets in the buffer as long as you can, and clear stack
//! copies yourself.

use std::ops::{Deref, DerefMut};
use std::slice;

use crate::config::PAGE;
use crate::error::MapFailed;
use crate::sys;
use crate::sys::round_up;

/// A page-aligned, swap-locked, guard-fenced byte buffer that wipes itself when
/// dropped.
pub struct SecretBuf {
    /// Start of the whole mapping, including the leading guard page.
    base: *mut u8,
    /// Total mapped bytes (both guards plus the data pages).
    total: usize,
    /// Usable data pointer (after the leading guard page).
    data: *mut u8,
    /// Usable length requested by the caller.
    len: usize,
    /// Data pages actually locked and wiped (len rounded up to a page).
    data_pages: usize,
    /// Whether the data pages are currently locked into RAM.
    locked: bool,
}

impl SecretBuf {
    /// Allocate a zeroed secret buffer of `len` bytes.
    ///
    /// The mapping is locked into RAM where permitted; locking can fail without
    /// privileges or against `RLIMIT_MEMLOCK`, and that is reported by
    /// [`is_locked`](Self::is_locked) rather than as an error, since the buffer
    /// is still usable and still wiped on drop.
    pub fn new(len: usize) -> Result<Self, MapFailed> {
        let data_pages = round_up(len.max(1), PAGE);
        let total = PAGE + data_pages + PAGE;

        let base = sys::map(total, PAGE);
        if base.is_null() {
            return Err(MapFailed { bytes: total });
        }

        // SAFETY: `base` spans `total` bytes; the guard ranges are within it.
        let data = unsafe { base.add(PAGE) };
        let rear = unsafe { data.add(data_pages) };
        let front_ok = sys::protect_none(base, PAGE);
        let rear_ok = sys::protect_none(rear, PAGE);
        debug_assert!(front_ok, "failed to install leading guard page");
        debug_assert!(rear_ok, "failed to install trailing guard page");

        let locked = sys::lock(data, data_pages);

        Ok(Self {
            base,
            total,
            data,
            len,
            data_pages,
            locked,
        })
    }

    /// Whether the secret pages are locked into RAM (`mlock` succeeded).
    #[inline]
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// The secret as a byte slice.
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `data` is valid for `len` bytes for the buffer's lifetime.
        unsafe { slice::from_raw_parts(self.data, self.len) }
    }

    /// The secret as a mutable byte slice.
    #[inline]
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: see `as_slice`; `&mut self` guarantees exclusive access.
        unsafe { slice::from_raw_parts_mut(self.data, self.len) }
    }

    /// Wipe the secret now, before the buffer is dropped.
    pub fn wipe(&mut self) {
        // SAFETY: `data` is valid for `data_pages` bytes.
        unsafe { sys::secure_zero(self.data, self.data_pages) };
    }
}

impl Deref for SecretBuf {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl DerefMut for SecretBuf {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }
}

impl Drop for SecretBuf {
    fn drop(&mut self) {
        // SAFETY: `data` is valid for `data_pages` bytes until we unmap below.
        unsafe { sys::secure_zero(self.data, self.data_pages) };
        if self.locked {
            sys::unlock(self.data, self.data_pages);
        }
        sys::unmap(self.base, self.total);
    }
}

// The buffer owns its mapping exclusively; moving it across threads is sound,
// and `&mut` gates all mutation.
unsafe impl Send for SecretBuf {}
unsafe impl Sync for SecretBuf {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_zeroed_and_roundtrips() {
        let mut s = SecretBuf::new(1024).unwrap();
        assert!(s.iter().all(|&b| b == 0));
        for (i, b) in s.as_mut_slice().iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        assert_eq!(s[100], 100);
        assert_eq!(s[300], (300 % 251) as u8);
    }

    #[test]
    fn length_is_exact_even_when_unaligned() {
        let s = SecretBuf::new(1000).unwrap();
        assert_eq!(s.len(), 1000);
    }

    #[test]
    fn explicit_wipe_clears() {
        let mut s = SecretBuf::new(64).unwrap();
        s.as_mut_slice().fill(0xAB);
        s.wipe();
        assert!(s.iter().all(|&b| b == 0));
    }
}
