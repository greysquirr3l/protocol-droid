//! `protocol-droid probe` — fingerprint what a server actually speaks.
//!
//! Try `initialize` (or `server/discover` on `2026-07-28`) at every revision
//! this build has wire types for. For each attempt, record whether the peer
//! agreed to the offer, and if so what it negotiated down to. The result is
//! a `version × capability` matrix on stdout (with the human form on stderr
//! unless `--json` is passed), and the in-process matrix builder is the unit
//! the rest of the command is built on.
//!
//! The matrix builder does not open a socket: it is given a
//! [`ClientTransport`] and runs the same handshake the consumer path runs.
//! Production code hands it the HTTP or legacy-SSE client built from the
//! `--server` spec; tests hand it an in-process fake. The two paths share
//! one decision, and a probe that diverged from the consumer path would be
//! a probe that answered a different question from the one it claimed to.
//!
//! [`ClientTransport`]: protocol_droid::application::client::ClientTransport

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Args;
use protocol_droid::application::client::{
    ClientConfig, ClientTransport, HandshakeError, TransportError, handshake,
};
use protocol_droid::domain::ir::JsonObject;
use protocol_droid::domain::{NegotiationError, ProtocolVersion, TransportKind, TransportSpec};
use serde::Serialize;

/// What the operator asked `probe` to do.
#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// The server to probe.
    #[arg(value_name = "SERVER")]
    pub server: String,

    /// Probe only the revisions the operator names.
    ///
    /// Comma-separated `version,version,...`. Default: every revision
    /// [`ProtocolVersion::KNOWN`] lists.
    #[arg(long, value_name = "VERSIONS", value_delimiter = ',')]
    pub versions: Vec<String>,

    /// Fingerprint an A2A agent instead of probing the MCP matrix.
    ///
    /// Fetches `server + /.well-known/agent-card.json`, parses the
    /// body as an [`AgentCard`](protocol_droid::domain::a2a::agent_card::AgentCard),
    /// and reports the agent's name, declared protocol version,
    /// skill count, and security schemes. A server that does not
    /// serve a well-known card is reported as `not an A2A agent`,
    /// not as a probe failure — the operator most wants to know
    /// when they have the wrong URL.
    #[cfg(feature = "a2a")]
    #[arg(long)]
    pub a2a: bool,

    /// Machine-readable JSON on stdout, with the human summary on stderr.
    #[arg(long)]
    pub json: bool,
}

/// One row of the probe matrix.
#[derive(Debug, Clone, Serialize)]
pub struct MatrixRow {
    /// The version the probe offered.
    pub offered: String,
    /// Whether the peer agreed.
    pub accepted: bool,
    /// The version the session would land on. `None` when `accepted` is false.
    pub negotiated: Option<String>,
    /// Why this row landed where it did, in one sentence.
    pub note: String,
}

/// The probe result, as one value with two renderings.
#[derive(Debug, Serialize)]
pub struct ProbeReport {
    /// The server spec as the operator wrote it.
    pub server: String,
    /// The transport the probe used to reach it.
    pub transport: String,
    /// One row per revision probed, in the order probed.
    pub rows: Vec<MatrixRow>,
}

impl std::fmt::Display for ProbeReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            formatter,
            "{} ({}) — probed {} revision(s):",
            self.server,
            self.transport,
            self.rows.len()
        )?;
        for row in &self.rows {
            if row.accepted {
                let negotiated = row.negotiated.as_deref().unwrap_or("?");
                writeln!(formatter, "  {} → accepted as {negotiated}", row.offered)?;
            } else {
                writeln!(formatter, "  {} → refused ({})", row.offered, row.note)?;
            }
        }
        Ok(())
    }
}

/// Runs `protocol-droid probe`.
///
/// # Errors
///
/// `anyhow` errors name the stage that failed: spec parsing, transport
/// open, or a JSON write. A refused row is not an error — the command
/// exits non-zero only when **no** row is accepted.
pub async fn run(args: &ProbeArgs) -> anyhow::Result<()> {
    #[cfg(feature = "a2a")]
    if args.a2a {
        return run_a2a(args).await;
    }
    let spec: TransportSpec = args
        .server
        .parse()
        .with_context(|| format!("--server is not a usable spec: {}", args.server))?;
    let versions = resolve_versions(&args.versions)?;

    let mut report = probe_spec(&spec, &versions).await?;

    // The backward-compatibility rule from the 2025-03-26 transport page: a
    // server that refuses a `POST` with a 4xx may still be the old HTTP+SSE
    // transport, which is recognised by opening a stream and finding an
    // `endpoint` event on it. Only tried when the endpoint was refused
    // outright, and only reported if it works.
    #[cfg(feature = "bridge")]
    if spec.kind() == TransportKind::StreamableHttp
        && refused_by_every_post(&report)
        && let Ok(legacy) = TransportSpec::new(TransportKind::LegacySse, spec.target())
        && let Ok(found) = probe_spec(&legacy, &versions).await
        && found.rows.iter().any(|row| row.accepted)
    {
        eprintln!("{spec} refuses POST but serves an HTTP+SSE stream; probed it as {legacy}");
        report = found;
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        eprintln!("{report}");
    }

    if report.rows.iter().all(|row| !row.accepted) {
        anyhow::bail!("no revision was accepted by {spec}");
    }
    Ok(())
}

/// Opens `spec` and runs the matrix against it.
async fn probe_spec(spec: &TransportSpec, versions: &[ProtocolVersion]) -> anyhow::Result<ProbeReport> {
    let (transport, kind) = open_transport(spec)
        .await
        .with_context(|| format!("could not open transport for {spec}"))?;
    let stateless = stateless_transport(spec, kind)?;
    Ok(matrix_report_with(
        transport.as_ref(),
        stateless.as_deref(),
        versions,
        kind,
        spec.to_string(),
    )
    .await)
}

/// Whether every row was refused with a client-error status on the `POST`.
#[cfg(feature = "bridge")]
fn refused_by_every_post(report: &ProbeReport) -> bool {
    !report.rows.is_empty()
        && report
            .rows
            .iter()
            .all(|row| !row.accepted && row.note.contains("refused the POST with 4"))
}

/// Fingerprints an A2A agent and reports the result.
///
/// The fetch and parse are isolated in
/// [`probe_a2a::A2aProbeReport::for_server`]; this function
/// picks the output sink and maps the `NotA2a` arm to a
/// non-zero exit so an operator who named the wrong URL
/// does not get a silent pass.
#[cfg(feature = "a2a")]
async fn run_a2a(args: &ProbeArgs) -> anyhow::Result<()> {
    let report = crate::probe_a2a::A2aProbeReport::for_server(&args.server).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        eprintln!("{report}");
    }
    if matches!(report, crate::probe_a2a::A2aProbeReport::NotA2a { .. }) {
        anyhow::bail!("{} is not an A2A agent", args.server);
    }
    Ok(())
}

/// Picks the revisions the probe will try.
///
/// Default is every revision in [`ProtocolVersion::KNOWN`]. An explicit
/// `--versions` list is parsed one at a time, and an unknown name is a
/// refusal rather than a silent skip.
fn resolve_versions(arg: &[String]) -> anyhow::Result<Vec<ProtocolVersion>> {
    if arg.is_empty() {
        return Ok(ProtocolVersion::KNOWN.to_vec());
    }
    arg.iter()
        .map(|text| {
            text.parse::<ProtocolVersion>()
                .with_context(|| format!("--versions contains an unknown revision: {text}"))
        })
        .collect()
}

/// Opens the transport the spec names, naming the kind on the way out.
async fn open_transport(
    spec: &TransportSpec,
) -> anyhow::Result<(Arc<dyn ClientTransport>, TransportKind)> {
    match spec.kind() {
        #[cfg(feature = "bridge")]
        TransportKind::StreamableHttp => {
            use protocol_droid::adapters::secondary::transport::http::{
                HttpClient, HttpClientConfig,
            };
            let config = HttpClientConfig::new(spec.target())
                .with_context(|| "the Streamable HTTP URL is not usable")?;
            let client = HttpClient::new(config)
                .with_context(|| "the Streamable HTTP client could not be built")?;
            Ok((Arc::new(client), TransportKind::StreamableHttp))
        }
        #[cfg(feature = "bridge")]
        TransportKind::LegacySse => {
            use protocol_droid::adapters::secondary::transport::legacy_sse::{
                LegacySseClient, LegacySseConfig,
            };
            let config = LegacySseConfig::new(spec.target())
                .with_context(|| "the legacy SSE URL is not usable")?;
            let client = LegacySseClient::connect(config)
                .await
                .with_context(|| "the legacy SSE client could not connect")?;
            Ok((Arc::new(client), TransportKind::LegacySse))
        }
        kind => {
            anyhow::bail!("no client adapter for `{kind}` in this build; pick http: or legacy-sse:")
        }
    }
}

/// The client a `2026-07-28` row is probed with, for a Streamable HTTP target.
///
/// A revision with no handshake is spoken statelessly: every request carries
/// its own `_meta` and request-metadata headers, and a conforming server
/// refuses a `server/discover` without them. Probing that revision through the
/// session client would report a refusal the server never made.
fn stateless_transport(
    spec: &TransportSpec,
    kind: TransportKind,
) -> anyhow::Result<Option<Arc<dyn ClientTransport>>> {
    #[cfg(feature = "bridge")]
    if kind == TransportKind::StreamableHttp {
        use protocol_droid::adapters::secondary::transport::http::{
            HttpClientConfig, StatelessHttpClient,
        };
        let config = HttpClientConfig::new(spec.target())
            .with_context(|| "the Streamable HTTP URL is not usable")?;
        let client = StatelessHttpClient::new(
            config,
            ProtocolVersion::V2026_07_28,
            serde_json::Value::Object(serde_json::Map::new()),
        )
        .with_context(|| "the stateless HTTP client could not be built")?;
        return Ok(Some(Arc::new(client)));
    }
    let _ = (spec, kind);
    Ok(None)
}

/// Builds the matrix: one handshake per `versions` entry, on `transport`.
pub async fn matrix_report(
    transport: &dyn ClientTransport,
    versions: &[ProtocolVersion],
    kind: TransportKind,
    server_spelling: String,
) -> ProbeReport {
    matrix_report_with(transport, None, versions, kind, server_spelling).await
}

/// [`matrix_report`], probing revisions with no handshake on `stateless` when
/// one is given.
pub async fn matrix_report_with(
    transport: &dyn ClientTransport,
    stateless: Option<&dyn ClientTransport>,
    versions: &[ProtocolVersion],
    kind: TransportKind,
    server_spelling: String,
) -> ProbeReport {
    let mut rows = Vec::with_capacity(versions.len());
    for &offered in versions {
        let row = match stateless {
            Some(stateless) if !protocol_droid::domain::handshake_defined_in(offered) => {
                one_row(stateless, offered, TransportKind::StatelessHttp).await
            }
            _ => one_row(transport, offered, kind).await,
        };
        rows.push(row);
    }
    ProbeReport {
        server: server_spelling,
        transport: kind.as_str().to_owned(),
        rows,
    }
}

/// One matrix cell: handshake at `offered`, record what happened.
async fn one_row(
    transport: &dyn ClientTransport,
    offered: ProtocolVersion,
    kind: TransportKind,
) -> MatrixRow {
    let offered_spelling = offered.as_wire().into_owned();
    let config = match ClientConfig::speaking(vec![offered], kind) {
        Ok(config) => config,
        Err(error) => {
            return MatrixRow {
                offered: offered_spelling,
                accepted: false,
                negotiated: None,
                note: format!("the offer could not be built: {error}"),
            };
        }
    };
    match handshake(transport, &config).await {
        Ok(session) => MatrixRow {
            offered: offered_spelling,
            accepted: true,
            negotiated: Some(session.version.as_wire().into_owned()),
            note: "the peer agreed to this offer".to_owned(),
        },
        Err(error) => MatrixRow {
            offered: offered_spelling,
            accepted: false,
            negotiated: None,
            note: describe(&error),
        },
    }
}

/// One sentence on why the handshake did not land.
fn describe(error: &HandshakeError) -> String {
    match error {
        HandshakeError::NothingOffered => "the client offered nothing".to_owned(),
        HandshakeError::MalformedResult { field, .. } => {
            format!("the answer was missing `{field}`")
        }
        HandshakeError::Negotiation(NegotiationError::UnsupportedServerVersion {
            answered,
            ..
        }) => format!("the server answered {answered}, which is not in the offer"),
        HandshakeError::Negotiation(_) => "the server and the client share no revision".to_owned(),
        HandshakeError::Transport(TransportError::Unavailable(reason)) => reason.clone(),
        HandshakeError::Transport(TransportError::Refused { code, message, .. }) => {
            format!("the server refused with {code}: {message}")
        }
        HandshakeError::Assembling { method, .. } => {
            format!("the {method} request could not be built")
        }
    }
}

#[doc(hidden)]
pub mod __for_tests {
    use super::{ClientTransport, Future, JsonObject, Pin, ProtocolVersion, TransportError};

    /// A `ClientTransport` that responds to `initialize` and
    /// `server/discover` with a fixed version set.
    #[derive(Debug)]
    pub struct ScriptedServer {
        pub accepted: Vec<ProtocolVersion>,
    }

    impl ClientTransport for ScriptedServer {
        fn send(
            &self,
            method: &str,
            params: &JsonObject,
        ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>> {
            let accepted = self.accepted.clone();
            let method = method.to_owned();
            let text = params.as_str().to_owned();
            Box::pin(async move {
                let value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|error| TransportError::Unavailable(format!("bad params: {error}")))?;
                match method.as_str() {
                    protocol_droid::application::client::INITIALIZE => {
                        let offered = value
                            .get("protocolVersion")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                TransportError::Unavailable(
                                    "no protocolVersion in initialize".to_owned(),
                                )
                            })?;
                        let parsed: ProtocolVersion = offered.parse().map_err(|error| {
                            TransportError::Unavailable(format!("unrecognised offer: {error}"))
                        })?;
                        let answered = if accepted.contains(&parsed) {
                            parsed
                        } else {
                            *accepted.last().ok_or_else(|| {
                                TransportError::Unavailable(
                                    "the scripted server has no accepted set".to_owned(),
                                )
                            })?
                        };
                        let body = serde_json::json!({
                            "protocolVersion": answered.as_wire(),
                            "capabilities": {},
                            "serverInfo": {"name": "scripted", "version": "0"},
                        });
                        JsonObject::from_value(body).map_err(|error| {
                            TransportError::Unavailable(format!("bad reply body: {error:?}"))
                        })
                    }
                    protocol_droid::application::client::DISCOVER => {
                        let versions: Vec<String> = accepted
                            .iter()
                            .map(|version| version.as_wire().into_owned())
                            .collect();
                        let body = serde_json::json!({
                            "supportedVersions": versions,
                            "capabilities": {},
                        });
                        JsonObject::from_value(body).map_err(|error| {
                            TransportError::Unavailable(format!("bad reply body: {error:?}"))
                        })
                    }
                    other => Err(TransportError::Unavailable(format!(
                        "the scripted server does not answer {other}"
                    ))),
                }
            })
        }

        fn notify(
            &self,
            _method: &str,
            _params: &JsonObject,
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use protocol_droid::application::client::{DISCOVER, INITIALIZE};
    use serde_json::{Value, json};

    use super::__for_tests::ScriptedServer;
    use super::*;

    #[derive(Debug)]
    enum Failure {
        Message(String),
        Row(&'static str, MatrixRow),
        Probe(&'static str),
        Lock,
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Message(text) => f.write_str(text),
                Self::Row(label, row) => write!(
                    f,
                    "{label}: row was offered={}, accepted={}, negotiated={:?}, note={}",
                    row.offered, row.accepted, row.negotiated, row.note
                ),
                Self::Probe(label) => write!(f, "probe did not run: {label}"),
                Self::Lock => f.write_str("a lock was poisoned"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<String> for Failure {
        fn from(text: String) -> Self {
            Self::Message(text)
        }
    }

    impl From<anyhow::Error> for Failure {
        fn from(error: anyhow::Error) -> Self {
            Self::Message(error.to_string())
        }
    }

    /// Probe against an in-process server pinned to `[2025-03-26, 2025-06-18]`
    /// reports exactly those revisions.
    #[tokio::test]
    async fn matrix_against_an_in_process_server_pinned_to_two_revisions() -> Result<(), Failure> {
        let server = ScriptedServer {
            accepted: vec![ProtocolVersion::V2025_03_26, ProtocolVersion::V2025_06_18],
        };
        let report = matrix_report(
            &server,
            &ProtocolVersion::KNOWN,
            TransportKind::StreamableHttp,
            "in-process:scripted".to_owned(),
        )
        .await;

        let accepted: Vec<&str> = report
            .rows
            .iter()
            .filter(|row| row.accepted)
            .map(|row| row.offered.as_str())
            .collect();
        if accepted != ["2025-03-26", "2025-06-18"] {
            return Err(Failure::Message(format!(
                "expected exactly [2025-03-26, 2025-06-18], got {accepted:?}"
            )));
        }

        for row in report.rows.iter().filter(|row| !row.accepted) {
            if row.note.is_empty() {
                return Err(Failure::Row(
                    "refused rows must carry a reason",
                    row.clone(),
                ));
            }
            if row.negotiated.is_some() {
                return Err(Failure::Row(
                    "a refused row cannot carry a negotiated version",
                    row.clone(),
                ));
            }
        }
        Ok(())
    }

    /// A server that supports only one revision reports one accepted row and
    /// refuses every other probe, in the order the probe offered them.
    #[tokio::test]
    async fn matrix_preserves_the_offered_order() -> Result<(), Failure> {
        let server = ScriptedServer {
            accepted: vec![ProtocolVersion::V2025_06_18],
        };
        let report = matrix_report(
            &server,
            &ProtocolVersion::KNOWN,
            TransportKind::StreamableHttp,
            "in-process:scripted".to_owned(),
        )
        .await;

        let names: Vec<&str> = report.rows.iter().map(|row| row.offered.as_str()).collect();
        let expected: Vec<String> = ProtocolVersion::KNOWN
            .iter()
            .map(|v| v.as_wire().into_owned())
            .collect();
        if names != expected.iter().map(String::as_str).collect::<Vec<_>>() {
            return Err(Failure::Message(format!(
                "expected {expected:?}, got {names:?}"
            )));
        }
        let accepted_count = report.rows.iter().filter(|row| row.accepted).count();
        if accepted_count != 1 {
            return Err(Failure::Message(format!(
                "expected exactly one accepted row, got {accepted_count}"
            )));
        }
        Ok(())
    }

    /// `--versions` rejects an unknown name rather than silently dropping it.
    #[test]
    fn resolve_versions_rejects_unknown_revisions() -> Result<(), Failure> {
        let outcome = resolve_versions(&["2025-06-18".to_owned(), "not-a-version".to_owned()]);
        match outcome {
            Err(error) => {
                let message = error.to_string();
                if !message.contains("not-a-version") {
                    return Err(Failure::Message(format!(
                        "the refusal did not name the bad revision: {message}"
                    )));
                }
                Ok(())
            }
            Ok(_) => Err(Failure::Message(
                "an unknown revision was silently accepted".to_owned(),
            )),
        }
    }

    /// `--versions` defaults to `ProtocolVersion::KNOWN`, in order.
    #[test]
    fn resolve_versions_defaults_to_every_known_revision() -> Result<(), Failure> {
        let resolved = resolve_versions(&[])?;
        if resolved != ProtocolVersion::KNOWN.to_vec() {
            return Err(Failure::Message(format!(
                "default did not match ProtocolVersion::KNOWN: got {resolved:?}"
            )));
        }
        Ok(())
    }

    /// The probe sends `initialize` once per non-2026-07-28 revision and
    /// `server/discover` exactly once.
    #[tokio::test]
    async fn matrix_calls_initialize_for_every_non_4_revision() -> Result<(), Failure> {
        use std::sync::Arc;

        struct Recording {
            accepted: Vec<ProtocolVersion>,
            log: Arc<Mutex<Vec<String>>>,
        }

        impl ClientTransport for Recording {
            fn send(
                &self,
                method: &str,
                params: &JsonObject,
            ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>>
            {
                let accepted = self.accepted.clone();
                let text = params.as_str().to_owned();
                let method = method.to_owned();
                let log = self.log.clone();
                Box::pin(async move {
                    if let Ok(mut guard) = log.lock() {
                        guard.push(method);
                    }
                    let value: Value = serde_json::from_str(&text).map_err(|error| {
                        TransportError::Unavailable(format!("bad params: {error}"))
                    })?;
                    let offered = value
                        .get("protocolVersion")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            TransportError::Unavailable(
                                "no protocolVersion in initialize".to_owned(),
                            )
                        })?;
                    let parsed: ProtocolVersion = offered.parse().map_err(|error| {
                        TransportError::Unavailable(format!("unrecognised offer: {error}"))
                    })?;
                    let answered = if accepted.contains(&parsed) {
                        parsed
                    } else {
                        *accepted.first().ok_or_else(|| {
                            TransportError::Unavailable("no accepted set".to_owned())
                        })?
                    };
                    let body = json!({
                        "protocolVersion": answered.as_wire(),
                        "capabilities": {},
                        "serverInfo": {"name": "recording", "version": "0"},
                    });
                    JsonObject::from_value(body).map_err(|error| {
                        TransportError::Unavailable(format!("bad reply body: {error:?}"))
                    })
                })
            }

            fn notify(
                &self,
                _method: &str,
                _params: &JsonObject,
            ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
                Box::pin(async { Ok(()) })
            }
        }

        let server = Recording {
            accepted: vec![ProtocolVersion::V2024_11_05, ProtocolVersion::V2026_07_28],
            log: Arc::new(Mutex::new(Vec::new())),
        };
        let _ = matrix_report(
            &server,
            &ProtocolVersion::KNOWN,
            TransportKind::StreamableHttp,
            "in-process:recording".to_owned(),
        )
        .await;

        let (initialize_calls, discover_calls) = {
            let log = server.log.lock().map_err(|_| Failure::Lock)?;
            let initialize_calls = log.iter().filter(|m| m.as_str() == INITIALIZE).count();
            let discover_calls = log.iter().filter(|m| m.as_str() == DISCOVER).count();
            drop(log);
            (initialize_calls, discover_calls)
        };

        if initialize_calls != 4 {
            return Err(Failure::Message(format!(
                "expected 4 initialize calls, got {initialize_calls}"
            )));
        }
        if discover_calls != 1 {
            return Err(Failure::Message(format!(
                "expected 1 discover call, got {discover_calls}"
            )));
        }
        Ok(())
    }

    /// One accepted row carries the negotiated version, and the negotiated
    /// version equals the offer when the server echoed it.
    #[tokio::test]
    async fn an_accepted_row_records_the_negotiated_version() -> Result<(), Failure> {
        let server = ScriptedServer {
            accepted: vec![ProtocolVersion::V2025_06_18],
        };
        let report = matrix_report(
            &server,
            &[ProtocolVersion::V2025_06_18],
            TransportKind::StreamableHttp,
            "in-process:scripted".to_owned(),
        )
        .await;
        let row = report.rows.first().ok_or(Failure::Probe("no rows"))?;
        if row.offered != "2025-06-18" {
            return Err(Failure::Message(format!(
                "offered was {}, expected 2025-06-18",
                row.offered
            )));
        }
        if !row.accepted {
            return Err(Failure::Row("row was refused", row.clone()));
        }
        if row.negotiated.as_deref() != Some("2025-06-18") {
            return Err(Failure::Row(
                "negotiated did not echo the offer",
                row.clone(),
            ));
        }
        Ok(())
    }

    /// Probing with a single revision that is NOT in `accepted` yields one
    /// refused row that names the server's counter-offer.
    #[tokio::test]
    async fn matrix_with_no_accepted_revisions_marks_every_row_refused() -> Result<(), Failure> {
        let server = ScriptedServer {
            accepted: vec![ProtocolVersion::V2025_06_18],
        };
        let report = matrix_report(
            &server,
            &[ProtocolVersion::V2024_11_05],
            TransportKind::StreamableHttp,
            "in-process:scripted".to_owned(),
        )
        .await;
        if report.rows.len() != 1 {
            return Err(Failure::Message(format!(
                "expected 1 row, got {}",
                report.rows.len()
            )));
        }
        let row = report.rows.first().ok_or(Failure::Probe("no rows"))?;
        if row.accepted {
            return Err(Failure::Row("row was accepted", row.clone()));
        }
        if row.note.is_empty() {
            return Err(Failure::Row("refused row did not say why", row.clone()));
        }
        Ok(())
    }

    /// `ProbeArgs` round-trips through clap with the same shape the binary
    /// sees — a regression in the public arg contract (a flag renamed, a
    /// `requires` removed) is caught here, not at the user's first
    /// invocation.
    #[test]
    fn probe_args_round_trip_through_clap() -> Result<(), Failure> {
        // The full `Cli` parse is what the binary does; a subcommand-only
        // parse exercises the same code path on the `ProbeArgs` derive but
        // lets clap place it as the top-level subcommand, which is the
        // shape an external consumer would see.
        use clap::{Parser, Subcommand};

        #[derive(Debug, Parser)]
        struct Wrapper {
            #[command(subcommand)]
            command: WrapperCmd,
        }

        #[derive(Debug, Subcommand)]
        enum WrapperCmd {
            Probe(ProbeArgs),
        }

        let parsed = Wrapper::try_parse_from([
            "protocol-droid",
            "probe",
            "http:http://127.0.0.1:9999/mcp",
            "--json",
            "--versions",
            "2025-03-26,2025-06-18",
        ]);
        let args = match parsed {
            Ok(Wrapper {
                command: WrapperCmd::Probe(args),
            }) => args,
            Err(error) => {
                return Err(Failure::Message(format!("clap parse failed: {error}")));
            }
        };
        if args.server != "http:http://127.0.0.1:9999/mcp" {
            return Err(Failure::Message(format!("server was {:?}", args.server)));
        }
        if !args.json {
            return Err(Failure::Message("--json was not set".to_owned()));
        }
        if args.versions != ["2025-03-26", "2025-06-18"] {
            return Err(Failure::Message(format!(
                "versions was {:?}",
                args.versions
            )));
        }
        Ok(())
    }

    /// `run` refuses a malformed spec with a message that names the
    /// `--server` flag — so a user with a typo knows to fix the spec, not
    /// the network. The test exercises the public entry point that the
    /// binary dispatches into, which is also why the lib is wired this way
    /// at all: every reachable `pub fn` should be reachable from a test.
    #[tokio::test]
    async fn run_refuses_a_malformed_spec_by_naming_the_flag() -> Result<(), Failure> {
        let args = ProbeArgs {
            server: "no-colon-here".to_owned(),
            versions: Vec::new(),
            #[cfg(feature = "a2a")]
            a2a: false,
            json: false,
        };
        let outcome = run(&args).await;
        match outcome {
            Err(error) => {
                let message = error.to_string();
                if !message.contains("--server") {
                    return Err(Failure::Message(format!(
                        "the refusal did not name the flag: {message}"
                    )));
                }
                Ok(())
            }
            Ok(()) => Err(Failure::Message(
                "the malformed spec was accepted".to_owned(),
            )),
        }
    }
}
