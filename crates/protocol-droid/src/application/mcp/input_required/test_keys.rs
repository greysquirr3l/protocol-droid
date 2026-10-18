//! Keys for tests, drawn fresh for each process.
//!
//! A key written into a test as a literal is a hard-coded cryptographic value,
//! whatever it is used for, and it teaches the next reader to copy the pattern.
//! These come from the operating system's random source through the standard
//! library's hasher seed, so no test depends on a particular key.

use std::hash::{BuildHasher as _, Hasher as _, RandomState};

use super::StateKey;

/// 32 bytes no two processes share.
#[must_use]
pub fn random_bytes() -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    for chunk in bytes.chunks_mut(8) {
        let word = RandomState::new().build_hasher().finish().to_le_bytes();
        for (slot, byte) in chunk.iter_mut().zip(word) {
            *slot = byte;
        }
    }
    bytes
}

/// A key that differs from every other call's.
#[must_use]
pub fn random_key() -> StateKey {
    StateKey::from_bytes(random_bytes())
}
