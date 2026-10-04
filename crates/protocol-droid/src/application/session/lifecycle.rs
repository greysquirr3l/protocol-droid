//! The protocol's half of the session engine: what a message means, and what
//! it was agreed on.
//!
//! # Why the trait has two methods
//!
//! [`classify`](Lifecycle::classify) and [`resolve`](Lifecycle::resolve) are the
//! only two questions the engine ever asks a protocol, and they are asked
//! together for a reason: a protocol that answered "this method is permitted"
//! with one rule and "this method opens the handshake" with another could
//! disagree, and the disagreement would show up as a session whose phase is
//! wrong while every individual answer looks reasonable. One method that
//! classifies, and one that reads a declaration, cannot be split apart without
//! the engine having to reconcile two sources of truth.
//!
//! # The two associated types, and why the engine cannot look inside them
//!
//! `Profile` is MCP's `FeatureProfile`, a u128 bitset, and would be A2A's
//! `Major.Minor` pair. `Version` is a date for MCP and something else
//! entirely for A2A. The engine stores both opaquely and never inspects
//! either: it passes a `Profile` back to the lifecycle whenever it needs a
//! decision, and renders a `Version` into a string for the event log. That is
//! what keeps this module free of MCP types, and it is the same property T09's
//! `MethodTable` has — a protocol drops in as an implementation rather than as
//! a fork.
//!
//! # Implementing it for a protocol with no handshake
//!
//! A2A has none: every request carries an `A2A-Version` header and there is
//! nothing to initialise. The sketch below is the whole of it, and it is a
//! doc-test rather than a stub in the tree so that nobody has to keep a
//! half-implemented protocol compiling. Note what it does *not* need: a
//! version list, a capability set, a transport, or any of the machinery MCP
//! requires, because none of those concepts are referenced here.

use serde_json::Value;

use crate::domain::session::SessionState;

/// What a message naming a method means for a session in a given state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Ordinary work. Admitted only if [`SessionState::admits_ordinary_work`]
    /// holds and the protocol does not narrow it further.
    Ordinary,
    /// The peer is opening a handshake.
    HandshakeStart,
    /// The peer is confirming that the handshake landed.
    HandshakeConfirm,
    /// Not permitted here, whatever the method is.
    Refused,
}

/// What a peer declared about itself, in this crate's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Declaration<'a> {
    /// A handshake request: the version it asked for, and whatever the
    /// protocol also puts in one.
    Handshake {
        /// The requested version, as the peer spelled it. Parsed here rather
        /// than by the engine, because a version this protocol cannot parse is
        /// a statement about *its* vocabulary.
        version: &'a str,
        /// The capabilities the peer advertised, if this protocol has any.
        capabilities: Option<&'a Value>,
    },
    /// A message that negotiates for itself: the reserved keys its metadata
    /// carried, and nothing else.
    Stateless {
        /// The metadata the request carried.
        meta: MetaView<'a>,
    },
}

/// The reserved metadata a request carried.
///
/// A borrowed view over an already-parsed object, because the transport
/// boundary is where JSON is parsed and nothing downstream of it should meet a
/// raw string. A protocol that reserves no keys is handed an empty view and
/// answers `None`; one that reserves several picks the ones it defines and
/// ignores the rest, because a peer sending a key this protocol has not heard
/// of is a peer running a newer draft, not a peer that sent nonsense.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetaView<'a> {
    object: Option<&'a serde_json::Map<String, Value>>,
}

impl<'a> MetaView<'a> {
    /// A view over a parsed metadata object.
    #[must_use]
    pub const fn new(object: Option<&'a serde_json::Map<String, Value>>) -> Self {
        Self { object }
    }

    /// The value at `key`, if the request carried one.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.object.and_then(|object| object.get(key))
    }

    /// Whether the request carried no metadata at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.object.is_none()
    }
}

/// What a protocol answered to a declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution<P, V> {
    /// What this session may now do, in the protocol's own type.
    pub profile: P,
    /// The version the session speaks, in the protocol's own type.
    pub version: V,
    /// Whether the peer asked for this version and got it, or was answered with
    /// a different one.
    pub echoed: bool,
}

/// Why a session could not be established.
///
/// Deliberately smaller than the engine's own error: these are the cases where
/// the *protocol's* answer is "I cannot", and the engine adds its own state
/// errors around them. The distinction matters because only one of the two is
/// something a peer can fix by asking for something different.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeclarationError {
    /// The peer named a version this side cannot parse.
    #[error("{declared:?} is not a version this protocol speaks")]
    Unparseable {
        /// What the peer sent, verbatim. It is a peer's own token, and the
        /// message is what lets an operator see the spelling that failed.
        declared: String,
    },
    /// A message declared no version, and there is no session to fall back to.
    ///
    /// The distinction from a missing header on a *stateful* session is the
    /// whole point of the stateless lifecycle: with nothing established, there
    /// is nothing to assume from, so an absent version is an error rather than
    /// a default. Answering with a guess here is how a stateless server ends
    /// up serving a modern peer a 2024 profile.
    #[error("the request declared no protocol version and no session is established")]
    NoVersion,
}

/// The vocabulary one protocol brings to the session engine.
///
/// Declared by the engine, which is its only consumer, and implemented by
/// whichever protocol module is in play. See the module docs for why it is two
/// methods and two associated types rather than a wider interface.
///
/// # Examples
///
/// A protocol with no handshake, which is what A2A's per-request version header
/// amounts to. Nothing in this module changes for it.
///
/// ```
/// use protocol_droid::application::session::{
///     Classification, Declaration, DeclarationError, Lifecycle, MetaView, Resolution,
/// };
/// use protocol_droid::domain::session::SessionState;
///
/// /// A2A speaks `Major.Minor`; MCP speaks dates. Nothing here assumes either.
/// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// struct MajorMinor(u8, u8);
///
/// impl std::fmt::Display for MajorMinor {
///     fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         write!(formatter, "{}.{}", self.0, self.1)
///     }
/// }
///
/// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// struct A2AProfile {
///     version: MajorMinor,
/// }
///
/// struct A2ALifecycle {
///     supported: Vec<MajorMinor>,
/// }
///
/// impl Lifecycle for A2ALifecycle {
///     type Profile = A2AProfile;
///     type Version = MajorMinor;
///
///     fn classify(
///         &self,
///         state: SessionState,
///         _profile: Option<Self::Profile>,
///         _method: &str,
///     ) -> Classification {
///         // No handshake, so nothing is ever a handshake — and the engine
///         // does not need to be told what "initialize" means.
///         let _ = state;
///         Classification::Ordinary
///     }
///
///     fn resolve(&self, declaration: Declaration<'_>) -> Result<Resolution<Self::Profile, Self::Version>, DeclarationError> {
///         // A2A negotiates out of its own metadata rather than a version
///         // string, so both arms are read but neither is the version. That
///         // difference is the point: the engine never supplies it.
///         match declaration {
///             Declaration::Stateless { .. } | Declaration::Handshake { .. } => {}
///         }
///         let version = self.supported.first().copied().ok_or(DeclarationError::NoVersion)?;
///         Ok(Resolution {
///             profile: A2AProfile { version },
///             version,
///             echoed: true,
///         })
///     }
/// }
///
/// let lifecycle = A2ALifecycle { supported: vec![MajorMinor(0, 3)] };
/// let resolution = lifecycle
///     .resolve(Declaration::Stateless { meta: MetaView::default() })
///     .expect("a supported version is answered");
/// assert_eq!(resolution.version, MajorMinor(0, 3));
/// assert_eq!(
///     lifecycle.classify(SessionState::Stateless, Some(resolution.profile), "message/send"),
///     Classification::Ordinary,
/// );
/// ```
pub trait Lifecycle {
    /// What this protocol calls the set of things a session may do.
    ///
    /// Opaque to the engine. `FeatureProfile` for MCP, which the engine also
    /// never inspects — it is handed back to [`classify`](Self::classify).
    type Profile: Copy + core::fmt::Debug + PartialEq;

    /// How this protocol spells a version.
    type Version: Copy + core::fmt::Display + PartialEq;

    /// What a message naming `method` means for a session in `state`.
    ///
    /// `profile` is `None` before anything has been agreed. A protocol that
    /// gates a method on a feature has to say so here rather than by
    /// inspecting a version, because the version is not this engine's to
    /// interpret.
    ///
    /// The engine refuses a message in a closing or closed session whatever
    /// this answers, so a protocol does not have to repeat that check.
    fn classify(
        &self,
        state: SessionState,
        profile: Option<Self::Profile>,
        method: &str,
    ) -> Classification;

    /// Reads what a peer declared and answers with what this side agrees to.
    ///
    /// # Errors
    ///
    /// [`DeclarationError`] when the declaration is not one this protocol can
    /// turn into a profile. Never for an *unsupported but well-formed* version:
    /// answering with something else is the specified behaviour, and an error
    /// there would stop a server working the day a revision is published.
    fn resolve(
        &self,
        declaration: Declaration<'_>,
    ) -> Result<Resolution<Self::Profile, Self::Version>, DeclarationError>;
}
