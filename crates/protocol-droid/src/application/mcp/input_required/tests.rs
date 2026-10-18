use super::{InputRequired, StateKey, capability_for, is_input_required};
use crate::domain::ir::JsonObject;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn key() -> StateKey {
    StateKey::from_bytes([7; 32])
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
        .map(|(index, c)| if index == 0 { if c == '0' { '1' } else { '0' } } else { c })
        .collect::<String>();
    assert_eq!(key().open(&flipped), None);
}

#[test]
fn a_state_sealed_under_another_key_does_not_open() {
    let sealed = StateKey::from_bytes([1; 32]).seal("round-1").unwrap_or_default();
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

/// RFC 4231 test case 2, through the same construction `seal` uses.
#[test]
fn the_mac_is_hmac_sha256() -> TestResult {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(b"Jefe")?;
    mac.update(b"what do ya want for nothing?");
    assert_eq!(
        super::hex(&mac.finalize().into_bytes()),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
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
        parsed.get("requestState").and_then(serde_json::Value::as_str),
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
