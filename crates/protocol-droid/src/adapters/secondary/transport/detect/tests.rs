//! The detection rule, exercised against servers that answer what it needs to.
//!
//! Inline rather than in `tests/`, because this is the one place the rule's
//! *reactions* are the claim, and a rule with no in-module test is a rule with no
//! reference to it — which the build says out loud.

use axum::http::StatusCode;

use super::{DetectConfig, DetectError, Detected, detect};
use crate::adapters::primary::transport::legacy_sse::DEFAULT_MESSAGE_PATH;
use crate::adapters::secondary::transport::test_support::ScriptedServer;
use crate::domain::TransportKind;

type TestError = Box<dyn std::error::Error>;

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

fn require(condition: bool, because: &str) -> Result<(), TestError> {
    if condition {
        Ok(())
    } else {
        Err(fail(because.to_owned()))
    }
}

/// Takes references because it only compares: a by-value signature makes every
/// caller clone a field to print it in a message it may never build.
fn require_eq<T>(got: &T, wanted: &T, because: &str) -> Result<(), TestError>
where
    T: PartialEq + std::fmt::Debug,
{
    if got == wanted {
        Ok(())
    } else {
        Err(fail(format!("{because}: wanted {wanted:?}, got {got:?}")))
    }
}



/// The rule's first branch: the probe worked, so the server is the new one.
#[tokio::test]
async fn a_probe_that_succeeds_means_streamable_http() -> Result<(), TestError> {
    let server = ScriptedServer::start(Vec::new()).await?
        .with_post_status(StatusCode::OK, "application/json", r#"{"ok":true}"#);
    let detected = detect(
        DetectConfig::new(server.url())?,
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    )
    .await?;

    require_eq(
        &detected.transport(),
        &TransportKind::StreamableHttp,
        "a probe that succeeds is a server speaking the new transport",
    )?;
    let Detected::Streamable { result, .. } = &detected else {
        return Err(fail("expected the Streamable HTTP branch"));
    };
    require_eq(
        &String::from_utf8_lossy(result).as_ref(),
        &r#"{"ok":true}"#,
        "the probe's own response is handed back, because the caller has to read the initialize result",
    )
}

/// The rule's second branch: 4xx means try the old transport.
///
/// `405` and `404` are the two the specification names by name, so both are
/// here rather than a representative one — a client that special-cases `405`
/// alone passes a single-status test and fails against a server that says `404`.
#[tokio::test]
async fn a_4xx_probe_falls_back_to_the_old_transport() -> Result<(), TestError> {
    for status in [StatusCode::METHOD_NOT_ALLOWED, StatusCode::NOT_FOUND] {
        let server = ScriptedServer::start(vec![format!(
            "event: endpoint\ndata: {DEFAULT_MESSAGE_PATH}?sessionId=1\n\n"
        )]).await?
        .with_post_status(status, "text/plain", "no");
        let detected = detect(
            DetectConfig::new(server.url())?,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await?;

        require_eq(
            &detected.transport(),
            &TransportKind::LegacySse,
            "a 4xx is the signal to try the old transport",
        )?;
        let client = detected.into_legacy()?;
        require_eq(
            &client.message_endpoint().query(),
            &Some("sessionId=1"),
            "and the client that comes back is already connected, endpoint event read and checked",
        )?;
    }
    Ok(())
}

/// **A 5xx is not a version signal, and this is the test that says so.**
///
/// The rule says 4xx. A server answering `500` has spoken: it is a server
/// speaking the new transport with something broken behind it. Falling back
/// there would open a second transport on top of a real failure and report the
/// wrong cause — the caller would debug the old transport while the new one
/// returns `500` for everything.
///
/// Asserted by counting requests as well as by the error, so a client that
/// *also* tried the `GET` fails even if it reported the right error afterwards.
#[tokio::test]
async fn a_5xx_probe_does_not_fall_back() -> Result<(), TestError> {
    for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_GATEWAY,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let server = ScriptedServer::start(vec![format!(
            "event: endpoint\ndata: {DEFAULT_MESSAGE_PATH}\n\n"
        )]).await?
        .with_post_status(status, "text/plain", "broken");

        let Err(error) = detect(
            DetectConfig::new(server.url())?,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await
        else {
            return Err(fail(format!(
                "{status} must not be read as a version signal"
            )));
        };
        require(
            matches!(error, DetectError::Unusable { .. }),
            "a 5xx is unusable, which is a different failure from a refused probe",
        )?;
        require_eq(
            &server.gets(),
            &0,
            "and the old transport is never opened, because the server already answered",
        )?;
    }
    Ok(())
}

/// Both halves of the rule ran and neither produced a transport.
///
/// Carries the status that triggered the fallback, so a reader is sent to the
/// half that failed rather than left guessing which one.
#[tokio::test]
async fn a_refused_probe_followed_by_a_refused_get_names_both() -> Result<(), TestError> {
    let server = ScriptedServer::start(Vec::new()).await?
        .with_post_status(StatusCode::METHOD_NOT_ALLOWED, "text/plain", "no")
        .with_get_status(StatusCode::NOT_FOUND, "text/plain");
    let result = detect(
        DetectConfig::new(server.url())?,
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    )
    .await;
    let Err(DetectError::Neither { status, .. }) = result else {
        return Err(fail("neither transport answered; that has to be its own error"));
    };
    require_eq(
        &status,
        &StatusCode::METHOD_NOT_ALLOWED,
        "the error carries the status that triggered the fallback",
    )
}

/// The `GET` goes to the same URL the caller gave.
///
/// The client does not know where the SSE endpoint is — the server names it in
/// the `endpoint` event, which is the whole design. A client that guessed
/// `/sse` would work against every server that happens to use that name and
/// against none that does not.
#[tokio::test]
async fn the_fallback_gets_the_url_the_caller_gave() -> Result<(), TestError> {
    // An empty body, so the GET happens and produces no transport. What is
    // under test is the URL it went to, not what came back.
    let server = ScriptedServer::start(Vec::new()).await?
        .with_post_status(StatusCode::METHOD_NOT_ALLOWED, "text/plain", "no")
        .with_get_status(StatusCode::OK, "text/event-stream");

    let _ = detect(
        DetectConfig::new(server.url())?,
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    )
    .await;

    require_eq(
        &server.gets(),
        &1,
        "exactly one GET, against the URL the caller supplied",
    )?;
    require_eq(
        &server.last_get_path().as_str(),
        &"/probe",
        "and it is the path in that URL, not a convention the client supplied",
    )
}

// ---------------------------------------------------------------------------
// A server that answers whatever the test tells it to.
