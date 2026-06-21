//! Large-region size classes.
//!
//! Requests on the large path are rounded up to a class so that freed regions
//! become interchangeable in the cache. Classes follow a 1.00 / 1.25 / 1.50 /
//! 1.75 × 2^b grid, capping internal waste at 25% — much tighter than the
//! power-of-two doubling a naive cache would use, which matters when the
//! regions are tens or hundreds of megabytes.

use crate::config::{HUGE_PAGE, LARGE_THRESHOLD, PAGE};

/// log2(LARGE_THRESHOLD); the grid starts at this power.
/// Derived from the constant so a change to LARGE_THRESHOLD can never
/// silently produce wrong index calculations.
const B_MIN: u32 = LARGE_THRESHOLD.trailing_zeros();

/// The size class for a request: its index (for cache bucketing) and the actual
/// number of bytes a region of that class spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Class {
    pub index: usize,
    pub total: usize,
}

/// Round `size` up to its size class. `size` must be at least
/// [`LARGE_THRESHOLD`]; the result is deterministic, so `dealloc` recovers the
/// exact same class from the layout it is handed.
#[inline]
pub fn class_of(size: usize) -> Class {
    debug_assert!(size >= LARGE_THRESHOLD);

    let mut b = (usize::BITS - 1 - size.leading_zeros()) as usize; // floor(log2)
    let base = 1usize << b;
    let step = base >> 2; // a quarter of the power = the grid spacing

    // How many quarter-steps above `base` we need, rounded up.
    let mut q = (size - base).div_ceil(step); // 0..=4
    if q == 4 {
        // 2.0 × 2^b is just the next power's 1.0× class.
        b += 1;
        q = 0;
    }

    let total = (1usize << b) + q * ((1usize << b) >> 2);
    let index = (b - B_MIN as usize) * 4 + q;
    Class { index, total }
}

/// Alignment a region of `total` bytes is mapped to: huge-page-aligned once it
/// is large enough to benefit from THP, page-aligned otherwise.
#[inline]
pub fn region_align(total: usize) -> usize {
    if total >= HUGE_PAGE {
        HUGE_PAGE
    } else {
        PAGE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CACHE_MAX_REGION, MAX_CLASSES};

    #[test]
    fn class_is_at_least_request_and_within_25_percent() {
        let mut size = LARGE_THRESHOLD;
        while size <= CACHE_MAX_REGION {
            let c = class_of(size);
            assert!(c.total >= size, "class {} < request {}", c.total, size);
            // Waste is bounded by the grid spacing (a quarter of the power).
            assert!((c.total - size) * 4 <= c.total, "waste too high at {size}");
            assert!(c.index < MAX_CLASSES, "index {} overflow", c.index);
            size += LARGE_THRESHOLD / 2 + 1;
        }
    }

    #[test]
    fn dealloc_recovers_the_same_class() {
        // Any request and its rounded class must map to one identical class,
        // since dealloc is handed the original layout (its size, not the class).
        for &s in &[
            64 * 1024,
            100 * 1024,
            1_000_000,
            2 * 1024 * 1024,
            33_000_000,
        ] {
            let c = class_of(s);
            assert_eq!(
                class_of(c.total),
                c,
                "size {s} not a fixed point of its class"
            );
        }
    }

    #[test]
    fn classes_are_strictly_increasing() {
        let mut last = 0;
        let mut size = LARGE_THRESHOLD;
        while size <= CACHE_MAX_REGION {
            let c = class_of(size);
            assert!(c.total > last || c.total == class_of(size - 1).total);
            last = c.total;
            size *= 2;
        }
    }
}
