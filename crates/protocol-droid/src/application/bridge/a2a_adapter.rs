//! The A2A half of a cross-protocol bridge.
//!
//! # What this adapter knows
//!
//! A2A's call is `SendMessage` with a `message` whose `parts` carry the
//! payload and a `metadata` entry naming the skill. A2A's reply is
//! either a `Task` or a `Message`. That is the whole of it — the adapter
//! does not know MCP exists, and the composition in [`super::composed`]
//! is what puts the two halves together.
//!
//! # Where the arguments live
//!
//! A2A carries structured data in a message *part*, not in a field beside
//! the message. The adapter reads the first data part as the call's
//! arguments and writes them back as a single data part. A part with no
//! MCP counterpart — a text part, a second data part — is recorded as a
//! loss in the report rather than dropped in silence.
//!
//! # The skill name
//!
//! A2A has no field for "the tool I am calling"; the bridge puts the
//! target in the message's `metadata` under `bridge_skill`. That is the
//! one piece of vocabulary the two protocols do not share, and it is what
//! the `target` on an [`IrCall`] carries.

use serde_json::{Map, Value};

use crate::domain::a2a::message::{Message, Part};
use crate::domain::ir::{
    Content, Extensions, IrCall, IrMethod, IrOutcome, JsonObject, TextContent,
};

use super::cross_protocol::{
    CrossProtocolError, ProtocolSide, RequestCodecReport, RequestCodecStep,
};
use super::protocol_adapter::{ProtocolAdapter, WireRequest};

/// The A2A method this adapter bridges.
const SEND_MESSAGE: &str = "SendMessage";

/// The metadata key the bridge puts the call's target in.
const BRIDGE_SKILL_KEY: &str = "bridge_skill";

/// The A2A half of a bridge.
///
/// Cheap to construct and to share: it holds no state, so the `Send +
/// Sync` bound the trait requires is satisfied by the type itself.
#[derive(Debug, Default, Clone, Copy)]
pub struct A2aAdapter;

impl A2aAdapter {
    /// The adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ProtocolAdapter for A2aAdapter {
    /// A2A v1.0.
    fn side(&self) -> ProtocolSide {
        ProtocolSide::A2a
    }

    /// `SendMessage` is the one A2A method the bridge translates.
    fn ir_method(&self, wire_method: &str) -> Option<IrMethod> {
        (wire_method == SEND_MESSAGE).then_some(IrMethod::Call)
    }

    /// The wire method for an IR call.
    fn wire_method(&self, ir_method: &IrMethod) -> Option<&'static str> {
        match ir_method {
            IrMethod::Call => Some(SEND_MESSAGE),
        }
    }

    /// The shape of an A2A call, for the translation report.
    fn request_shape(&self) -> &'static str {
        "A2A SendMessage{message, metadata}"
    }

    /// The shape of an A2A reply, for the translation report.
    fn reply_shape(&self) -> &'static str {
        "A2A Task/Message"
    }

    /// Lifts `SendMessage` parameters into the IR.
    ///
    /// The `metadata.bridge_skill` entry becomes the call's target, the
    /// first data part becomes its arguments, and the rest of the
    /// `metadata` rides along in the extensions bag.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MethodNotBridged`] for any method but
    /// `SendMessage`, and [`CrossProtocolError::MalformedPayload`] when
    /// the parameters carry no message or the data part is not an object.
    fn lift_request(
        &self,
        wire_method: &str,
        params: &Value,
        report: &mut RequestCodecReport,
    ) -> Result<IrCall, CrossProtocolError> {
        if wire_method != SEND_MESSAGE {
            return Err(CrossProtocolError::MethodNotBridged(
                wire_method.to_owned(),
                ProtocolSide::A2a,
            ));
        }
        let Some(object) = params.as_object() else {
            return Err(CrossProtocolError::MalformedPayload {
                side: ProtocolSide::A2a,
                detail: "params is not an object".to_owned(),
            });
        };
        let Some(message) = object.get("message").and_then(Value::as_object) else {
            return Err(CrossProtocolError::MalformedPayload {
                side: ProtocolSide::A2a,
                detail: "params carries no message".to_owned(),
            });
        };
        let metadata = object.get("metadata").and_then(Value::as_object);
        let target = metadata
            .and_then(|meta| meta.get(BRIDGE_SKILL_KEY))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        let mut extensions = Map::new();
        if let Some(meta) = metadata {
            for (key, value) in meta {
                if key != BRIDGE_SKILL_KEY {
                    extensions.insert(key.clone(), value.clone());
                }
            }
        }

        let mut arguments = None;
        let mut lost = 0_usize;
        if let Some(parts) = message.get("parts").and_then(Value::as_array) {
            for part in parts {
                match part.get("data") {
                    Some(data) if arguments.is_none() => {
                        arguments =
                            Some(JsonObject::from_value(data.clone()).map_err(|error| {
                                CrossProtocolError::MalformedPayload {
                                    side: ProtocolSide::A2a,
                                    detail: format!("the data part is not a JSON object: {error}"),
                                }
                            })?);
                    }
                    _ => lost += 1,
                }
            }
        }
        if lost > 0 {
            report.record(RequestCodecStep::Dropped {
                what: "A2A message parts with no counterpart on the other side",
                count: lost,
            });
        }

        Ok(IrCall::new(IrMethod::Call, target, arguments)
            .with_extensions(Extensions::from_json_object(extensions)))
    }

    /// Lowers an IR call into `SendMessage` parameters.
    ///
    /// The arguments become a single data part; the target becomes the
    /// `bridge_skill` metadata entry.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MalformedPayload`] when the arguments cannot
    /// be put back on the wire, which the IR's validation makes
    /// impossible.
    fn lower_request(
        &self,
        call: &IrCall,
        _report: &mut RequestCodecReport,
    ) -> Result<WireRequest, CrossProtocolError> {
        let mut parts = Vec::new();
        if let Some(arguments) = call.arguments.as_ref() {
            let value = serde_json::from_str(arguments.as_str()).map_err(|error| {
                CrossProtocolError::MalformedPayload {
                    side: ProtocolSide::A2a,
                    detail: format!("the arguments are not valid JSON: {error}"),
                }
            })?;
            let mut part = Map::new();
            part.insert("data".to_owned(), value);
            parts.push(Value::Object(part));
        }
        let mut message = Map::new();
        message.insert("role".to_owned(), Value::String("user".to_owned()));
        message.insert("parts".to_owned(), Value::Array(parts));

        let mut metadata = call.extensions.to_json_object();
        if !call.target.is_empty() {
            metadata.insert(
                BRIDGE_SKILL_KEY.to_owned(),
                Value::String(call.target.clone()),
            );
        }

        let mut params = Map::new();
        params.insert("message".to_owned(), Value::Object(message));
        if !metadata.is_empty() {
            params.insert("metadata".to_owned(), Value::Object(metadata));
        }
        Ok(WireRequest {
            method: SEND_MESSAGE.to_owned(),
            params: Value::Object(params),
        })
    }

    /// Lifts an A2A reply into the IR.
    ///
    /// A `Task` reply carries its answer in its history; a `Message` reply
    /// carries it in its parts. A task that failed says so in its state,
    /// which becomes the outcome's error flag.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MalformedPayload`] when the result is not an
    /// object.
    fn lift_reply(
        &self,
        result: &Value,
        report: &mut RequestCodecReport,
    ) -> Result<IrOutcome, CrossProtocolError> {
        let Some(object) = result.as_object() else {
            return Err(CrossProtocolError::MalformedPayload {
                side: ProtocolSide::A2a,
                detail: "result is not an object".to_owned(),
            });
        };
        let mut content = if object.contains_key("id") {
            task_content(object)
        } else {
            message_content(object)
        };
        if let Some(artifacts) = object.get("artifacts").and_then(Value::as_array)
            && !artifacts.is_empty()
        {
            // A task's artifacts are outputs in their own right. MCP's
            // reply has one content list and no place for a second, so
            // the artifacts do not travel — and the report says so
            // rather than letting them vanish.
            report.record(RequestCodecStep::Dropped {
                what: "A2A task artifacts",
                count: artifacts.len(),
            });
        }
        if content.is_empty()
            && let Some(state) = state_of(object)
        {
            // An empty task with no history still has an answer, and the
            // answer is the state it ended in.
            content = vec![Content::Text(TextContent::new(state))];
        }
        let is_error =
            state_of(object).is_some_and(|state| state == "failed" || state == "rejected");
        Ok(IrOutcome {
            content,
            structured: None,
            is_error,
            extensions: Extensions::empty(),
        })
    }

    /// Lowers an IR outcome into an A2A reply.
    ///
    /// The reply is a `Message` from the agent, which is what an A2A peer
    /// reads as the answer to a `SendMessage`.
    ///
    /// # Errors
    ///
    /// Never in practice: every field of an [`IrOutcome`] has an A2A form.
    fn lower_reply(
        &self,
        outcome: &IrOutcome,
        report: &mut RequestCodecReport,
    ) -> Result<Value, CrossProtocolError> {
        if outcome.structured.is_some() {
            // A2A has no structured output; a message carries parts, not
            // a sidecar object.
            report.record(RequestCodecStep::Dropped {
                what: "structured content with no A2A counterpart",
                count: 1,
            });
        }
        let mut parts = Vec::new();
        for content in &outcome.content {
            let mut mapping = crate::domain::bridge::a2a_mcp::CrossProtocolReport::new();
            match crate::domain::bridge::a2a_mcp::mcp_content_to_a2a_part(content, &mut mapping) {
                Some(part) => parts.push(part_to_value(&part)),
                None => report.record(RequestCodecStep::Dropped {
                    what: "a content block with no A2A counterpart",
                    count: 1,
                }),
            }
        }
        let mut message = Map::new();
        message.insert("role".to_owned(), Value::String("assistant".to_owned()));
        message.insert("parts".to_owned(), Value::Array(parts));

        let mut out = Map::new();
        out.insert("message".to_owned(), Value::Object(message));
        if outcome.is_error {
            let mut status = Map::new();
            status.insert("state".to_owned(), Value::String("failed".to_owned()));
            out.insert("status".to_owned(), Value::Object(status));
        }
        Ok(Value::Object(out))
    }
}

/// The state a task ended in.
fn state_of(object: &Map<String, Value>) -> Option<&str> {
    object
        .get("status")
        .and_then(Value::as_object)
        .and_then(|status| status.get("state"))
        .and_then(Value::as_str)
}

/// The content of a task's most recent answer.
///
/// A task's answer is the last message in its history that the agent
/// wrote. The task's own identity — its id, its context, its artifacts —
/// belongs to the A2A peer and has no counterpart on the other side, so
/// it does not travel.
fn task_content(object: &Map<String, Value>) -> Vec<Content> {
    let history = object
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    history
        .iter()
        .rev()
        .find_map(|entry| {
            let is_agent = entry
                .as_object()
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                .is_some_and(|role| role != "user");
            if is_agent {
                parse_message(entry).map(|message| message_content_of(&message))
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// The content of a message reply.
fn message_content(object: &Map<String, Value>) -> Vec<Content> {
    parse_message(&Value::Object(object.clone()))
        .map(|message| message_content_of(&message))
        .unwrap_or_default()
}

/// The content of a typed A2A message.
fn message_content_of(message: &Message) -> Vec<Content> {
    let mut report = crate::domain::bridge::a2a_mcp::CrossProtocolReport::new();
    crate::domain::bridge::a2a_mcp::a2a_message_to_mcp_content(message, &mut report)
}

/// A wire message as the typed A2A message.
///
/// A message this build cannot read has no content to carry, and the
/// bridge reports it as empty rather than guessing at the shape.
fn parse_message(value: &Value) -> Option<Message> {
    serde_json::from_value(value.clone()).ok()
}

/// A typed A2A part as the wire form.
///
/// The spec's form is the proto's `oneof`: the object has exactly one of
/// `text`, `file` or `data` as a top-level field, with no `kind`
/// discriminator.
fn part_to_value(part: &Part) -> Value {
    serde_json::to_value(part).unwrap_or_else(|_| {
        let mut map = Map::new();
        map.insert("text".to_owned(), Value::String(String::new()));
        Value::Object(map)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn report() -> RequestCodecReport {
        RequestCodecReport::empty()
    }

    /// The `SendMessage` params an A2A client sends for a call to the
    /// skill `lookup` with `{"query": "weather"}` as its payload.
    fn send_message_params() -> Value {
        json!({
            "message": {
                "role": "user",
                "parts": [{"data": {"query": "weather"}}]
            },
            "metadata": {"bridge_skill": "lookup"}
        })
    }

    #[test]
    fn a_send_message_lifts_into_a_call_with_its_skill_and_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let call = adapter.lift_request("SendMessage", &send_message_params(), &mut rep)?;
        assert_eq!(call.method, IrMethod::Call);
        assert_eq!(call.target, "lookup");
        let arguments = call.arguments.ok_or("the call carried no arguments")?;
        let value: Value = serde_json::from_str(arguments.as_str())?;
        assert_eq!(value.get("query").and_then(Value::as_str), Some("weather"));
        Ok(())
    }

    #[test]
    fn a_send_message_with_no_data_part_carries_no_arguments()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let params = json!({
            "message": {"role": "user", "parts": [{"text": "hi"}]},
            "metadata": {"bridge_skill": "lookup"}
        });
        let call = adapter.lift_request("SendMessage", &params, &mut rep)?;
        assert!(call.arguments.is_none());
        Ok(())
    }

    #[test]
    fn a_part_with_no_counterpart_is_recorded_as_a_loss() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let params = json!({
            "message": {"role": "user", "parts": [{"text": "hi"}]},
            "metadata": {"bridge_skill": "lookup"}
        });
        adapter.lift_request("SendMessage", &params, &mut rep)?;
        assert!(
            rep.steps
                .iter()
                .any(|step| matches!(step, RequestCodecStep::Dropped { count: 1, .. }))
        );
        Ok(())
    }

    #[test]
    fn a_send_message_with_no_message_is_refused() {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let outcome = adapter.lift_request("SendMessage", &json!({}), &mut rep);
        assert!(matches!(
            outcome,
            Err(CrossProtocolError::MalformedPayload {
                side: ProtocolSide::A2a,
                ..
            })
        ));
    }

    #[test]
    fn a_call_lowers_back_to_the_send_message_it_came_from()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let params = send_message_params();
        let call = adapter.lift_request("SendMessage", &params, &mut rep)?;
        let wire = adapter.lower_request(&call, &mut rep)?;
        assert_eq!(wire.method, "SendMessage");
        assert_eq!(wire.params, params);
        Ok(())
    }

    #[test]
    fn a_task_reply_lifts_into_an_outcome_with_its_answer() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let task = json!({
            "id": "task-1",
            "status": {"state": "completed"},
            "history": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "agent", "parts": [{"text": "hello"}]}
            ]
        });
        let outcome = adapter.lift_reply(&task, &mut rep)?;
        assert!(!outcome.is_error);
        let first = outcome.content.first().ok_or("no content")?;
        assert!(matches!(first, Content::Text(text) if text.text == "hello"));
        Ok(())
    }

    #[test]
    fn a_failed_task_lifts_into_an_erroneous_outcome() -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let task = json!({"id": "task-1", "status": {"state": "failed"}});
        let outcome = adapter.lift_reply(&task, &mut rep)?;
        assert!(outcome.is_error);
        Ok(())
    }

    #[test]
    fn an_outcome_lowers_into_a_message_the_agent_wrote() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let outcome = IrOutcome::new(vec![Content::Text(TextContent::new("hello"))]);
        let wire = adapter.lower_reply(&outcome, &mut rep)?;
        assert_eq!(
            wire,
            json!({"message": {"role": "assistant", "parts": [{"text": "hello"}]}})
        );
        Ok(())
    }

    #[test]
    fn a_task_with_artifacts_records_what_it_left_behind() {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let task = json!({
            "id": "task-1",
            "status": {"state": "completed"},
            "artifacts": [{"artifactId": "a-1", "parts": [{"text": "a file"}]}]
        });
        adapter.lift_reply(&task, &mut rep).ok();
        assert!(rep.steps.iter().any(|step| matches!(
            step,
            RequestCodecStep::Dropped { what, count: 1 } if *what == "A2A task artifacts"
        )));
    }

    #[test]
    fn structured_content_records_what_it_left_behind() -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let structured = JsonObject::from_value(json!({"rows": 2}))
            .map_err(|error| format!("the fixture was not a JSON object: {error}"))?;
        let outcome = IrOutcome::new(Vec::new()).with_structured(structured);
        adapter.lower_reply(&outcome, &mut rep)?;
        assert!(
            rep.steps
                .iter()
                .any(|step| matches!(step, RequestCodecStep::Dropped { count: 1, .. }))
        );
        Ok(())
    }

    #[test]
    fn an_erroneous_outcome_lowers_into_a_failed_status() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut rep = report();
        let adapter = A2aAdapter::new();
        let outcome = IrOutcome::new(Vec::new()).with_error(true);
        let wire = adapter.lower_reply(&outcome, &mut rep)?;
        assert_eq!(
            wire.get("status").and_then(|status| status.get("state")),
            Some(&Value::String("failed".to_owned()))
        );
        Ok(())
    }
}
