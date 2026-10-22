//! The bridge's reply path, against the real codecs.
//!
//! These are the tests that decide whether content translation is honest. The
//! unit tests in `application/bridge/content.rs` prove the *decision* — when to
//! rebuild, when to forward — against a codec that counts. What only a real
//! codec can show is the consequence of that decision: whether a
//! `2026-07-28` server's `structuredContent` actually reaches a `2025-03-26`
//! client as text, and whether a content block added after this build survives
//! the trip untouched.
//!
//! # Why this file is in `tests/`
//!
//! Because it points application types at an adapter. That is the one direction
//! the layering forbids inside the crate, and this is the one place a test may
//! legitimately do it — the boundary itself is the subject.

use std::sync::{Arc, Mutex};

use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::application::bridge::{Bridge, BridgeError, BridgePeer, Forwarded, SendFuture};
use protocol_droid::domain::ir::{LowerError, ToolResult};
use protocol_droid::domain::{
    Agreement, CapabilitySet, DegradePolicy, Feature, Leg, Negotiation, ProtocolVersion,
    TransportKind, resolve,
};
use serde_json::{Map, Value, json};

type TestError = Box<dyn std::error::Error>;

/// An upstream that answers `tools/call` with a canned result.
///
/// Cloneable so a test can hold a handle to what the bridge sent, which is the
/// only way to assert on the outbound side of a forwarding test.
#[derive(Clone)]
struct Upstream {
    reply: Map<String, Value>,
    sent: Arc<Mutex<Vec<Map<String, Value>>>>,
}

impl Upstream {
    fn answering(reply: &str) -> Result<Self, TestError> {
        Ok(Self {
            reply: serde_json::from_str(reply)?,
            sent: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn sent(&self) -> Result<Vec<Map<String, Value>>, TestError> {
        self.sent
            .lock()
            .map(|sent| sent.clone())
            .map_err(|_| -> TestError { "the recorder was poisoned".into() })
    }
}

impl BridgePeer for Upstream {
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .map_err(|_| BridgeError::Close("the recorder was poisoned".to_owned()))?
                .push(envelope);
            Ok(self.reply.clone())
        })
    }

    fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            self.send(envelope).await?;
            Ok(())
        })
    }

    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async { Ok(()) })
    }
}

/// A leg that can express everything its own revision defines.
fn full(version: ProtocolVersion) -> Leg {
    let caps = CapabilitySet::all_for(version);
    Leg::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: caps,
            server_caps: caps,
            transport: TransportKind::Stdio,
        }),
    )
}

/// A downgrade is provoked by naming the *older revision*, never by
/// subtracting capabilities from a newer one: the point of the revision is that
/// its schema has no field for the newer things, and a profile hand-edited to
/// omit one would be testing the gate rather than the wire. Every leg in this
/// file is therefore built by `full`.
fn call(tool: &str) -> Result<Map<String, Value>, TestError> {
    Ok(serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool }
    }))?)
}

fn bridge_over(
    client: Leg,
    server: Leg,
    policy: DegradePolicy,
    reply: &str,
) -> Result<(Bridge<Upstream, WireToolResults>, Upstream), TestError> {
    let upstream = Upstream::answering(reply)?;
    let bridge = Bridge::new(
        Agreement::between(client, server),
        policy,
        upstream.clone(),
        WireToolResults,
    );
    Ok((bridge, upstream))
}

/// The named scenario: a server returns `structuredContent`, and the client
/// negotiated `2025-03-26`, which has no field for it.
///
/// The specification's recommended fallback is to serialise the object into a
/// text content block and record the downgrade. Both halves are asserted on the
/// wire, because an IR-only assertion would pass even if the reply the client
/// receives still carried a member its revision cannot parse.
///
/// The upstream is `2025-06-18` — the revision that introduced the field — and
/// not `2026-07-28`, which *requires* `resultType` as well. That would have
/// put a second downgrade on the path and proved the two at once, which is worth
/// having and is not what this test is for.
#[tokio::test]
async fn a_structured_result_reaches_an_older_client_as_text() -> Result<(), TestError> {
    let client = full(ProtocolVersion::V2025_03_26);
    let (bridge, _) = bridge_over(
        client,
        full(ProtocolVersion::V2025_06_18),
        DegradePolicy::permissive(),
        r#"{"jsonrpc":"2.0","id":1,"result":{
             "content":[{"type":"text","text":"answer"}],
             "structuredContent":{"a":1}}}"#,
    )?;

    let forwarded = bridge.forward(&call("echo")?).await?;
    let result = forwarded
        .reply
        .get("result")
        .ok_or("the reply carried no result")?;

    assert!(
        result.get("structuredContent").is_none(),
        "the client negotiated a revision with no field for it: {result}"
    );
    let blocks = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or("the lowered result had no content array")?;
    let serialised = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    assert!(
        serialised.contains("\"a\":1"),
        "the structured content must survive as text: {serialised}"
    );
    assert!(
        !forwarded.report.is_empty(),
        "a downgrade that is not recorded is a silent loss"
    );
    Ok(())
}

/// The fail-secure half, on the wire the client actually reads.
///
/// The default policy refuses rather than hand over a result the client cannot
/// parse, and the refusal names the feature and the revision so an operator can
/// tell a missing capability from a bug.
#[tokio::test]
async fn a_strict_policy_refuses_a_result_the_client_cannot_read() -> Result<(), TestError> {
    let client = full(ProtocolVersion::V2025_03_26);
    let (bridge, _) = bridge_over(
        client,
        full(ProtocolVersion::V2025_06_18),
        DegradePolicy::strict(),
        r#"{"jsonrpc":"2.0","id":1,"result":{
             "content":[{"type":"text","text":"answer"}],
             "structuredContent":{"a":1}}}"#,
    )?;

    let outcome = bridge.forward(&call("echo")?).await;

    assert!(
        matches!(outcome, Err(BridgeError::NotReadable(_))),
        "expected a refusal, got {outcome:?}"
    );
    let Err(BridgeError::NotReadable(cause)) = outcome else {
        return Err("expected a refusal, got something else".into());
    };
    assert!(
        matches!(
            cause,
            protocol_droid::domain::ir::LowerError::FeatureUnavailable {
                feature: Feature::ToolStructuredOutput,
                ..
            }
        ),
        "the refusal must name the feature: {cause}"
    );
    Ok(())
}

/// The claim the whole design rests on: a result the client can read comes back
/// exactly as the server wrote it.
///
/// The reply here carries an unmodelled member *and* a content block whose
/// ordering would be lost by a re-encode. Byte equality is the whole assertion —
/// a test that compared only the modelled fields would pass against an
/// implementation that quietly rebuilt the reply.
#[tokio::test]
async fn a_reply_the_client_can_read_is_handed_back_byte_identical() -> Result<(), TestError> {
    let reply = r#"{"jsonrpc":"2.0","id":1,"result":{
        "content":[{"type":"text","text":"answer","annotations":{"audience":["assistant"]}}],
        "isError":false,
        "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"probe","version":"1"}},
        "x-vendor-directive":{"kept":[1,2,3]}}}"#;
    let (bridge, _) = bridge_over(
        full(ProtocolVersion::V2026_07_28),
        full(ProtocolVersion::V2026_07_28),
        DegradePolicy::strict(),
        reply,
    )?;

    let sent: Map<String, Value> = serde_json::from_str(reply)?;
    let forwarded = bridge.forward(&call("echo")?).await?;

    assert_eq!(
        forwarded.reply, sent,
        "a reply with nothing to translate must not be rebuilt"
    );
    assert!(
        forwarded.report.is_empty(),
        "nothing was decided, so nothing should be recorded"
    );
    Ok(())
}

/// A content block added after this build was compiled must still reach the
/// client.
///
/// The generated union refuses an unrecognised `type` outright, deliberately, so
/// this is the case that separates "rebuild only what needs rebuilding" from
/// "rebuild everything". An implementation that always re-encoded would fail
/// here — and the failure would look like a bridge bug rather than an extension
/// the client might well understand.
#[tokio::test]
async fn a_result_this_build_cannot_read_is_forwarded_untouched() -> Result<(), TestError> {
    let reply = r#"{"jsonrpc":"2.0","id":1,"result":{
        "content":[{"type":"text","text":"answer"},
                   {"type":"annotation","audience":["assistant"],"priority":0.5}]}}"#;
    let (bridge, _) = bridge_over(
        full(ProtocolVersion::V2025_03_26),
        full(ProtocolVersion::V2026_07_28),
        DegradePolicy::permissive(),
        reply,
    )?;

    let sent: Map<String, Value> = serde_json::from_str(reply)?;
    let forwarded = bridge.forward(&call("echo")?).await?;

    assert_eq!(
        forwarded.reply, sent,
        "a block this build cannot model must be forwarded, not dropped"
    );
    Ok(())
}

/// Only a `tools/call` reply carries a `ToolResult`. Everything else the client
/// might ask for has an array or a scalar for a result, and asking the codec
/// about one is how a `tools/list` reply gets rebuilt into something the server
/// never sent.
#[tokio::test]
async fn a_reply_that_is_not_a_tool_result_is_never_rebuilt() -> Result<(), TestError> {
    let reply = r#"{"jsonrpc":"2.0","id":1,"result":[{"name":"echo"}]}"#;
    let (bridge, _) = bridge_over(
        full(ProtocolVersion::V2025_03_26),
        full(ProtocolVersion::V2026_07_28),
        DegradePolicy::permissive(),
        reply,
    )?;

    let sent: Map<String, Value> = serde_json::from_str(reply)?;
    let listed: Map<String, Value> = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    }))?;
    let forwarded = bridge.forward(&listed).await?;

    assert_eq!(forwarded.reply, sent);
    assert!(forwarded.report.is_empty());
    Ok(())
}

/// An error envelope is the upstream's own words about a request, and carries
/// no result to translate.
#[tokio::test]
async fn an_error_reply_is_handed_back_untouched() -> Result<(), TestError> {
    let reply = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no such tool"}}"#;
    let (bridge, _) = bridge_over(
        full(ProtocolVersion::V2025_03_26),
        full(ProtocolVersion::V2026_07_28),
        DegradePolicy::strict(),
        reply,
    )?;

    let sent: Map<String, Value> = serde_json::from_str(reply)?;
    let forwarded = bridge.forward(&call("absent")?).await?;

    assert_eq!(forwarded.reply, sent);
    Ok(())
}

/// A notification gets no answer, so there is nothing to translate and nothing
/// to record. The call is what proves the translation is not merely harmless
/// here — an implementation that translated unconditionally would be reaching
/// for a reply that does not exist.
#[tokio::test]
async fn the_notification_path_never_translates() -> Result<(), TestError> {
    let (bridge, upstream) = bridge_over(
        full(ProtocolVersion::V2025_03_26),
        full(ProtocolVersion::V2026_07_28),
        DegradePolicy::permissive(),
        r#"{"jsonrpc":"2.0","id":1,"result":{"content":[],"structuredContent":{"a":1}}}"#,
    )?;

    let announced: Map<String, Value> = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }))?;
    let forwarded: Forwarded = bridge.forward(&announced).await?;

    assert!(
        forwarded.reply.is_empty(),
        "a notification has no reply to carry"
    );
    assert!(forwarded.report.is_empty());
    assert_eq!(
        upstream.sent()?.len(),
        1,
        "the notification was still forwarded"
    );
    Ok(())
}

/// The revision boundary in the other direction: a `2025-03-26` server's result
/// reaching a `2026-07-28` client needs no translation at all, because a text
/// block means the same thing in both. This is the common case, and it is the
/// one that must cost nothing.
#[tokio::test]
async fn a_plain_result_crosses_the_boundary_untouched() -> Result<(), TestError> {
    let reply = r#"{"jsonrpc":"2.0","id":1,"result":{
        "content":[{"type":"text","text":"answer"}],"isError":false}}"#;
    let (bridge, _) = bridge_over(
        full(ProtocolVersion::V2026_07_28),
        full(ProtocolVersion::V2025_03_26),
        DegradePolicy::strict(),
        reply,
    )?;

    let sent: Map<String, Value> = serde_json::from_str(reply)?;
    let forwarded = bridge.forward(&call("echo")?).await?;

    assert_eq!(forwarded.reply, sent);
    assert!(forwarded.report.is_empty());
    Ok(())
}

/// `resultType` has no defined fallback, so no policy can authorise dropping
/// it — and the bridge refuses to deliver a result the client cannot read
/// rather than quietly losing a field the client may well have been selecting
/// on.
///
/// This is the counterpart to `structuredContent`, which *does* have a defined
/// fallback and *is* downgraded under a permissive policy. The pair is the
/// point: what separates them is not the bridge's discretion, it is whether the
/// specification says what to give up.
#[tokio::test]
async fn a_result_type_has_no_fallback_and_is_refused_not_dropped() -> Result<(), TestError> {
    let reply = r#"{"jsonrpc":"2.0","id":1,"result":{
        "content":[{"type":"text","text":"answer"}],"resultType":"search"}}"#;
    let (bridge, _) = bridge_over(
        full(ProtocolVersion::V2025_03_26),
        full(ProtocolVersion::V2026_07_28),
        DegradePolicy::permissive(),
        reply,
    )?;

    let outcome = bridge.forward(&call("search")?).await;
    let Err(BridgeError::NotReadable(cause)) = outcome else {
        return Err("a result with no defined fallback must not be delivered".into());
    };

    assert!(
        matches!(
            cause,
            LowerError::NoRuleForFeature {
                feature: Feature::ToolResultType
            }
        ),
        "the refusal must name the feature it has no rule for: {cause}"
    );
    Ok(())
}

/// The IR type this whole path exists for, pinned by name so the test file
/// cannot drift away from the scenario it documents.
#[test]
fn the_scenario_is_about_a_tool_result() -> Result<(), TestError> {
    let result = ToolResult::text("answer").with_structured(r#"{"a":1}"#)?;
    assert_eq!(result.content.len(), 1);
    Ok(())
}
