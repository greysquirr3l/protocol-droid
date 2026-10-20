//! `AuthContext` — the identity a visibility rule is evaluated against.
//!
//! # Why this exists at all
//!
//! A registry that lists the same tools to every session is wrong the moment
//! two sessions belong to different principals, and it is wrong *silently*:
//! nothing fails, the tool is simply visible to someone who should not see it.
//! The fix is to make the caller state who is asking, so that "should this
//! session see this primitive" is a question with an answer rather than an
//! assumption.
//!
//! # Why this is a value object and not a mechanism
//!
//! This type is a rule **input**. It carries a principal's identity and the
//! set of scopes they hold, and it knows how to answer "do you hold this set".
//! It does not parse a bearer token, verify a signature, consult an
//! introspection endpoint, or decide *why* a principal holds a scope — every one
//! of those is T16's OAuth work, and every one of them belongs in an adapter
//! that produces this value rather than here. The split is the same one the
//! project already draws for [`Clock`](crate::application::session::clock::Clock): the
//! decision consumes the reading, it never takes it.
//!
//! # Default deny
//!
//! [`ScopeSet::holds`] is a subset test, so a principal holding nothing sees
//! nothing that requires anything. A primitive with no required scopes is
//! visible to every authenticated principal, which is the one case where "no
//! rule" means "allow" — and it is expressed as an empty subset rather than as
//! a special case, so there is no second path through the predicate.

use core::fmt;
use std::sync::Arc;

/// A permission a principal may hold.
///
/// Deliberately not a hierarchy. `Read` does not imply `Write` and `Write` does
/// not imply `Admin`: a primitive states the exact set it needs, and the
/// principal states the exact set it holds. A hierarchy would make the visible
/// set depend on an ordering the specification does not define, and would make
/// a missing rule resolve by accident of declaration order rather than by
/// intent.
///
/// T16 replaces this with whatever the peer's token actually carries. The
/// visibility rule in [`super::visibility`] is written against the subset test
/// and will not need to change when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Reading a primitive: listing it, and calling one that changes nothing.
    Read,
    /// Mutating through a primitive: calling a tool that writes.
    Write,
    /// Administration: primitives that alter the server's own configuration.
    Admin,
}

impl Scope {
    /// Every scope, in declaration order.
    pub const ALL: [Self; 3] = [Self::Read, Self::Write, Self::Admin];

    /// The name used in a deny message and in tracing.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }

    const fn bit(self) -> u128 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
            Self::Admin => 4,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The scopes a principal holds.
///
/// A `u128` bitset for the same reason [`FeatureProfile`](crate::domain::FeatureProfile)
/// is one: the set is copied into every visibility decision, so it has to be
/// `Copy` and eight bytes of comparison beats a `HashSet` on a path that runs
/// once per primitive per list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ScopeSet {
    bits: u128,
}

impl ScopeSet {
    /// No scopes. The default, so a context built without a set denies
    /// everything that requires one.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Every scope.
    #[must_use]
    pub const fn all() -> Self {
        Self { bits: 0b111 }
    }

    /// Adds a scope.
    #[must_use]
    pub const fn with(self, scope: Scope) -> Self {
        Self {
            bits: self.bits | scope.bit(),
        }
    }

    /// Whether the scope is held.
    #[must_use]
    pub const fn contains(self, scope: Scope) -> bool {
        self.bits & scope.bit() != 0
    }

    /// Whether no scope is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Whether every scope in `required` is held.
    ///
    /// The direction matters: this is "does the principal have everything the
    /// primitive needs", and the subset relation is what makes an empty
    /// requirement visible to everyone while a single missing scope hides the
    /// primitive from everyone.
    #[must_use]
    pub const fn holds(self, required: Self) -> bool {
        self.bits & required.bits == required.bits
    }

    /// The scopes held but not required, and the reverse — for a message that
    /// says which side was short.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
        }
    }
}

/// Who is asking, and what they hold.
///
/// Built once per request at the transport boundary from whatever the peer
/// proved, and then read — never mutated — by the visibility predicate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthContext {
    principal: Arc<str>,
    scopes: ScopeSet,
}

impl AuthContext {
    /// An unauthenticated principal holding nothing.
    ///
    /// Distinct from [`AuthContext::new`] with a name and no scopes in exactly
    /// one way, and it is worth being explicit about the difference: an
    /// anonymous caller is a peer that has not presented credentials, while a
    /// named principal holding nothing is a peer that presented credentials
    /// which granted nothing. Both see the same primitives. The name differs so
    /// a trace can say which happened.
    #[must_use]
    pub fn anonymous() -> Self {
        Self {
            principal: Arc::from("anonymous"),
            scopes: ScopeSet::empty(),
        }
    }

    /// A named principal holding `scopes`.
    #[must_use]
    pub fn new(principal: impl Into<Arc<str>>, scopes: ScopeSet) -> Self {
        Self {
            principal: principal.into(),
            scopes,
        }
    }

    /// A principal holding every scope. For a server with no authorization of
    /// its own, which is the common case before T16.
    #[must_use]
    pub fn unrestricted(principal: impl Into<Arc<str>>) -> Self {
        Self::new(principal, ScopeSet::all())
    }

    /// Who is asking.
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// What they hold.
    #[must_use]
    pub const fn scopes(&self) -> ScopeSet {
        self.scopes
    }

    /// Whether the principal holds every scope in `required`.
    #[must_use]
    pub const fn holds(&self, required: ScopeSet) -> bool {
        self.scopes.holds(required)
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthContext, Scope, ScopeSet};

    /// The default must deny. A context that accidentally defaulted to
    /// permissive would make every visibility rule a no-op, and nothing else
    /// in the tree would catch it.
    #[test]
    fn the_default_holds_nothing() {
        let context = AuthContext::default();
        assert!(!context.holds(ScopeSet::empty().with(Scope::Read)));
        assert!(context.holds(ScopeSet::empty()));
    }

    /// A subset test, not a hierarchy: `Write` does not imply `Admin`, because
    /// a primitive that needs `Admin` was written to be reachable by nobody but
    /// an administrator and a hierarchy would quietly widen that.
    #[test]
    fn scopes_do_not_imply_one_another() {
        let write = ScopeSet::empty().with(Scope::Write);
        assert!(!write.contains(Scope::Admin));
        assert!(!write.contains(Scope::Read));
        assert!(!write.holds(ScopeSet::all()));
    }

    #[test]
    fn holds_is_the_subset_relation() {
        let admin = ScopeSet::empty().with(Scope::Admin);
        let read_write_admin = ScopeSet::empty()
            .with(Scope::Read)
            .with(Scope::Write)
            .with(Scope::Admin);
        assert!(read_write_admin.holds(admin));
        assert!(admin.holds(admin));
        assert!(!admin.holds(read_write_admin));
        // An empty requirement is held by everyone, including nobody.
        assert!(admin.holds(ScopeSet::empty()));
    }

    /// `difference` is used to say which side was short. Getting the direction
    /// wrong would print the held scopes and hide the missing ones, which is
    /// the one fact the message exists to convey.
    #[test]
    fn difference_reports_what_is_missing() {
        let missing = ScopeSet::empty()
            .with(Scope::Read)
            .with(Scope::Write)
            .difference(ScopeSet::empty().with(Scope::Read));
        assert!(missing.contains(Scope::Write));
        assert!(!missing.contains(Scope::Read));
    }

    #[test]
    fn anonymous_and_named_are_both_denial_by_default() {
        let anonymous = AuthContext::anonymous();
        assert_eq!(anonymous.principal(), "anonymous");
        assert!(!anonymous.holds(ScopeSet::empty().with(Scope::Read)));

        let named = AuthContext::new("alice", ScopeSet::empty());
        assert_eq!(named.principal(), "alice");
        assert!(!named.holds(ScopeSet::empty().with(Scope::Read)));
    }

    #[test]
    fn unrestricted_holds_everything() {
        let context = AuthContext::unrestricted("bob");
        assert!(context.holds(ScopeSet::all()));
    }

    /// Every scope needs a distinct bit, or two scopes would share one and
    /// holding one would silently grant the other.
    #[test]
    fn every_scope_has_its_own_bit() {
        for scope in Scope::ALL {
            let set = ScopeSet::empty().with(scope);
            assert!(set.contains(scope), "{scope} missing from its own set");
            for other in Scope::ALL {
                if other != scope {
                    assert!(!set.contains(other), "bit for {scope} also grants {other}");
                }
            }
        }
    }
}
