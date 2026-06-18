//! Error type for the explicit mapping APIs ([`Arena`](crate::Arena),
//! [`SecretBuf`](crate::SecretBuf)).

use std::fmt;

/// A request to the kernel for memory could not be satisfied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapFailed {
    /// Bytes that were requested from the kernel.
    pub bytes: usize,
}

impl fmt::Display for MapFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to map {} bytes from the kernel", self.bytes)
    }
}

impl std::error::Error for MapFailed {}
