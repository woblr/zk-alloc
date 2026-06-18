//! Thin wrappers over the Linux virtual-memory syscalls we rely on.
//!
//! Everything here is anonymous private memory. The interesting policy choices
//! live in the callers; this module just makes the syscalls safe to call and
//! reports failure as a null pointer or a `bool` rather than an errno.

use crate::config::{HUGE_PAGE, PAGE};

/// Map `len` bytes of fresh anonymous memory aligned to `align`.
///
/// `align` must be a power of two. For `align <= PAGE` the kernel's own
/// page-alignment is enough and we map exactly `len`. For larger alignment we
/// over-map, carve out an aligned window, and unmap the slack — the standard
/// trick, since `mmap` only promises page alignment.
///
/// Regions aligned to a huge page are advised `MADV_HUGEPAGE` so the kernel can
/// fault them in as transparent huge pages, cutting TLB pressure on the big
/// FFT/MSM buffers that dominate a prover's footprint.
///
/// Returns a null pointer on failure.
pub fn map(len: usize, align: usize) -> *mut u8 {
    debug_assert!(align.is_power_of_two());
    debug_assert!(len % PAGE == 0);

    if align <= PAGE {
        let p = raw_map(len);
        if !p.is_null() && align >= HUGE_PAGE {
            advise_hugepage(p, len);
        }
        return p;
    }

    // Over-map by one alignment so an aligned start is guaranteed to fit.
    let over = len + align;
    let base = raw_map(over);
    if base.is_null() {
        return std::ptr::null_mut();
    }

    let addr = base as usize;
    let aligned = (addr + align - 1) & !(align - 1);
    let head = aligned - addr;
    if head > 0 {
        unsafe { libc::munmap(base as *mut libc::c_void, head) };
    }
    let tail = over - head - len;
    if tail > 0 {
        unsafe { libc::munmap((aligned + len) as *mut libc::c_void, tail) };
    }

    let p = aligned as *mut u8;
    if align >= HUGE_PAGE {
        advise_hugepage(p, len);
    }
    p
}

/// Unmap a region previously returned by [`map`].
#[inline]
pub fn unmap(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        libc::munmap(ptr as *mut libc::c_void, len);
    }
}

#[inline]
fn raw_map(len: usize) -> *mut u8 {
    // MAP_NORESERVE: don't account swap up front. The cache reserves a lot of
    // address space it may never touch; physical pages arrive on first write.
    // Huge-page backing comes from THP (madvise) in `map`, not MAP_HUGETLB —
    // it needs no reserved pool and can't SIGBUS on fault.
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;

    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            flags,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        std::ptr::null_mut()
    } else {
        p as *mut u8
    }
}

#[inline]
fn advise_hugepage(ptr: *mut u8, len: usize) {
    unsafe {
        libc::madvise(ptr as *mut libc::c_void, len, libc::MADV_HUGEPAGE);
    }
}

/// Drop a range's physical pages now. Subsequent reads of anonymous private
/// memory return zero-fill-on-demand, so this is a cheap lazy re-zero: pages the
/// caller never touches are never faulted, unlike an eager memset.
#[inline]
pub fn advise_dontneed(ptr: *mut u8, len: usize) {
    unsafe {
        libc::madvise(ptr as *mut libc::c_void, len, libc::MADV_DONTNEED);
    }
}

/// Hint that a cached region's pages may be reclaimed under memory pressure,
/// without dropping RSS immediately. If the region is reused before the kernel
/// reclaims, the hint is cancelled on the first write and no re-zeroing or
/// re-faulting happens — the "warm" tier between resident and released.
#[inline]
pub fn advise_free(ptr: *mut u8, len: usize) {
    unsafe {
        libc::madvise(ptr as *mut libc::c_void, len, libc::MADV_FREE);
    }
}

/// Lock a range into RAM so sensitive data is never written to swap.
#[inline]
pub fn lock(ptr: *mut u8, len: usize) -> bool {
    unsafe { libc::mlock(ptr as *const libc::c_void, len) == 0 }
}

/// Release a lock taken with [`lock`].
#[inline]
pub fn unlock(ptr: *mut u8, len: usize) -> bool {
    unsafe { libc::munlock(ptr as *const libc::c_void, len) == 0 }
}

/// Make a range inaccessible (a guard page). Any access faults.
#[inline]
pub fn protect_none(ptr: *mut u8, len: usize) -> bool {
    unsafe { libc::mprotect(ptr as *mut libc::c_void, len, libc::PROT_NONE) == 0 }
}

/// Overwrite `len` bytes at `ptr` with zero in a way the compiler may not elide.
///
/// `explicit_bzero` exists precisely so dead-store elimination cannot drop the
/// wipe of a buffer that is never read again — the situation when clearing
/// secret witness data before the memory is recycled.
///
/// # Safety
/// `ptr` must be valid for writes of `len` bytes.
#[inline]
pub unsafe fn secure_zero(ptr: *mut u8, len: usize) {
    extern "C" {
        fn explicit_bzero(s: *mut libc::c_void, n: libc::size_t);
    }
    explicit_bzero(ptr as *mut libc::c_void, len);
}
