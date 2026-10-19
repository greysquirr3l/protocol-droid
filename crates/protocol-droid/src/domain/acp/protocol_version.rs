//! `AcpProtocolVersion` — the wire-level protocol version.
//!
//! # Why a separate type from [`AcpVersion`](crate::domain::acp::version::AcpVersion)
//!
//! The ACP `meta.json` file and the schema's `MANIFEST.toml`
//! entry pin a *release* (`schema-v1.25.0`). The wire's
//! `protocolVersion` field on `InitializeRequest` and
//! `InitializeResponse` is a `u16` that increments on
//! breaking changes (per the schema's own docstring on
//! `ProtocolVersion`:
//! <https://github.com/agentclientprotocol/agent-client-protocol/blob/schema-v1.25.0/schema/v1/schema.json#LProtocolVersion>).
//!
//! The two coexist: a build pinned to `AcpVersion::V1_25_0`
//! *always* sends `AcpProtocolVersion(1)` on the wire. A
//! future build pinned to a v2 release sends `2`. A peer
//! that does not understand the offered version answers
//! with the same `u16` value the build knows, and the
//! negotiation result is "the lower of the two".
//!
//! # Why `Copy` and a `u16` newtype
//!
//! The wire form is a `u16`; the dispatcher's API boundary
//! hands out the typed value by value, not by reference.
//! A bare `u16` would type-collide with every other
//! `u16` in the codebase; the newtype is the boundary
//! marker the type checker reads.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The wire-level protocol version. The current pin is
/// `V1`, which is what every `schema-v1.x.y` release
/// advertises. A future breaking change moves the pin to
/// `V2` and the build knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AcpProtocolVersion(
    /// The wire-level integer. `0` is reserved (the
    /// schema's `minimum` is 0, but no published release
    /// has ever used 0; the `AcpProtocolVersion::CURRENT`
    /// constant is the only well-known value).
    pub u16,
);

impl AcpProtocolVersion {
    /// The current protocol version. `1` is the wire value
    /// every `schema-v1.x.y` release advertises. The
    /// release tag in [`AcpVersion`](crate::domain::acp::version::AcpVersion) is a different
    /// number (the schema's release tag is a `Major.Minor.Patch`
    /// tuple, the wire value is the *protocol* major).
    pub const CURRENT: Self = Self(1);
}

impl Default for AcpProtocolVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

impl fmt::Display for AcpProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The error returned by [`AcpProtocolVersion::from_str`]
/// when the input is not a well-formed decimal integer
/// in `0..=u16::MAX`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpProtocolVersionParseError {
    input: String,
    reason: &'static str,
}

impl fmt::Display for AcpProtocolVersionParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ACP protocol version {:?} is invalid: {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for AcpProtocolVersionParseError {}

impl FromStr for AcpProtocolVersion {
    type Err = AcpProtocolVersionParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let value = input.parse::<u16>().map_err(|error| {
            let reason = match error.kind() {
                core::num::IntErrorKind::Empty => "the input is empty",
                core::num::IntErrorKind::InvalidDigit => "the input is not a decimal integer",
                core::num::IntErrorKind::PosOverflow => "the value overflows u16",
                _ => "the input is not a valid u16",
            };
            AcpProtocolVersionParseError {
                input: input.to_owned(),
                reason,
            }
        })?;
        Ok(Self(value))
    }
}

impl Serialize for AcpProtocolVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AcpProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        u16::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test error type. `serde_json` is the
    /// only fallible call; everything else is
    /// infallible for a well-formed value.
    #[derive(Debug)]
    #[expect(
        dead_code,
        reason = "the test only constructs the error variant on failure; the field is read by the Debug derive"
    )]
    enum Failure {
        Parse(AcpProtocolVersionParseError),
        Json(serde_json::Error),
    }

    impl From<AcpProtocolVersionParseError> for Failure {
        fn from(error: AcpProtocolVersionParseError) -> Self {
            Self::Parse(error)
        }
    }

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::Json(error)
        }
    }

    /// The current pin round-trips through
    /// `Display` and `FromStr`, and serialises
    /// as a bare `u16` (no struct wrapper, no
    /// string) on the wire.
    #[test]
    fn the_current_pin_round_trips_and_serialises_as_u16() -> Result<(), Failure> {
        let value: AcpProtocolVersion = "1".parse()?;
        assert_eq!(value, AcpProtocolVersion::CURRENT);
        let round_tripped: AcpProtocolVersion = format!("{value}").parse()?;
        assert_eq!(round_tripped, value);
        let json = serde_json::to_string(&value)?;
        assert_eq!(json, "1", "the wire form is a bare u16");
        Ok(())
    }

    /// An empty input and an out-of-range input
    /// are both errors with a reason. The error
    /// type carries the input and the reason so
    /// a log line can be actionable.
    #[test]
    fn parse_failures_carry_the_input_and_the_reason() -> Result<(), Failure> {
        match "".parse::<AcpProtocolVersion>() {
            Err(error) => assert_eq!(error.reason, "the input is empty"),
            Ok(_) => {
                return Err(Failure::Parse(AcpProtocolVersionParseError {
                    input: "<empty>".to_owned(),
                    reason: "an empty input should fail to parse",
                }));
            }
        }
        match "999999".parse::<AcpProtocolVersion>() {
            Err(error) => assert_eq!(error.reason, "the value overflows u16"),
            Ok(_) => {
                return Err(Failure::Parse(AcpProtocolVersionParseError {
                    input: "999999".to_owned(),
                    reason: "an out-of-range input should fail to parse",
                }));
            }
        }
        match "1.5".parse::<AcpProtocolVersion>() {
            Err(error) => assert_eq!(error.reason, "the input is not a decimal integer"),
            Ok(_) => {
                return Err(Failure::Parse(AcpProtocolVersionParseError {
                    input: "1.5".to_owned(),
                    reason: "a fractional input should fail to parse",
                }));
            }
        }
        Ok(())
    }
}
