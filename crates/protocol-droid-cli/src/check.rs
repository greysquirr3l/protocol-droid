//! `protocol-droid bridge --check-result` — what would the bridge do to this?
//!
//! ## Why this exists
//!
//! Content translation only fails in a way you can act on if you find out
//! _before_ the client does. A bridge that silently drops `structuredContent`
//! for a client that negotiated an older revision looks identical to one that
//! works, right up until a tool's output stops being machine-readable. This
//! answers the question without a socket, a peer, or a handshake.
//!
//! ## It asks the real question, not a paraphrase of it
//!
//! The probe calls [`Translator`] — the same type `Bridge::forward` calls —
//! rather than re-deriving the decision from a feature profile. A second copy of
//! the rules is a second copy that can disagree, and the disagreement would
//! show up as "the check said forward" on a bridge that downgraded. The
//! transport here is narrower on purpose: no peers, no envelopes in flight.
//!
//! ## Three outcomes, and the third is the interesting one
//!
//! - **forwarded** — the client can read it, and the reply went out byte for
//!   byte. `unchanged` is asserted rather than assumed, because that is the
//!   claim that protects unmodelled directives and post-build content blocks.
//! - **downgraded** — the client cannot read part of it, a rule the
//!   specification defines was applied, and every step is named.
//! - **refused** — the client cannot read it and the specification defines no
//!   fallback. Exits non-zero: this is a broken pairing, not a warning.
//!
//! ## Two sinks, one serialiser
//!
//! As everywhere in this binary: [`Check`] is serialised once, `--json` picks
//! stdout over stderr, and the human form is a `Display` over the same value.

use std::io::Read as _;

use anyhow::Context as _;
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::application::bridge::Translator;
use protocol_droid::domain::ir::LowerError;
use protocol_droid::domain::{
    CapabilitySet, DegradePolicy, Leg, Negotiation, ProtocolVersion, TransportKind, resolve,
};
use serde::Serialize;
use serde_json::{Map, Value};

/// What the bridge would do with a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// The client can read it; the reply is handed on untouched.
    Forwarded,
    /// Part of it could not be expressed at the client's revision, and a
    /// defined fallback was applied.
    Downgraded,
    /// The client cannot read it and no fallback is defined for that.
    Refused,
}

/// The answer, as one value with two renderings.
#[derive(Debug, Serialize)]
pub struct Check {
    /// Which of the three outcomes this is.
    pub outcome: Outcome,
    /// The revision the client negotiated.
    pub client_version: String,
    /// The revision the server speaks.
    pub server_version: String,
    /// Whether the reply came back byte-identical.
    ///
    /// Separated from `outcome` on purpose: "forwarded" and "the bytes are the
    /// same" are different claims, and only the second is what a client relies
    /// on.
    pub unchanged: bool,
    /// Every downgrade applied, named.
    pub steps: Vec<String>,
    /// The reply the bridge would send.
    pub reply: Value,
    /// Why, in a sentence.
    pub note: String,
}

impl std::fmt::Display for Check {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:?}: {} -> {} ({})",
            self.outcome, self.server_version, self.client_version, self.note
        )?;
        for step in &self.steps {
            write!(formatter, "\n  {step}")?;
        }
        Ok(())
    }
}

/// Reads the reply under test from a path, or from stdin for `-`.
fn read(path: &str) -> anyhow::Result<String> {
    if path == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading the reply from stdin")?;
        Ok(text)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading the reply from {path}"))
    }
}

/// The profile a client at `version` ends up with when nothing is withheld.
///
/// "Nothing is withheld" is the honest question for a probe: the point is what
/// the *revision* can express, and a caller withholding a capability would be
/// asking about a different thing that deserves its own flag.
fn leg(version: ProtocolVersion) -> Leg {
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

/// Runs the probe.
///
/// # Errors
///
/// [`anyhow::Error`] when the payload cannot be read or is not a JSON object.
/// A refusal is *not* an error here — it is a result, reported through
/// [`Check::outcome`] — but the caller still exits non-zero on it.
pub fn run(
    path: &str,
    client_version: ProtocolVersion,
    server_version: ProtocolVersion,
    policy: DegradePolicy,
    json: bool,
) -> anyhow::Result<Check> {
    let text = read(path)?;
    let envelope: Map<String, Value> =
        serde_json::from_str(&text).context("the reply must be a single JSON object")?;

    let check = Translator::new(WireToolResults).translate(
        envelope.clone(),
        server_version,
        &leg(client_version),
        policy,
    );

    let value = match check {
        Ok(translated) => {
            let unchanged = translated.reply == envelope;
            let steps = translated
                .report
                .steps()
                .iter()
                // `Debug` rather than `Display`: the variant name is the
                // greppable one, and giving a domain enum a formatter for a
                // CLI's benefit would be the wrong layer's change.
                .map(|step| format!("{:?} at {}", step.rule, step.target))
                .collect::<Vec<_>>();

            let (outcome, note) = if steps.is_empty() {
                (
                    Outcome::Forwarded,
                    "the client can read this as the server wrote it",
                )
            } else {
                (
                    Outcome::Downgraded,
                    "the client cannot read part of this; each rule below was applied",
                )
            };

            Check {
                outcome,
                client_version: client_version.to_string(),
                server_version: server_version.to_string(),
                unchanged,
                steps,
                reply: Value::Object(translated.reply),
                note: note.to_owned(),
            }
        }
        Err(cause) => Check {
            outcome: Outcome::Refused,
            client_version: client_version.to_string(),
            server_version: server_version.to_string(),
            unchanged: true,
            steps: Vec::new(),
            reply: Value::Object(envelope),
            note: refusal(&cause),
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        eprintln!("{value}");
    }

    Ok(value)
}

/// Explains a refusal by what to change.
///
/// The two `LowerError`s that reach here mean different things to whoever has
/// to act on them, so they are told apart rather than both printed as an error
/// string: one says "downgrade on purpose", the other says "the specification
/// does not say what to give up".
fn refusal(cause: &LowerError) -> String {
    match cause {
        LowerError::FeatureUnavailable { feature, .. } => format!(
            "the client cannot read this and the policy refuses to downgrade \
             {feature}; pass --allow-downgrade to authorise it"
        ),
        LowerError::NoRuleForFeature { feature } => format!(
            "{feature} has no defined fallback, so no policy can authorise \
             downgrading it; the bridge will not invent one"
        ),
        // Every variant is named rather than caught by a wildcard. A fourth
        // variant has to come with a decision about what this command tells
        // the operator, and a wildcard arm would let it arrive with a generic
        // sentence instead.
        LowerError::FieldNotRepresentable { field, .. } => {
            format!("{field} cannot be written for this pair of revisions")
        }
    }
}

/// Whether a refusal should fail the command.
///
/// A refused result is a broken pairing between two revisions. Exiting zero
/// would make a CI check on this command pass on exactly the configuration it
/// exists to catch.
pub fn refused(outcome: Outcome) -> bool {
    outcome == Outcome::Refused
}

#[cfg(test)]
mod tests {
    use protocol_droid::domain::ir::LowerError;
    use protocol_droid::domain::{DegradePolicy, Feature, ProtocolVersion};

    use super::{Check, Outcome, leg, refusal, refused};

    type TestError = Box<dyn std::error::Error>;

    /// The revision that introduced `structuredContent`.
    ///
    /// Used wherever the fixture carries it, because `2026-07-28` *requires*
    /// `resultType` as well and a fixture missing it fails to deserialise — the
    /// codec then reads nothing, the reply is forwarded, and a test about
    /// downgrading passes for entirely the wrong reason.
    const HAS_STRUCTURED: ProtocolVersion = ProtocolVersion::V2025_06_18;

    /// The revision that requires `resultType`, so only a fixture carrying it
    /// can be read at all.
    const HAS_RESULT_TYPE: ProtocolVersion = ProtocolVersion::V2026_07_28;

    #[test]
    fn a_client_that_can_read_it_is_forwarded() -> Result<(), TestError> {
        let envelope = parse(
            r#"{"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":"answer"}]}}"#,
        )?;
        let check = check(
            &envelope,
            HAS_STRUCTURED,
            ProtocolVersion::V2026_07_28,
            DegradePolicy::strict(),
        )?;

        assert_eq!(check.outcome, Outcome::Forwarded);
        assert!(check.unchanged);
        assert!(check.steps.is_empty());
        assert_eq!(check.reply, envelope);
        Ok(())
    }

    /// The named scenario: the client negotiated a revision with no field for
    /// `structuredContent`, the fixture is otherwise readable, and the bytes
    /// change.
    #[test]
    fn a_downgrade_is_named_and_the_bytes_change() -> Result<(), TestError> {
        let envelope = parse(
            r#"{"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":"answer"}],
                "structuredContent":{"a":1}}}"#,
        )?;
        let check = check(
            &envelope,
            ProtocolVersion::V2025_03_26,
            HAS_STRUCTURED,
            DegradePolicy::permissive(),
        )?;

        assert_eq!(check.outcome, Outcome::Downgraded);
        assert!(
            !check.unchanged,
            "a downgrade must actually change the bytes"
        );
        assert!(!check.steps.is_empty(), "a downgrade must name its rule");
        assert_ne!(check.reply, envelope);
        Ok(())
    }

    /// The same input under a strict policy: opposite outcome, and that is
    /// what `--allow-downgrade` buys.
    #[test]
    fn the_same_reply_is_refused_when_downgrading_is_not_authorised() -> Result<(), TestError> {
        let envelope = parse(
            r#"{"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":"answer"}],
                "structuredContent":{"a":1}}}"#,
        )?;
        let check = check(
            &envelope,
            ProtocolVersion::V2025_03_26,
            HAS_STRUCTURED,
            DegradePolicy::strict(),
        )?;

        assert_eq!(check.outcome, Outcome::Refused);
        assert!(
            check.unchanged,
            "a refusal sends the original, never a partial"
        );
        assert!(refused(check.outcome));
        assert!(check.note.contains("--allow-downgrade"), "{}", check.note);
        Ok(())
    }

    /// `resultType` has no defined fallback, so `--allow-downgrade` cannot
    /// authorise it. The flag does not mean "never refuse".
    #[test]
    fn a_feature_with_no_fallback_is_refused_even_when_downgrading_is_allowed()
    -> Result<(), TestError> {
        let envelope = parse(
            r#"{"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":"answer"}],"resultType":"search"}}"#,
        )?;
        let check = check(
            &envelope,
            ProtocolVersion::V2025_03_26,
            HAS_RESULT_TYPE,
            DegradePolicy::permissive(),
        )?;

        assert_eq!(check.outcome, Outcome::Refused);
        assert!(refused(check.outcome));
        assert!(
            check.note.contains("no defined fallback"),
            "the note must separate 'you did not authorise it' from 'the \
             specification does not say what to give up': {}",
            check.note
        );
        assert!(
            !check.note.contains("--allow-downgrade"),
            "pointing at a flag that cannot help is worse than not offering it"
        );
        Ok(())
    }

    /// A block this build cannot model reaches the client byte-identical,
    /// directives and all. The forward path is what makes the bridge usable
    /// against servers newer than it was built for.
    #[test]
    fn an_unmodelled_block_is_forwarded_not_rebuilt() -> Result<(), TestError> {
        let envelope = parse(
            r#"{"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":"answer"},
                           {"type":"annotation","audience":["assistant"]}],
                "x-vendor-directive":{"kept":[1,2,3]}}}"#,
        )?;
        let check = check(
            &envelope,
            HAS_STRUCTURED,
            HAS_STRUCTURED,
            DegradePolicy::strict(),
        )?;

        assert_eq!(check.outcome, Outcome::Forwarded);
        assert!(check.unchanged);
        assert_eq!(check.reply, envelope);
        Ok(())
    }

    /// An upstream at a revision this build has no wire types for cannot be
    /// read at all — so nothing is *lost*, and the reply is forwarded rather
    /// than refused. Refusing here would break a bridge talking to a future
    /// server over a limitation that costs it nothing.
    #[test]
    fn an_upstream_at_an_unknown_revision_is_forwarded_untouched() -> Result<(), TestError> {
        let unknown = ProtocolVersion::Unknown("2027-01-01".parse()?);
        let envelope = parse(
            r#"{"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":"answer"}]}}"#,
        )?;
        let check = check(
            &envelope,
            ProtocolVersion::V2026_07_28,
            unknown,
            DegradePolicy::strict(),
        )?;

        assert_eq!(check.outcome, Outcome::Forwarded);
        assert!(check.unchanged);
        assert!(
            !refused(check.outcome),
            "an unreadable upstream is not a refusal"
        );
        assert_eq!(check.reply, envelope);
        Ok(())
    }

    /// The human rendering and the JSON are two renderings of one value, so
    /// the outcome and both revisions have to appear in both.
    #[test]
    fn the_human_form_names_the_outcome_and_both_versions() -> Result<(), TestError> {
        let envelope = parse(
            r#"{"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":"answer"}],
                "structuredContent":{"a":1}}}"#,
        )?;
        let check = check(
            &envelope,
            ProtocolVersion::V2025_03_26,
            HAS_STRUCTURED,
            DegradePolicy::permissive(),
        )?;
        let rendered = check.to_string();

        assert!(rendered.contains("Downgraded"), "{rendered}");
        assert!(rendered.contains("2025-06-18"), "{rendered}");
        assert!(rendered.contains("2025-03-26"), "{rendered}");
        Ok(())
    }

    /// The two refusals give different advice, because the action differs: one
    /// has a flag that fixes it, the other has nothing to pass.
    #[test]
    fn the_two_refusals_give_different_advice() {
        let unavailable = refusal(&LowerError::FeatureUnavailable {
            feature: Feature::ToolStructuredOutput,
            negotiated: ProtocolVersion::V2025_03_26,
        });
        let no_rule = refusal(&LowerError::NoRuleForFeature {
            feature: Feature::ToolResultType,
        });

        assert_ne!(unavailable, no_rule);
        assert!(unavailable.contains("--allow-downgrade"));
        assert!(!no_rule.contains("--allow-downgrade"));
    }

    /// A payload that is not a JSON object is a read error, not a result.
    #[test]
    fn a_payload_that_is_not_an_object_is_rejected() -> Result<(), TestError> {
        let path = write_temp("[1, 2, 3]")?;
        let outcome = super::run(
            &path,
            ProtocolVersion::V2026_07_28,
            HAS_STRUCTURED,
            DegradePolicy::strict(),
            true,
        );
        let _ = std::fs::remove_file(&path);

        assert!(
            outcome.is_err(),
            "an array is not a reply envelope and must not be reported as one"
        );
        Ok(())
    }

    /// A missing file is an error naming the path, not a panic.
    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let missing = std::env::temp_dir().join("protocol-droid-check-does-not-exist.json");
        let outcome = super::run(
            &missing.to_string_lossy(),
            ProtocolVersion::V2026_07_28,
            HAS_STRUCTURED,
            DegradePolicy::strict(),
            true,
        );

        assert!(outcome.is_err());
    }

    /// The probe's client leg is built from the revision alone, so a client at
    /// its own newest revision can read everything that revision defines.
    #[test]
    fn a_client_leg_is_built_from_its_revision_alone() {
        let version = HAS_RESULT_TYPE;
        assert!(
            leg(version).profile.allows(Feature::ToolStructuredOutput),
            "2026-07-28 defines structuredContent, so its own profile must have it"
        );
    }

    /// Exactly three outcomes, and `refused` agrees with only one of them.
    #[test]
    fn only_the_refusal_fails_the_command() {
        for outcome in [Outcome::Forwarded, Outcome::Downgraded, Outcome::Refused] {
            assert_eq!(refused(outcome), outcome == Outcome::Refused);
        }
    }

    /// Run the probe over an in-memory reply.
    ///
    /// Goes through the real file path, so the probe and its tests cannot drift
    /// apart on how a payload is obtained.
    fn check(
        envelope: &serde_json::Value,
        client: ProtocolVersion,
        server: ProtocolVersion,
        policy: DegradePolicy,
    ) -> Result<Check, TestError> {
        let path = write_temp(&envelope.to_string())?;
        let result = super::run(&path, client, server, policy, true);
        let _ = std::fs::remove_file(&path);
        Ok(result?)
    }

    fn parse(text: &str) -> Result<serde_json::Value, TestError> {
        Ok(serde_json::from_str(text)?)
    }

    /// Writes `text` to a file unique to this test and returns its path.
    fn write_temp(text: &str) -> Result<String, TestError> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "protocol-droid-check-{}-{:?}.json",
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::thread::current().id()
        ));
        std::fs::write(&path, text)?;
        Ok(path.to_string_lossy().into_owned())
    }
}
