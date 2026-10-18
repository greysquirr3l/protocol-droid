//! Translating a `tools/call` result across a revision boundary.
//!
//! # Where the revisions actually disagree
//!
//! At the envelope level a bridge is free: `jsonrpc`, `id`, `method`, `params`,
//! `result` and `error` mean the same thing in every revision of JSON-RPC, so
//! moving a message between two peers loses nothing and there is no downgrade to
//! record. The first place the revisions genuinely disagree is `CallToolResult`,
//! and that disagreement has to be resolved against the _client's_ profile,
//! because the client is the side that will fail to read the answer.
//!
//! # Why an untranslated reply is handed back as the peer wrote it
//!
//! The obvious implementation re-encodes every reply: lift, lower, splice. It is
//! wrong, and wrong in a way that only shows up against a peer this build does
//! not fully model.
//!
//! The generated content union refuses an unrecognised `type` outright — and
//! deliberately, so that a malformed block cannot be quietly accepted as
//! whichever alternative happens to parse. So a server pushing a block type
//! added after this build deserialised would fail to lift *at all*, and a
//! bridge that treated that as an error would refuse to forward a message it had
//! no reason to touch. That is the opposite of what this crate promises: an
//! unmodelled directive is forwarded, not interpreted.
//!
//! So the reply is rebuilt only once a downgrade has actually been authorised.
//! Everything else — a result the client can already read, a shape this build
//! cannot read, a reply that is not a result at all — comes back
//! byte-identical, with an empty report, because nothing was decided.
//!
//! # Why a `lift` that reads nothing is not a failure
//!
//! It is the same claim from the other side. A value this build cannot read may
//! be perfectly readable by the client on the far side, which can be newer than
//! the bridge itself. Refusing it would make the bridge the component that
//! breaks forward compatibility, and the symptom would appear at the client as a
//! bridge bug rather than as an extension it does not know.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::domain::ir::ToolResult;
use crate::domain::ir::lower::{LowerError, TranslationReport, check_expressible};
use crate::domain::feature::Feature;
use crate::domain::{DegradePolicy, Leg, ProtocolVersion};

/// The JSON-RPC member a reply's payload travels under.
const RESULT: &str = "result";

/// The member a refusal travels under, and which excludes [`RESULT`].
const ERROR: &str = "error";

/// The `resultType` of an ordinary, finished result.
const RESULT_TYPE_COMPLETE: &str = "complete";

/// Reads and writes a `ToolResult` in one revision's wire encoding.
///
/// Declared by the consumer — this module — and implemented by the codec layer,
/// which is the only place that knows a revision's generated types. The bridge
/// names no wire type, so it could not implement this even if it wanted to.
///
/// # Why `lift` answers `Option`
///
/// `None` means *this build does not read that shape*, and it is a decision
/// rather than a fault: the caller forwards the value untouched so a peer that
/// does understand it still receives it. A build that had no wire types at all
/// for the revision would answer `None` for everything, and the bridge would
/// degrade into a pure pass-through rather than into a component that breaks a
/// connection it never had to translate.
///
/// # Example
///
/// The smallest implementor that could exist. A production one is
/// [`WireToolResults`](crate::adapters::secondary::codec::result::WireToolResults),
/// which dispatches to the six per-revision codecs.
///
/// ```
/// use protocol_droid::application::bridge::ToolResultCodec;
/// use protocol_droid::domain::ir::{LowerError, ToolResult, TranslationReport};
/// use protocol_droid::domain::{DegradePolicy, ProtocolVersion};
/// use serde_json::Value;
///
/// /// Reads nothing, so every reply through this codec passes through.
/// struct ReadsNothing;
///
/// impl ToolResultCodec for ReadsNothing {
///     fn lift(&self, _version: ProtocolVersion, _wire: &Value) -> Option<ToolResult> {
///         None
///     }
///
///     fn lower(
///         &self,
///         _version: ProtocolVersion,
///         _value: ToolResult,
///         _policy: DegradePolicy,
///         _report: &mut TranslationReport,
///     ) -> Result<Value, LowerError> {
///         Ok(Value::Null)
///     }
/// }
/// ```
/// A `Bridge` holds its codec across an `await` while the reply is in flight,
/// and a serving loop may drive several bridges at once, so a codec that is not
/// `Sync` would make every one of those futures non-`Send`. The bound is on the
/// port rather than on `Bridge` for one reason: this crate's ports already
/// require `Send + Sync`, and a codec that only worked on the current thread
/// would not be a codec a bridge could hold.
pub trait ToolResultCodec: Send + Sync {
    /// The [`ToolResult`] `wire` holds at `version`, or `None` if this build
    /// does not read it.
    fn lift(&self, version: ProtocolVersion, wire: &Value) -> Option<ToolResult>;

    /// Writes `value` at `version`, recording anything the codec changed in
    /// `report`.
    ///
    /// `value` arrives already lowered — the codec asks
    /// [`lower_tool_result`](crate::domain::ir::lower::lower_tool_result)
    /// itself rather than being handed a decision, which is why this is not a
    /// bare serialisation and why the report comes back from here.
    ///
    /// # Errors
    ///
    /// [`LowerError::FieldNotRepresentable`] when the value does not fit a
    /// shape `version` types — the typed-`meta` case.
    fn lower(
        &self,
        version: ProtocolVersion,
        value: ToolResult,
        policy: DegradePolicy,
        report: &mut TranslationReport,
    ) -> Result<Value, LowerError>;
}

/// Lets a shared codec be a codec.
///
/// `Arc<T>` satisfies none of this crate's ports on its own — it is not a
/// `FrameSink` and it is not a `FrameSource` either, for the same reason. A
/// `Bridge` is usually held by a serving loop and a test wants to watch what it
/// did, and an `Arc` is the only thing that gives both at once.
impl<C: ToolResultCodec> ToolResultCodec for Arc<C> {
    fn lift(&self, version: ProtocolVersion, wire: &Value) -> Option<ToolResult> {
        (**self).lift(version, wire)
    }

    fn lower(
        &self,
        version: ProtocolVersion,
        value: ToolResult,
        policy: DegradePolicy,
        report: &mut TranslationReport,
    ) -> Result<Value, LowerError> {
        (**self).lower(version, value, policy, report)
    }
}

/// What translating a reply produced.
#[derive(Debug)]
pub struct Translated {
    /// The reply to send downstream.
    pub reply: Map<String, Value>,
    /// What the translation changed. Empty whenever the reply passed through.
    pub report: TranslationReport,
}

/// Turns a reply into one the downstream client can read.
#[derive(Debug)]
pub struct Translator<C> {
    codec: C,
}

impl<C: ToolResultCodec> Translator<C> {
    /// A translator over one codec.
    #[must_use]
    pub const fn new(codec: C) -> Self {
        Self { codec }
    }

    /// Translates `reply` — which arrived in `from`'s encoding — into something
    /// the `to` leg negotiated for.
    ///
    /// Returns the reply untouched with an empty report whenever there is
    /// nothing to decide. That is the common case and it is deliberate: see the
    /// module docs for why rebuilding an untranslated reply is a bug rather than
    /// a thoroughness.
    ///
    /// # Errors
    ///
    /// [`LowerError`] when the reply needs a downgrade `policy` does not
    /// authorise, or when the codec cannot write the lowered value at `to`. The
    /// caller refuses on the error rather than forwarding a result the client
    /// cannot read.
    ///
    /// # Examples
    ///
    /// A result the client can read is handed back exactly as it arrived.
    ///
    /// ```
    /// use protocol_droid::application::bridge::{ToolResultCodec, Translator};
    /// use protocol_droid::domain::ir::{LowerError, ToolResult, TranslationReport};
    /// use protocol_droid::domain::{
    ///     CapabilitySet, DegradePolicy, Leg, Negotiation, ProtocolVersion, TransportKind, resolve,
    /// };
    /// use serde_json::{Map, Value, json};
    ///
    /// struct ReadsNothing;
    ///
    /// impl ToolResultCodec for ReadsNothing {
    ///     fn lift(&self, _version: ProtocolVersion, _wire: &Value) -> Option<ToolResult> {
    ///         None
    ///     }
    ///
    ///     fn lower(
    ///         &self,
    ///         _version: ProtocolVersion,
    ///         _value: ToolResult,
    ///         _policy: DegradePolicy,
    ///         _report: &mut TranslationReport,
    ///     ) -> Result<Value, LowerError> {
    ///         Ok(Value::Null)
    ///     }
    /// }
    ///
    /// let version = ProtocolVersion::V2025_03_26;
    /// let caps = CapabilitySet::all_for(version);
    /// let leg = Leg::new(
    ///     version,
    ///     resolve(Negotiation {
    ///         version,
    ///         client_caps: caps,
    ///         server_caps: caps,
    ///         transport: TransportKind::Stdio,
    ///     }),
    /// );
    ///
    /// let reply: Map<String, Value> = serde_json::from_value(json!({
    ///     "jsonrpc": "2.0",
    ///     "id": 1,
    ///     "result": { "content": [{ "type": "text", "text": "hi" }] },
    /// }))?;
    ///
    /// let translator = Translator::new(ReadsNothing);
    /// let translated =
    ///     translator.translate(reply.clone(), version, &leg, DegradePolicy::strict())?;
    ///
    /// assert_eq!(translated.reply, reply);
    /// assert!(translated.report.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn translate(
        &self,
        reply: Map<String, Value>,
        from: ProtocolVersion,
        to: &Leg,
        policy: DegradePolicy,
    ) -> Result<Translated, LowerError> {
        let untouched = TranslationReport::new(to.version);

        // Scoped so the borrow of `reply` ends before it is rebuilt below. The
        // early returns move it on those paths only; the fall-through path is
        // the one that goes on to use it.
        let lifted = {
            // A refusal is the upstream's own words about a request, not a
            // result this layer has any opinion on.
            if reply.contains_key(ERROR) {
                return Ok(Translated {
                    reply,
                    report: untouched,
                });
            }
            let Some(payload) = reply.get(RESULT) else {
                return Ok(Translated {
                    reply,
                    report: untouched,
                });
            };
            // `ToolResult` is an object. A scalar or an array here is not one,
            // whatever method produced it, so there is nothing to translate.
            if !payload.is_object() {
                return Ok(Translated {
                    reply,
                    report: untouched,
                });
            }
            self.codec.lift(from, payload)
        };

        // Read nothing. Forwarded anyway — see the module docs.
        let Some(lifted) = lifted else {
            return Ok(Translated {
                reply,
                report: untouched,
            });
        };

        // `complete` is what a result with no `resultType` means: the
        // `2026-07-28` text has a client read an absent field as `complete`. A
        // client at a revision without the field reads it the same way, so
        // there is nothing to lose here, and refusing would make every tool
        // result from a new server unreadable to an old client. Any other value
        // (`input_required`) has no older form and is left for the check below
        // to refuse.
        let mut lifted = lifted;
        if lifted.result_type.as_deref() == Some(RESULT_TYPE_COMPLETE)
            && !to.profile.allows(Feature::ToolResultType)
        {
            lifted.result_type = None;
        }

        // Nothing the client cannot read means nothing to decide, which is what
        // makes returning the peer's own bytes correct rather than merely
        // convenient. Asking before lowering is also what keeps the codec from
        // re-encoding a message it was not asked to change.
        if check_expressible(&lifted.required_features(), &to.profile, to.version).is_ok() {
            return Ok(Translated {
                reply,
                report: untouched,
            });
        }

        let mut report = TranslationReport::new(to.version);
        let value = self.codec.lower(to.version, lifted, policy, &mut report)?;

        // Replaces the member. It was read a few lines above and nothing
        // removed it, so this cannot add a `result` that was not there — and
        // adding one would invent an answer the upstream never sent.
        let mut reply = reply;
        reply.insert(RESULT.to_owned(), value);

        Ok(Translated { reply, report })
    }
}

#[cfg(test)]
pub(in crate::application::bridge) mod doubles {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};

    use super::ToolResultCodec;
    use crate::domain::ir::{LowerError, ToolResult, TranslationReport};
    use crate::domain::{DegradePolicy, ProtocolVersion};

    /// A codec that reads one fixed value and counts every crossing.
    ///
    /// The counters are atomics rather than a `Mutex` because a poisoned lock
    /// would add an error type to every assertion for no benefit: nothing here
    /// can fail while the lock is held.
    pub(in crate::application::bridge) struct Counting {
        readable: Option<ToolResult>,
        lifts: AtomicUsize,
        lowers: AtomicUsize,
    }

    impl Counting {
        /// A codec that reads `readable` back from anything.
        #[must_use]
        pub(in crate::application::bridge) fn reading(readable: ToolResult) -> Self {
            Self {
                readable: Some(readable),
                lifts: AtomicUsize::new(0),
                lowers: AtomicUsize::new(0),
            }
        }

        /// A codec that reads nothing, so every reply passes through.
        #[must_use]
        pub(in crate::application::bridge) fn reading_nothing() -> Self {
            Self {
                readable: None,
                lifts: AtomicUsize::new(0),
                lowers: AtomicUsize::new(0),
            }
        }

        /// How many times a value was offered to be read.
        pub(in crate::application::bridge) fn lifts(&self) -> usize {
            self.lifts.load(Ordering::Relaxed)
        }

        /// How many times a value was offered to be written.
        ///
        /// Counts entries, not successes: the codec is where the policy is
        /// enforced, so a refusal increments this too.
        pub(in crate::application::bridge) fn lowers(&self) -> usize {
            self.lowers.load(Ordering::Relaxed)
        }
    }

    impl ToolResultCodec for Counting {
        fn lift(&self, _version: ProtocolVersion, _wire: &Value) -> Option<ToolResult> {
            self.lifts.fetch_add(1, Ordering::Relaxed);
            self.readable.clone()
        }

        fn lower(
            &self,
            _version: ProtocolVersion,
            value: ToolResult,
            policy: DegradePolicy,
            report: &mut TranslationReport,
        ) -> Result<Value, LowerError> {
            self.lowers.fetch_add(1, Ordering::Relaxed);
            let (lowered, lowered_report) = crate::domain::ir::lower::lower_tool_result(
                value,
                &crate::domain::FeatureProfile::empty(),
                ProtocolVersion::V2025_03_26,
                policy,
            )?;
            report.absorb(&lowered_report);
            // A marker rather than the lowered value: these tests are about
            // *whether* the result was rebuilt, not about how it reads, which
            // is the codec's own subject and is covered by its tests.
            Ok(json!({ "rebuilt": true, "blocks": lowered.content.len() }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Map, Value, json};

    use super::doubles::Counting;
    use super::{ERROR, RESULT, Translator};
    use crate::domain::ir::lower::LowerError;
    use crate::domain::ir::{FeatureSet, ToolResult};
    use crate::domain::{
        CapabilitySet, DegradePolicy, Feature, FeatureProfile, Leg, Negotiation, ProtocolVersion,
        TransportKind, resolve,
    };

    type TestError = Box<dyn std::error::Error>;

    /// A leg that can express what `2025-03-26` can, and nothing more.
    fn client_leg() -> Leg {
        let version = ProtocolVersion::V2025_03_26;
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

    /// A leg with no features at all, so nothing but plain text is expressible.
    fn bare_leg() -> Leg {
        Leg::new(ProtocolVersion::V2024_11_05, FeatureProfile::empty())
    }

    fn reply(payload: Value) -> Map<String, Value> {
        let mut reply = Map::new();
        reply.insert("jsonrpc".to_owned(), json!("2.0"));
        reply.insert("id".to_owned(), json!(1));
        reply.insert(RESULT.to_owned(), payload);
        reply
    }

    fn structured() -> Result<ToolResult, TestError> {
        Ok(ToolResult::text("answer").with_structured(r#"{"a":1}"#)?)
    }

    /// The claim the whole module rests on: nothing to decide means the peer's
    /// own bytes come back, and the codec is not even asked.
    #[test]
    fn a_reply_needing_no_translation_never_reaches_the_codec() -> Result<(), TestError> {
        let codec = Arc::new(Counting::reading(ToolResult::text("answer")));
        let sent = reply(json!({ "content": [{ "type": "text", "text": "answer" }] }));

        let translated = Translator::new(Arc::clone(&codec)).translate(
            sent.clone(),
            ProtocolVersion::V2026_07_28,
            &client_leg(),
            DegradePolicy::strict(),
        )?;

        assert_eq!(translated.reply, sent, "the peer's own bytes must survive");
        assert!(translated.report.is_empty());
        assert_eq!(
            codec.lifts(),
            1,
            "it looked, found nothing to do, and stopped"
        );
        assert_eq!(
            codec.lowers(),
            0,
            "a reply with nothing to decide is not rebuilt"
        );
        Ok(())
    }

    /// A finished result from a `2026-07-28` server reads the same to a client
    /// whose revision has no `resultType`, so it is forwarded rather than refused.
    #[test]
    fn a_complete_result_type_does_not_stop_an_older_client() -> Result<(), TestError> {
        let mut finished = ToolResult::text("answer");
        finished.result_type = Some("complete".to_owned());
        let codec = Arc::new(Counting::reading(finished));
        let sent = reply(json!({
            "content": [{ "type": "text", "text": "answer" }],
            "resultType": "complete",
        }));

        let translated = Translator::new(Arc::clone(&codec)).translate(
            sent.clone(),
            ProtocolVersion::V2026_07_28,
            &client_leg(),
            DegradePolicy::strict(),
        )?;

        assert_eq!(translated.reply, sent);
        assert_eq!(codec.lowers(), 0);
        Ok(())
    }

    /// `input_required` asks the client to do something an older revision cannot
    /// express, and no policy can make it complete.
    #[test]
    fn an_input_required_result_is_still_refused_for_an_older_client() {
        let mut pending = ToolResult::text("answer");
        pending.result_type = Some("input_required".to_owned());
        let codec = Arc::new(Counting::reading(pending));
        let sent = reply(json!({
            "content": [{ "type": "text", "text": "answer" }],
            "resultType": "input_required",
        }));

        let outcome = Translator::new(codec).translate(
            sent,
            ProtocolVersion::V2026_07_28,
            &client_leg(),
            DegradePolicy::permissive(),
        );

        assert!(outcome.is_err(), "{outcome:?}");
    }

    /// A value this build cannot read is forwarded, not refused.
    ///
    /// The codec *was* asked — so this is not a claim that the bridge never
    /// looks — and what came back is the original, untouched.
    #[test]
    fn a_shape_this_build_cannot_read_is_forwarded_untouched() -> Result<(), TestError> {
        let codec = Arc::new(Counting::reading_nothing());
        let sent =
            reply(json!({ "content": [{ "type": "annotation", "audience": ["assistant"] }] }));

        let translated = Translator::new(Arc::clone(&codec)).translate(
            sent.clone(),
            ProtocolVersion::V2026_07_28,
            &client_leg(),
            DegradePolicy::strict(),
        )?;

        assert_eq!(translated.reply, sent);
        assert!(translated.report.is_empty());
        assert_eq!(codec.lifts(), 1);
        assert_eq!(codec.lowers(), 0);
        Ok(())
    }

    /// The `structured-output-downlevel` scenario, on the reply path.
    #[test]
    fn an_authorised_downgrade_rebuilds_the_result_and_records_it() -> Result<(), TestError> {
        let codec = Arc::new(Counting::reading(structured()?));
        let sent = reply(json!({ "content": [{ "type": "text", "text": "answer" }],
                                 "structuredContent": { "a": 1 } }));

        let translated = Translator::new(Arc::clone(&codec)).translate(
            sent,
            ProtocolVersion::V2026_07_28,
            &bare_leg(),
            DegradePolicy::permissive(),
        )?;

        assert!(!translated.report.is_empty(), "a rebuild must be recorded");
        assert_eq!(
            translated.reply.get(RESULT),
            Some(&json!({ "rebuilt": true, "blocks": 2 }))
        );
        assert_eq!(codec.lowers(), 1);
        Ok(())
    }

    /// The fail-secure half: the default policy refuses rather than downgrading,
    /// and the refusal names what the client could not read.
    ///
    /// Note that the codec *is* asked, and refuses from inside its own
    /// `lower`. That is deliberate: the translator cannot pre-check the policy,
    /// because only the codec knows which features lowering will actually have
    /// to strip. A second opinion in the translator is how two revisions start
    /// disagreeing about what they support.
    #[test]
    fn a_strict_policy_refuses_rather_than_writing() -> Result<(), TestError> {
        let codec = Arc::new(Counting::reading(structured()?));
        let sent = reply(json!({ "content": [], "structuredContent": { "a": 1 } }));

        let outcome = Translator::new(Arc::clone(&codec)).translate(
            sent,
            ProtocolVersion::V2026_07_28,
            &bare_leg(),
            DegradePolicy::strict(),
        );

        assert!(
            matches!(
                outcome,
                Err(LowerError::FeatureUnavailable {
                    feature: Feature::ToolStructuredOutput,
                    ..
                })
            ),
            "expected the named feature to be refused, got {outcome:?}"
        );
        assert_eq!(
            codec.lowers(),
            1,
            "the codec is where the policy is enforced, so it is entered once"
        );
        Ok(())
    }

    /// A refusal is the upstream's own words, not a result to translate.
    #[test]
    fn an_error_reply_is_never_rebuilt() -> Result<(), TestError> {
        let codec = Arc::new(Counting::reading(structured()?));
        let mut sent = Map::new();
        sent.insert("jsonrpc".to_owned(), json!("2.0"));
        sent.insert("id".to_owned(), json!(1));
        sent.insert(
            ERROR.to_owned(),
            json!({ "code": -32601, "message": "no such tool" }),
        );

        let translated = Translator::new(Arc::clone(&codec)).translate(
            sent.clone(),
            ProtocolVersion::V2026_07_28,
            &bare_leg(),
            DegradePolicy::permissive(),
        )?;

        assert_eq!(translated.reply, sent);
        assert!(translated.report.is_empty());
        assert_eq!(codec.lifts(), 0, "an error envelope is not a tool result");
        Ok(())
    }

    /// A payload that is not an object is not a `ToolResult`, whatever produced
    /// it. `tools/list` is the real case.
    #[test]
    fn a_non_object_payload_is_left_alone() -> Result<(), TestError> {
        let codec = Arc::new(Counting::reading(structured()?));
        let sent = reply(json!([{ "name": "echo" }]));

        let translated = Translator::new(Arc::clone(&codec)).translate(
            sent.clone(),
            ProtocolVersion::V2026_07_28,
            &bare_leg(),
            DegradePolicy::permissive(),
        )?;

        assert_eq!(translated.reply, sent);
        assert_eq!(codec.lowers(), 0);
        Ok(())
    }

    /// The gate the early return rests on: a value needing nothing is a subset
    /// of any profile, so "nothing to decide" is reachable rather than dead.
    #[test]
    fn a_text_result_needs_nothing_from_any_profile() {
        let required = ToolResult::text("answer").required_features();
        let available = FeatureSet::union_of(FeatureProfile::empty().iter());
        assert!(required.is_subset_of(available));
    }
}
