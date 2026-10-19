//! The `initialize` exchange, as both a server and a bridge read it.
//!
//! This is shared because two callers genuinely need it and neither owns it.
//! A server answers a client's handshake. A bridge is the server on its
//! downstream leg, so it reads the same request and answers the same way —
//! with the difference that the capabilities it advertises are the
//! _intersection_ of what the client asked for and what the upstream server
//! agreed to, rather than whatever the local process can do.
//!
//! Putting the parsing here rather than in either caller is what keeps that
//! difference from becoming a second reading of the wire format. The
//! capability tables in [`crate::domain`] map a Rust name to a JSON path, and
//! two copies of that mapping would be two definitions of what a client is
//! allowed to say.
//!
//! # What is not here
//!
//! Choosing a revision is not in this module. Reading an offer is a wire
//! format question; agreeing on a version is a negotiation question, and the
//! two answer to different inputs — see
//! [`agree`](crate::domain::negotiation::negotiate_server) for the server and
//! [`crate::application::bridge::downstream`] for the bridge.

use crate::domain::feature::FeatureProfile;
use crate::domain::ir::JsonObject;
use crate::domain::{
    CLIENT_CAPABILITY_WIRE, CapabilitySet, ProtocolVersion, SERVER_CAPABILITY_WIRE,
};

/// What a client said in its `initialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The revision the client asked for.
    ///
    /// An [`Offer`], not a choice: the client naming a revision this build has
    /// never heard of is normal and is answered with a counter-offer, not a
    /// refusal.
    pub version: ProtocolVersion,
    /// What the client said it supported, read through the capability tables.
    pub capabilities: CapabilitySet,
    /// The client's own name, which is echoed nowhere but is worth keeping for
    /// an operator reading a report.
    pub name: String,
}

/// Why an `initialize` could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InitializeError {
    /// The request named nothing usable.
    ///
    /// One variant for every failure rather than one per field, because no
    /// caller can act on the difference: every one of them is answered to the
    /// peer as an invalid request, and splitting them would produce five
    /// variants that are always mapped to the same thing.
    #[error("initialize params were missing or malformed")]
    Malformed,
}

/// Reads an `initialize` request body.
///
/// # Errors
///
/// [`InitializeError::Malformed`] when the body is not an object, when
/// `protocolVersion` is absent, is not a string, or does not parse as a
/// protocol version; when `capabilities` is absent or is not an object; or
/// when `clientInfo` is absent, is not an object, or has no `name`.
///
/// `clientInfo.version` is the one required field not enforced. It is
/// free-form, nothing in this crate's answer depends on it, and refusing a
/// handshake over a missing version string would buy a stricter reading of one
/// schema at the cost of every client that omits it.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::mcp::initialize::read_offer;
/// use protocol_droid::domain::ir::JsonObject;
///
/// let params = JsonObject::new(
///     r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe"}}"#,
/// )?;
/// let offer = read_offer(&params)?;
/// assert_eq!(offer.name, "probe");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn read_offer(params: &JsonObject) -> Result<Offer, InitializeError> {
    let parsed: serde_json::Value =
        serde_json::from_str(params.as_str()).map_err(|_| InitializeError::Malformed)?;
    let object = parsed.as_object().ok_or(InitializeError::Malformed)?;

    let offered = object
        .get("protocolVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or(InitializeError::Malformed)?
        .parse::<ProtocolVersion>()
        .map_err(|_| InitializeError::Malformed)?;

    let capabilities = object
        .get("capabilities")
        .and_then(serde_json::Value::as_object)
        .ok_or(InitializeError::Malformed)?;

    let client = object
        .get("clientInfo")
        .filter(|value| value.is_object())
        .ok_or(InitializeError::Malformed)?;
    let name = client
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or(InitializeError::Malformed)?;

    Ok(Offer {
        version: offered,
        capabilities: client_capabilities(capabilities),
        name: name.to_owned(),
    })
}

/// Reads a `ClientCapabilities` object as a [`CapabilitySet`].
///
/// Public because SEP-2575's per-request `_meta` carries the same object in a
/// different message, and reading it twice with two tables would be two
/// definitions of what a client is allowed to say. See
/// [`crate::application::mcp::request_meta`].
///
/// The inverse of the client's own renderer, and table-driven for the same
/// reason: [`Capability::as_str`] is a dot in a Rust name and two levels in
/// JSON, so a capability advertised under the wrong key is one this cannot see.
/// A peer that sent nothing is not an error — it advertised nothing, and an
/// empty set is what that means.
///
/// An unrecognised key is ignored rather than refused. The specification's
/// extensibility point is `experimental`, and a client built against a later
/// revision may name capabilities this build has never heard of; refusing the
/// handshake over one would make every future client unconnectable.
///
/// [`Capability::as_str`]: crate::domain::Capability::as_str
pub fn client_capabilities(object: &serde_json::Map<String, serde_json::Value>) -> CapabilitySet {
    let mut set = CapabilitySet::empty();
    for (capability, path) in CLIENT_CAPABILITY_WIRE {
        if path_present(object, path) {
            set.insert(*capability);
        }
    }
    set
}

/// Whether every key on `path` is present, as an object where the schema gives
/// one and a terminal key at the end.
///
/// The mirror of the client's `insert_path`: a parent capability's presence is
/// the claim, and the schema gives `roots` an object rather than a boolean, so
/// `roots: {}` is the wire form and `roots: true` is not.
fn path_present(root: &serde_json::Map<String, serde_json::Value>, path: &[&str]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut cursor = root;
    for key in parents {
        let Some(next) = cursor.get(*key).and_then(serde_json::Value::as_object) else {
            return false;
        };
        cursor = next;
    }
    cursor.contains_key(*last)
}

/// Asserts `path` on `root`, creating every object on the way.
///
/// A path of one key leaves an **empty object** rather than a boolean, because
/// a parent capability's presence is the claim and the schema gives it an
/// object: `ClientCapabilities.roots` is an object, so `"roots": {}` is the
/// wire form and `"roots": true` is not one any schema admits. A longer path
/// ends in `true`, because a sub-capability like `listChanged` genuinely is a
/// boolean.
fn insert_path(root: &mut serde_json::Map<String, serde_json::Value>, path: &[&str]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cursor = root;
    for key in parents {
        let entry = cursor
            .entry((*key).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let serde_json::Value::Object(parent) = entry else {
            return;
        };
        cursor = parent;
    }
    let leaf = if parents.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::Value::Bool(true)
    };
    cursor.insert((*last).to_string(), leaf);
}

/// Renders a profile's server capabilities as a `ServerCapabilities` object.
///
/// Reads [`FeatureProfile::advertised_by_server`] rather than being handed a
/// set, so there is no way to reach this function with a capability set that
/// did not come from the profile. That is the drift this exists to prevent: a
/// caller that passed in its own set would be free to advertise a flag the
/// gates behind it would refuse, and the peer would have been told otherwise
/// at connect time.
///
/// A bridge passes the intersection here, which is the whole point of computing
/// one: the object a client reads is the set of things the bridge can actually
/// do behind it.
///
/// A capability in the derived set with no row in [`SERVER_CAPABILITY_WIRE`]
/// is not emitted. Two are in that position and neither is reachable from a
/// server: the client-side ones, which [`advertised_by_server`] cannot produce
/// because their feature rows are `Side::Client`, and
/// [`Capability::Extensions`], which only `2026-07-28` defines — a revision a
/// caller has already refused before getting here, because that revision has
/// no `InitializeResult` to put it in.
#[must_use]
pub fn server_capabilities_json(profile: FeatureProfile) -> serde_json::Value {
    let advertised = profile.advertised_by_server();
    let mut root = serde_json::Map::new();

    for (capability, path) in SERVER_CAPABILITY_WIRE {
        if !advertised.contains(*capability) {
            continue;
        }
        insert_path(&mut root, path);
    }

    serde_json::Value::Object(root)
}

#[cfg(test)]
mod tests {
    use super::{InitializeError, read_offer, server_capabilities_json};
    use crate::domain::ir::JsonObject;
    use crate::domain::{Capability, CapabilitySet, ProtocolVersion};

    /// What a test here can fail with.
    #[derive(Debug)]
    enum Failure {
        /// The fixture was not a JSON object.
        Fixture(crate::domain::ir::JsonObjectError),
        /// The test's own complaint.
        Message(String),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Fixture(error) => write!(f, "bad fixture: {error}"),
                Self::Message(text) => f.write_str(text),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<crate::domain::ir::JsonObjectError> for Failure {
        fn from(error: crate::domain::ir::JsonObjectError) -> Self {
            Self::Fixture(error)
        }
    }

    impl From<String> for Failure {
        fn from(text: String) -> Self {
            Self::Message(text)
        }
    }

    fn params(text: &str) -> Result<JsonObject, Failure> {
        JsonObject::new(text).map_err(Failure::from)
    }

    #[test]
    fn reads_a_minimal_offer() -> Result<(), Failure> {
        let body = params(
            r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe"}}"#,
        )?;
        let offer = read_offer(&body)
            .map_err(|_| Failure::from("a minimal offer was refused".to_string()))?;

        assert_eq!(offer.version, ProtocolVersion::V2025_06_18);
        assert_eq!(offer.capabilities, CapabilitySet::empty());
        assert_eq!(offer.name, "probe");
        Ok(())
    }

    /// An unknown revision is an offer, not a refusal. The client is the only
    /// thing that can speak it, so refusing here would make every future
    /// client unconnectable — the answer is a counter-offer.
    #[test]
    fn an_unknown_revision_is_still_read() -> Result<(), Failure> {
        let body = params(
            r#"{"protocolVersion":"2099-01-01","capabilities":{},"clientInfo":{"name":"future"}}"#,
        )?;
        let offer = read_offer(&body)
            .map_err(|_| Failure::from("a future revision was refused".to_string()))?;
        assert!(matches!(offer.version, ProtocolVersion::Unknown(_)));
        Ok(())
    }

    #[test]
    fn capabilities_are_read_through_the_tables() -> Result<(), Failure> {
        let body = params(
            r#"{"protocolVersion":"2025-06-18","capabilities":{"roots":{}},"clientInfo":{"name":"r"}}"#,
        )?;
        let offer =
            read_offer(&body).map_err(|_| Failure::from("roots were refused".to_string()))?;
        assert!(
            offer.capabilities.contains(Capability::Roots),
            "an empty roots object is the wire form for the capability being present"
        );
        Ok(())
    }

    /// Every malformed case collapses to one error, so the table checks that
    /// none of them is accidentally accepted rather than that each has its own
    /// variant.
    #[test]
    fn malformed_bodies_are_all_refused() -> Result<(), Failure> {
        let cases = [
            r#"{"capabilities":{},"clientInfo":{"name":"x"}}"#,
            r#"{"protocolVersion":7,"capabilities":{},"clientInfo":{"name":"x"}}"#,
            r#"{"protocolVersion":"not-a-date","capabilities":{},"clientInfo":{"name":"x"}}"#,
            r#"{"protocolVersion":"2025-06-18","clientInfo":{"name":"x"}}"#,
            r#"{"protocolVersion":"2025-06-18","capabilities":true,"clientInfo":{"name":"x"}}"#,
            r#"{"protocolVersion":"2025-06-18","capabilities":{}}"#,
            r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{}}"#,
            r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":3}}"#,
        ];

        // A body that is not a JSON object cannot appear in this table: the
        // reader takes a `&JsonObject`, so `params: "[]"` was already refused
        // by whoever parsed it. Testing it here would assert that a type
        // holds, which is what the signature is for.

        for (index, case) in cases.iter().enumerate() {
            let body = params(case)?;
            if read_offer(&body) != Err(InitializeError::Malformed) {
                return Err(Failure::from(format!("case {index} was accepted: {case}")));
            }
        }
        Ok(())
    }

    /// `clientInfo.version` is free-form and nothing here depends on it, so a
    /// client that omits it connects. This is a deliberate leniency and would
    /// silently tighten if someone added the field to the required set.
    #[test]
    fn a_client_without_a_version_string_is_accepted() -> Result<(), Failure> {
        let body = params(
            r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"x"}}"#,
        )?;
        read_offer(&body)
            .map_err(|_| Failure::from("a nameless version was refused".to_string()))?;
        Ok(())
    }

    /// The writer emits only what the profile derived.
    ///
    /// There is no round-trip here to test: the reader above reads a
    /// _client_ capabilities object and the writer renders a _server_ one, so
    /// they walk different tables in opposite directions and are not
    /// inverses. What matters is that the writer cannot drift from the gates —
    /// a capability the profile refused must not appear, or the peer was told
    /// otherwise at connect time.
    #[test]
    fn the_writer_advertises_only_what_the_profile_allows() -> Result<(), Failure> {
        let version = ProtocolVersion::V2025_06_18;
        let everything = crate::domain::feature::resolve(crate::domain::Negotiation {
            version,
            client_caps: CapabilitySet::all_for(version),
            server_caps: CapabilitySet::all_for(version),
            transport: crate::domain::TransportKind::Stdio,
        });
        let nothing = crate::domain::feature::resolve(crate::domain::Negotiation {
            version,
            client_caps: CapabilitySet::all_for(version),
            server_caps: CapabilitySet::empty(),
            transport: crate::domain::TransportKind::Stdio,
        });

        let serde_json::Value::Object(generous) = server_capabilities_json(everything) else {
            return Err(Failure::from(
                "tools should render as an object".to_string(),
            ));
        };
        let serde_json::Value::Object(stingy) = server_capabilities_json(nothing) else {
            return Err(Failure::from(
                "tools should render as an object".to_string(),
            ));
        };

        assert!(
            stingy.is_empty(),
            "a server that claimed nothing may claim nothing"
        );
        assert!(
            !generous.is_empty(),
            "a server claiming everything must claim something"
        );
        Ok(())
    }

    /// A single-key capability path renders as an empty object, not `true`.
    ///
    /// `experimental` is the case: the schema gives it an object, so
    /// `"experimental": true` is not a wire form any schema admits, and a
    /// client reading it with the same table this crate uses would not see the
    /// capability.
    #[test]
    fn a_parent_capability_renders_as_an_object() -> Result<(), Failure> {
        let version = ProtocolVersion::V2025_06_18;
        let profile = crate::domain::feature::resolve(crate::domain::Negotiation {
            version,
            client_caps: CapabilitySet::all_for(version),
            server_caps: CapabilitySet::all_for(version),
            transport: crate::domain::TransportKind::Stdio,
        });
        let serde_json::Value::Object(object) = server_capabilities_json(profile) else {
            return Err(Failure::from(
                "server_capabilities_json always renders an object".to_string(),
            ));
        };

        match object.get("experimental") {
            Some(experimental) => assert!(
                experimental.is_object(),
                "experimental must be an object; `true` is not a form the schema admits"
            ),
            None => Err(Failure::from(
                "experimental was in the profile but not in the wire form".to_string(),
            ))?,
        }
        Ok(())
    }
}
