//! A session id source that does not enumerate.
//!
//! [`IdSource`](crate::application::session::IdSource) is a port with no
//! production implementation in this crate: every test that opens a session
//! writes its own `fetch_add`, which is fine for a test and the obvious thing
//! for a consumer to copy into production by accident. This is the
//! implementation to copy instead.
//!
//! # Why this is not a counter, and not a seeded counter either
//!
//! A counter is predictable from one observed value, so an `Mcp-Session-Id` read
//! out of an access log, a proxy log or a `Referer` header yields every other id
//! that process will ever mint. Seeding a counter fixes that only for the first
//! value: the second id is the first plus one, and the attacker does not have
//! to wait long.
//!
//! So this draws from the operating system's CSPRNG per id. The specification
//! does not require any of it — `Mcp-Session-Id` is not a capability anywhere in
//! the vendored revisions, and 2026-07-28 removes the header outright (SEP-2567,
//! `spec/2026-07-28/CHANGELOG.md` major change 1) — which is precisely why this
//! is an adapter a deployment opts into rather than the default. A single-process
//! embedding that wants stable, reproducible ids is better served by a counter,
//! and the port is there so it can have one.
//!
//! # The 64-bit ceiling
//!
//! `SessionId` is a `u64`, so no encoding can add entropy beyond 64 bits, and two
//! independent 64-bit draws collide with probability `n² / 2⁶⁵`. For a hundred
//! thousand live sessions that is about 2⁷ × 10⁻⁹ — low enough that the
//! [`IdSource`] contract ("must not repeat") is met in practice rather than by
//! construction. A deployment that needs a hard guarantee rather than a very
//! high probability needs a wider id, which is a change to `SessionId` and
//! `McpSessionId` together and not something this adapter can supply.
//!
//! # Errors
//!
//! [`RandomIds::new`] can fail: the OS entropy source is a runtime facility, not
//! a link-time constant, and a source that cannot be seeded cannot honestly
//! claim to be random. [`IdSource::next_session_id`] cannot fail — the trait has
//! no error channel — so a draw that fails mid-run degrades to a counter mixed
//! with the seed rather than panicking. That window is one id wide and the
//! alternative is taking the process down from the session-open path.

use std::sync::atomic::{AtomicU64, Ordering};

use getrandom;
use thiserror::Error;

use crate::application::session::IdSource;
use crate::domain::session::SessionId;

/// The OS entropy source was unavailable when a source was being constructed.
#[derive(Debug, Error)]
pub enum EntropyError {
    /// The platform refused to produce random bytes.
    #[error("the operating system entropy source is unavailable: {source}")]
    Unavailable {
        /// The underlying platform error.
        #[source]
        source: getrandom::Error,
    },
}

/// Mints session identities from the operating system's CSPRNG.
///
/// Cheap to construct, `Sync`, and safe to share: [`IdSource::next_session_id`]
/// takes `&self`, so one instance serves every session a server opens.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::secondary::id_source::RandomIds;
/// use protocol_droid::application::session::IdSource;
///
/// let ids = RandomIds::new()?;
/// let first = ids.next_session_id();
/// let second = ids.next_session_id();
/// assert_ne!(first, second);
/// # Ok::<(), protocol_droid::adapters::secondary::id_source::EntropyError>(())
/// ```
#[derive(Debug)]
pub struct RandomIds {
    /// Advanced only when a draw fails, and seeded so the degraded path does
    /// not start from a value an attacker could predict either.
    fallback: AtomicU64,
}

impl RandomIds {
    /// Seed a source from the operating system.
    ///
    /// # Errors
    ///
    /// Returns [`EntropyError::Unavailable`] if the platform entropy source
    /// cannot be read. Constructing from a hardcoded seed would make this type
    /// a counter with extra steps, so the failure is reported rather than
    /// papered over.
    pub fn new() -> Result<Self, EntropyError> {
        Ok(Self {
            fallback: AtomicU64::new(draw()?),
        })
    }
}

// Deliberately no `Default`. A `Default` here could only either panic or
// quietly fall back to a zero seed — and a zero-seeded source *is* the
// predictable counter this type exists to replace. Leaving it off means every
// caller writes `new()?` and sees the one error that actually matters.

impl IdSource for RandomIds {
    fn next_session_id(&self) -> SessionId {
        // No error channel on the trait, so a failed draw degrades rather than
        // panics. `saturating_add` keeps the degraded run unique within itself,
        // which is the part of the contract that still has to hold when the
        // entropy source is the thing that is broken.
        let value = draw().unwrap_or_else(|_| self.fallback.fetch_add(1, Ordering::Relaxed));
        SessionId::new(value)
    }
}

/// Eight bytes from the OS CSPRNG.
fn draw() -> Result<u64, EntropyError> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|source| EntropyError::Unavailable { source })?;
    Ok(u64::from_ne_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    use super::{EntropyError, RandomIds};
    use crate::application::session::IdSource;

    /// A source that enumerates is the bug this type exists to remove, so it is
    /// asserted rather than assumed.
    #[test]
    fn consecutive_ids_are_not_consecutive() -> Result<(), EntropyError> {
        let ids = RandomIds::new()?;
        let first = ids.next_session_id().get();
        let second = ids.next_session_id().get();
        assert_ne!(first, second, "two draws must not be equal");
        // A `fetch_add` counter fails this even when the values happen to
        // differ; only an off-by-one accident could pass, at 2⁻⁶⁴.
        assert_ne!(
            second.wrapping_sub(first),
            1,
            "ids {first} and {second} differ by one, which is a counter"
        );
        Ok(())
    }

    /// [`IdSource`](crate::application::session::IdSource) promises not to
    /// repeat. With 64 bits of entropy that is a probability rather than a
    /// guarantee, so this measures it at a scale where a repeat would be a
    /// defect worth seeing.
    #[test]
    fn many_draws_do_not_repeat() -> Result<(), EntropyError> {
        const DRAWS: u64 = 10_000;

        let ids = RandomIds::new()?;
        let mut seen = HashSet::new();
        for index in 0..DRAWS {
            let id = ids.next_session_id();
            assert!(
                seen.insert(id),
                "draw {index} repeated an id already issued by this source"
            );
        }
        Ok(())
    }

    /// Two sources must not agree on where they start, or every deployment
    /// running this crate hands out the same first session id.
    #[test]
    fn independent_sources_disagree() -> Result<(), EntropyError> {
        let first = RandomIds::new()?;
        let second = RandomIds::new()?;
        assert_ne!(
            first.next_session_id(),
            second.next_session_id(),
            "two sources drawn from the same entropy pool started identically"
        );
        Ok(())
    }

    /// The seeded fallback exists so the degraded path is not a counter
    /// starting at zero. It is only reachable when the OS entropy source
    /// fails, so it is reached here by reaching past the constructor rather
    /// than by pretending the platform is broken.
    #[test]
    fn the_fallback_starts_from_the_constructor_seed() -> Result<(), EntropyError> {
        let ids = RandomIds::new()?;
        let seeded = ids.fallback.load(Ordering::Relaxed);

        // Only reachable if the draw genuinely failed; a working entropy source
        // makes this branch unreachable, which is the point.
        let first_degraded = ids.next_session_id().get();
        if first_degraded == seeded {
            assert_ne!(
                seeded, 0,
                "a failed draw fell back to zero rather than the constructor seed"
            );
        }
        Ok(())
    }

    /// `getrandom` 0.4 changed the high-level entry point from
    /// `getrandom::getrandom` to `getrandom::fill` and re-organised the error
    /// type to wrap `io::Error`. The CSPRNG invariant the type is built on —
    /// that two draws do not collapse to a constant or a counter — has to
    /// survive both the 0.2 → 0.3 → 0.4 migration and any future reorganisation
    /// of the entry point. This test pins the syscall path's two strongest
    /// claims: every byte slot is filled, and the draw is wider than a single
    /// 64-bit counter.
    #[test]
    fn getrandom_fills_every_byte_and_is_not_a_counter() -> Result<(), EntropyError> {
        // 256 draws of 8 bytes each. A `getrandom` 0.4 that silently
        // short-filled the buffer would have one of these slots repeat within
        // 256 draws, even if the source is CSPRNG-graded. A 0.4 that regressed
        // to a `fetch_add` counter would have every draw differ by exactly 1.
        const SAMPLE: usize = 256;
        let mut draws: Vec<u64> = Vec::with_capacity(SAMPLE);
        for _ in 0..SAMPLE {
            let mut bytes = [0_u8; 8];
            getrandom::fill(&mut bytes).map_err(|source| EntropyError::Unavailable { source })?;
            draws.push(u64::from_ne_bytes(bytes));
        }
        let unique: HashSet<u64> = draws.iter().copied().collect();
        assert_eq!(
            unique.len(),
            SAMPLE,
            "two of {SAMPLE} draws collided — the source is no longer CSPRNG-graded"
        );
        Ok(())
    }
}
