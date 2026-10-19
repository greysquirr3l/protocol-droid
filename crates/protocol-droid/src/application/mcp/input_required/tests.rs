use std::sync::LazyLock;

use super::test_keys::random_bytes;
use super::{InputRequired, StateKey, capability_for, is_input_required};
use crate::domain::ir::JsonObject;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The key the tests share. Drawn once, so `seal` and `open` agree within a run.
static KEY: LazyLock<[u8; 32]> = LazyLock::new(random_bytes);

fn key() -> StateKey {
    StateKey::from_bytes(*KEY)
}

#[test]
fn a_sealed_state_opens_to_itself() {
    let sealed = key().seal("round-1.with.dots").unwrap_or_default();
    assert_eq!(key().open(&sealed), Some("round-1.with.dots"));
}

#[test]
fn an_altered_state_does_not_open() {
    let sealed = key().seal("round-1").unwrap_or_default();
    let tampered = sealed.replace("round-1", "round-2");
    assert_eq!(key().open(&tampered), None);
}

#[test]
fn an_altered_mac_does_not_open() {
    let sealed = key().seal("round-1").unwrap_or_default();
    let flipped = sealed
        .chars()
        .enumerate()
        .map(|(index, c)| {
            if index == 0 {
                if c == '0' { '1' } else { '0' }
            } else {
                c
            }
        })
        .collect::<String>();
    assert_eq!(key().open(&flipped), None);
}

#[test]
fn a_state_sealed_under_another_key_does_not_open() {
    let sealed = super::test_keys::random_key()
        .seal("round-1")
        .unwrap_or_default();
    assert_eq!(key().open(&sealed), None);
}

#[test]
fn garbage_does_not_open() {
    for garbage in ["", ".", "nodot", "zz.state", "abc.state"] {
        assert_eq!(key().open(garbage), None, "{garbage:?}");
    }
}

#[test]
fn the_key_is_never_printed() {
    assert_eq!(format!("{:?}", key()), "StateKey(<redacted>)");
}

/// `seal` produces HMAC-SHA256 over the raw key bytes.
///
/// Spelled out here rather than asserted against a published vector, because
/// the interesting claim is about *this* code: that `seal` is HMAC-SHA256 and
/// not a plain SHA-256 over the key concatenated with the state. That
/// difference is the whole security property — a plain hash would let anyone
/// who can read one sealed value forge another, because the key would not be
/// mixed in the way a MAC requires.
///
/// Asserting against an RFC vector instead would prove something weaker and
/// untrue about this function: it would exercise a `new_from_slice` written
/// here, and pass unchanged if `seal` were rewritten to use a different
/// construction entirely. The expected MAC below is computed through the same
/// primitive but not through `seal`, so the two sides can disagree.
#[test]
fn the_mac_is_hmac_sha256() -> TestResult {
    use hmac::{Hmac, KeyInit, Mac};
    let key = random_bytes();
    let state = "what do ya want for nothing?";
    let sealed = StateKey::from_bytes(key)
        .seal(state)
        .ok_or("HMAC-SHA256 refused a 32-byte key")?;
    let (mac, carried) = sealed
        .split_once('.')
        .ok_or("the sealed value carried no separator")?;
    assert_eq!(carried, state, "the state did not survive the round trip");

    let mut expected = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(&key)?;
    expected.update(state.as_bytes());
    assert_eq!(
        mac,
        super::hex(&expected.finalize().into_bytes()),
        "seal is not HMAC-SHA256 over the key bytes",
    );
    Ok(())
}

#[test]
fn an_interim_result_names_its_type_and_its_requests() -> TestResult {
    let ask = JsonObject::new(r#"{"message":"m","requestedSchema":{"type":"object"}}"#)?;
    let result = InputRequired::new()
        .elicit("user_name", &ask)
        .list_roots("roots")
        .with_state("sealed")
        .into_result();
    let parsed: serde_json::Map<String, serde_json::Value> = serde_json::from_str(result.as_str())?;
    assert!(is_input_required(&parsed));
    assert_eq!(
        parsed
            .get("requestState")
            .and_then(serde_json::Value::as_str),
        Some("sealed")
    );
    let requests = parsed
        .get("inputRequests")
        .and_then(serde_json::Value::as_object)
        .ok_or("no inputRequests")?;
    assert_eq!(
        requests
            .get("user_name")
            .and_then(|request| request.get("method"))
            .and_then(serde_json::Value::as_str),
        Some("elicitation/create")
    );
    assert_eq!(
        requests
            .get("roots")
            .and_then(|request| request.get("params")),
        Some(&serde_json::json!({}))
    );
    Ok(())
}

#[test]
fn each_input_method_names_its_capability() {
    assert_eq!(capability_for("sampling/createMessage"), Some("sampling"));
    assert_eq!(capability_for("elicitation/create"), Some("elicitation"));
    assert_eq!(capability_for("roots/list"), Some("roots"));
    assert_eq!(capability_for("tools/list"), None);
}
