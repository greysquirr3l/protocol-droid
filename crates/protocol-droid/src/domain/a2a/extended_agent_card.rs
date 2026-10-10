//! A2A `GetExtendedAgentCard` typed envelopes.
//!
//! # What these are
//!
//! [`GetExtendedAgentCardRequest`] and
//! [`GetExtendedAgentCardResponse`] are the
//! typed shapes for the `GetExtendedAgentCard`
//! JSON-RPC method. The method returns the
//! authenticated agent's extended card — the
//! version of the card that is only available
//! after the client has authenticated with the
//! agent.
//!
//! The A2A v1.0 spec splits the agent card into
//! two reads: the *public* card (no
//! authentication) served on the well-known
//! path, and the *extended* card (requires
//! authentication) returned by
//! `GetExtendedAgentCard`. The public card is
//! what a peer sees when it discovers the
//! agent; the extended card is the same card
//! plus the additional capabilities the agent
//! does not advertise publicly (a paid tool, an
//! admin endpoint, etc.).
//!
//! # Why the request is an empty struct
//!
//! The spec's `GetExtendedAgentCardRequest`
//! has only a `tenant` field, which is a
//! path parameter — not a wire field. A
//! JSON-RPC request with no body has `params:
//! null` or `params: {}` on the wire, and
//! the Rust type that matches that is
//! `GetExtendedAgentCardRequest` (an empty
//! struct that serialises to `{}` by default).
//!
//! # Why the response is a transparent newtype
//! around `AgentCard`
//!
//! The wire form is the `AgentCard` object
//! directly — no wrapper. A newtype with
//! `transparent` is the cheapest shape that
//! keeps the type distinct from a raw
//! `AgentCard` (and lets the consumer
//! pattern-match on the response type if they
//! want to).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::agent_card::AgentCard;

/// A typed `GetExtendedAgentCard` request.
/// The wire form is `{}` (the spec's request
/// has no body fields).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetExtendedAgentCardRequest {
    /// A per-request metadata slot. The
    /// spec does not name a metadata field
    /// on the request, but a server that
    /// wants to accept arbitrary
    /// client-side context (a trace id,
    /// a tenant override) can use this
    /// map. Empty by default, absent on
    /// the wire.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl GetExtendedAgentCardRequest {
    /// Starts a new request.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            metadata: BTreeMap::new(),
        }
    }
}

/// A typed `GetExtendedAgentCard` response.
/// The wire form is the [`AgentCard`] object
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GetExtendedAgentCardResponse(pub AgentCard);

impl From<AgentCard> for GetExtendedAgentCardResponse {
    fn from(card: AgentCard) -> Self {
        Self(card)
    }
}

impl std::ops::Deref for GetExtendedAgentCardResponse {
    type Target = AgentCard;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{GetExtendedAgentCardRequest, GetExtendedAgentCardResponse};
    use crate::domain::a2a::agent_card::AgentCardBuilder;

    type TestError = Box<dyn std::error::Error>;

    #[test]
    fn request_serialises_as_empty_object() -> Result<(), TestError> {
        let request = GetExtendedAgentCardRequest::new();
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a GetExtendedAgentCardRequest must serialise to a JSON object")?;
        assert!(obj.is_empty(), "expected `{{}}`, got {value}");
        Ok(())
    }

    #[test]
    fn response_serialises_as_card_object() -> Result<(), TestError> {
        let interface = crate::domain::a2a::agent_card::AgentInterface {
            url: url::Url::parse("https://example.com/a2a/v1")
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?,
            protocol_binding: crate::domain::a2a::agent_card::JSONRPC.clone(),
            protocol_version: crate::domain::a2a::A2aVersion::V1_0,
            tenant: None,
        };
        let card = AgentCardBuilder::new()
            .with_name("test-agent")
            .with_description("An A2A test agent.")
            .with_version("0.1.0")
            .with_interface(interface)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response = GetExtendedAgentCardResponse::from(card);
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a GetExtendedAgentCardResponse must serialise to a JSON object")?;
        // The wire form is the inner card
        // — no wrapper. The card's `name`
        // is the discriminator.
        assert_eq!(
            obj.get("name").and_then(serde_json::Value::as_str),
            Some("test-agent")
        );
        Ok(())
    }

    #[test]
    fn response_from_card_via_into_works() -> Result<(), TestError> {
        let interface = crate::domain::a2a::agent_card::AgentInterface {
            url: url::Url::parse("https://example.com/a2a/v1")
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?,
            protocol_binding: crate::domain::a2a::agent_card::JSONRPC.clone(),
            protocol_version: crate::domain::a2a::A2aVersion::V1_0,
            tenant: None,
        };
        let card = AgentCardBuilder::new()
            .with_name("test-agent")
            .with_description("An A2A test agent.")
            .with_version("0.1.0")
            .with_interface(interface)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response: GetExtendedAgentCardResponse = card.clone().into();
        assert_eq!(response.0, card);
        Ok(())
    }
}
