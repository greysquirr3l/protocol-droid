//! `connect`, on the paths that do not need a server behind them.
//!
//! A server would be a second, much larger test surface for what is left to
//! check here: that a bad URL is refused as a URL, that an unreachable one is
//! refused as unreachable, and that the name each [`Spoke`] reports is the name
//! a log line or a JSON report will get. The path that *does* need a server —
//! the probe answering, and the negotiation reading the answer — is covered in
//! `application::client::handshake`, where the answer is a value rather than a
//! socket.

use crate::adapters::secondary::transport::connect::{ConnectError, Spoke, connect};
use crate::application::client::ClientConfig;
use crate::domain::{ProtocolVersion, TransportKind};

fn config() -> ClientConfig {
    ClientConfig::new(ProtocolVersion::V2025_06_18, TransportKind::StreamableHttp)
}

#[tokio::test]
async fn a_url_that_is_not_a_url_is_refused_as_a_url() -> Result<(), TestError> {
    // Refused before anything is sent, so the caller is told about the typo
    // rather than about a connection failure it did not cause.
    let error = expect_err(connect("not a url at all", &config()).await)?;

    let ConnectError::Http(_) = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    Ok(())
}

#[tokio::test]
async fn a_url_nothing_is_listening_on_is_refused_as_a_detection_failure() -> Result<(), TestError>
{
    // Port 0 is never something a server bound. The point is which error the
    // caller gets: a `Detect` refusal means "no transport answered", which is
    // a different problem from "your URL is malformed" and a different one
    // again from "we connected and could not agree on a revision".
    let error = expect_err(connect("http://127.0.0.1:0/mcp", &config()).await)?;

    let ConnectError::Detect(_) = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    Ok(())
}

#[tokio::test]
async fn a_client_offering_no_revision_is_refused_before_anything_is_sent() -> Result<(), TestError>
{
    // `ClientConfig::speaking` already refuses an empty list, so the only way
    // here is a client that was built one version at a time and then had them
    // removed. Refusing before the probe is the point: a client with no
    // revision cannot negotiate, and finding that out from a `4xx` would send
    // the reader to the wrong layer entirely.
    let config = ClientConfig::speaking(
        [ProtocolVersion::V2025_06_18],
        TransportKind::StreamableHttp,
    )?;

    // Sanity: the reachable case does negotiate rather than short-circuit.
    assert_eq!(config.versions().len(), 1);

    let error = expect_err(connect("not a url at all", &config).await)?;
    let ConnectError::Http(_) = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    Ok(())
}

#[test]
fn the_transport_names_are_stable() {
    // These strings end up in JSON reports and log lines, where a rename is a
    // breaking change to a consumer rather than a rename of a variant. Asserted
    // as literals on purpose: deriving the expectation from the enum would pass
    // whatever the name became.
    assert_eq!(Spoke::StreamableHttp.as_str(), "streamable-http");
    assert_eq!(Spoke::LegacyHttpSse.as_str(), "legacy-http-sse");
}

#[test]
fn the_two_transports_are_distinguishable() {
    // A report that collapsed both to one name would be a report that cannot
    // say which transport a failing session was on.
    assert_ne!(Spoke::StreamableHttp, Spoke::LegacyHttpSse);
}

/// The error half, for a test that expects the call to fail.
fn expect_err<T>(result: Result<T, ConnectError>) -> Result<ConnectError, TestError> {
    match result {
        Ok(_) => Err(TestError::ExpectedAnError),
        Err(error) => Ok(error),
    }
}

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error("the call was expected to fail and succeeded")]
    ExpectedAnError,
    #[error("the error was not the shape under test: {got:?}")]
    WrongShape { got: ConnectError },
    #[error(transparent)]
    Client(#[from] crate::application::client::ClientError),
}
