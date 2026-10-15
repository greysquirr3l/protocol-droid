//! `protocol-droid probe` — the A2A leg.
//!
//! Fetches the agent card at
//! `server + /.well-known/agent-card.json`, parses it as
//! [`AgentCard`](protocol_droid::domain::a2a::agent_card::AgentCard),
//! and reports the name, version, declared skills, and
//! security schemes. The output is one of two shapes:
//! the `A2aProbeReport` when the card is reachable, or
//! `A2aProbeReport::NotA2a { reason }` when the URL
//! resolved to something that is not an A2A agent.
//!
//! The card fetch is a `reqwest` GET, deliberately outside
//! the MCP matrix: a probe that ran a `ClientTransport`
//! handshake would impose a JSON-RPC body on the
//! conversation, and an A2A agent that is not also an
//! MCP server would reply with a refusal that has nothing
//! to do with A2A compatibility. The fingerprint is the
//! card; the matrix is the handshake.
//!
//! # Why a separate `report` rather than reusing `ProbeReport`
//!
//! The matrix's `rows: Vec<MatrixRow>` is the shape the
//! operator reads as "revision X was accepted as Y", and
//! the A2A card has no equivalent: there is no negotiation,
//! only what the agent declared. A dedicated report keeps
//! the consumer path (the agent's JSON-RPC method table)
//! and the operator path (the CLI's probe) from sharing a
//! data shape they have no reason to share.

use std::sync::Arc;

use anyhow::Context as _;
use protocol_droid::domain::a2a::agent_card::AgentCard;
use serde::Serialize;

/// What `probe --a2a` returned.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum A2aProbeReport {
    /// The URL resolves to an A2A agent.
    Fingerprint {
        /// The URL the operator named.
        server: String,
        /// The agent's name from the card.
        name: Arc<str>,
        /// The protocol version declared by the card, if any.
        protocol_version: Option<String>,
        /// The number of skills the card declares.
        skill_count: usize,
        /// The names of every security scheme the card declares.
        security_schemes: Vec<String>,
        /// A short human line summarising the agent.
        note: String,
    },
    /// The URL did not resolve to an A2A agent.
    NotA2a {
        /// The URL the operator named.
        server: String,
        /// Why the probe concluded this is not an A2A agent.
        reason: String,
    },
}

impl A2aProbeReport {
    /// Builds the report for `server`, fetching the well-known card.
    ///
    /// # Errors
    ///
    /// `anyhow` errors name the stage that failed: the URL
    /// parse, the card fetch, the JSON-RPC shape check, or a
    /// `serde_json` parse. A refusal to accept the card is
    /// not an error — it becomes the `NotA2a` arm of the
    /// report, because a peer that does not answer the
    /// well-known path is exactly the case the operator
    /// most wants the probe to surface.
    pub async fn for_server(server: &str) -> anyhow::Result<Self> {
        let base = normalise_base(server)?;
        let url = format!("{base}/.well-known/agent-card.json");

        let http = reqwest::Client::builder()
            .user_agent(concat!("protocol-droid/", env!("CARGO_PKG_VERSION")))
            .build()
            .with_context(|| "could not build the A2A probe HTTP client")?;
        let response = http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("could not fetch {url}"))?;
        let status = response.status();
        if !status.is_success() {
            return Ok(Self::NotA2a {
                server: server.to_owned(),
                reason: format!("the well-known card returned {status}"),
            });
        }
        let body = response
            .text()
            .await
            .with_context(|| format!("could not read the body of {url}"))?;
        let card: AgentCard = match serde_json::from_str(&body) {
            Ok(card) => card,
            Err(err) => {
                return Ok(Self::NotA2a {
                    server: server.to_owned(),
                    reason: format!("the well-known body is not an AgentCard: {err}"),
                });
            }
        };

        let security_schemes = collect_security_schemes(&card);
        let skill_count = card.skills.len();
        let name = Arc::clone(&card.name);
        let protocol_version = extract_protocol_version(&card);
        let note = format!(
            "{name} declares {skill_count} skill(s) and {} security scheme(s)",
            security_schemes.len()
        );

        Ok(Self::Fingerprint {
            server: server.to_owned(),
            name,
            protocol_version,
            skill_count,
            security_schemes,
            note,
        })
    }
}

impl std::fmt::Display for A2aProbeReport {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fingerprint {
                name,
                protocol_version,
                skill_count,
                security_schemes,
                note,
                ..
            } => {
                writeln!(formatter, "{name} — {note}")?;
                if let Some(version) = protocol_version {
                    writeln!(formatter, "  protocol version: {version}")?;
                }
                if !security_schemes.is_empty() {
                    writeln!(formatter, "  security: {}", security_schemes.join(", "))?;
                }
                writeln!(formatter, "  skills: {skill_count} declared")?;
            }
            Self::NotA2a { server, reason } => {
                writeln!(formatter, "{server} is not an A2A agent: {reason}")?;
            }
        }
        Ok(())
    }
}

/// Strips a trailing slash and a trailing path the operator
/// might have included (e.g. `https://host/a2a`),
/// returning the origin (`https://host`). A spec that
/// named a path is treated as the agent's root — the
/// well-known card is fetched from the root, not from
/// the operator's path, because the A2A discovery
/// spec puts the card at `root + /.well-known/...`.
///
/// A hand-rolled parser is fine here: the A2A probe
/// only needs the `scheme + host + port` of a URL
/// shaped `http://host[:port][/path]`, and pulling in
/// the `url` crate for a 6-line substring scan is more
/// surface than the rest of the CLI exposes.
fn normalise_base(server: &str) -> anyhow::Result<String> {
    let trimmed = server.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--server is empty");
    }
    let (scheme, rest) = if let Some(suffix) = trimmed.strip_prefix("https://") {
        ("https", suffix)
    } else if let Some(suffix) = trimmed.strip_prefix("http://") {
        ("http", suffix)
    } else {
        anyhow::bail!("--server is not a parseable URL: {server}");
    };
    // The host (and optional port) ends at the first `/`,
    // `?`, or `#`. A trailing `/` is allowed.
    let origin_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = rest[..origin_end].trim_end_matches('/');
    if origin.is_empty() {
        anyhow::bail!("--server has no host component: {server}");
    }
    Ok(format!("{scheme}://{origin}"))
}

/// The A2A spec puts the protocol version on each
/// declared interface (`AgentInterface::protocol_version`),
/// not on the card itself. A probe that asks "what
/// protocol does this agent speak" reads the first
/// interface — the spec says the list is in preference
/// order, so the first is the one a client would
/// negotiate. A card with no interfaces still
/// defines a valid agent, just at no specific revision;
/// the report says so rather than guessing.
fn extract_protocol_version(card: &AgentCard) -> Option<String> {
    let interface = card.supported_interfaces.first()?;
    Some(interface.protocol_version.as_str().to_owned())
}

/// Names the security schemes the card declares.
///
/// The A2A spec lets an agent declare zero or more
/// `securitySchemes`. Listing them by name is enough for
/// the operator to know whether the card expects
/// `Bearer`, `ApiKey`, `OAuth2`, or a custom scheme —
/// the probe does not need to attempt authentication
/// to answer that question.
fn collect_security_schemes(card: &AgentCard) -> Vec<String> {
    let mut names: Vec<String> = card
        .security_schemes
        .keys()
        .map(|name| name.as_ref().to_owned())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use protocol_droid::domain::a2a::agent_card::AgentCard;

    use super::{
        A2aProbeReport, collect_security_schemes, extract_protocol_version, normalise_base,
    };

    type TestError = Box<dyn std::error::Error>;

    /// What one test failure looks like, collapsed into one type so
    /// the production `?` machinery can be used in tests rather than
    /// `.expect("card parses")`, which `clippy::expect_used` denies.
    #[derive(Debug)]
    enum Failure {
        Card(serde_json::Error),
        Other(String),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Card(err) => f.write_str(&format!("card parse: {err}")),
                Self::Other(text) => f.write_str(text),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<serde_json::Error> for Failure {
        fn from(err: serde_json::Error) -> Self {
            Self::Card(err)
        }
    }

    /// A typed `?` for card fixtures that should always parse — the
    /// `From` impl above makes the test a single `parse_fixture()?`
    /// rather than `.expect("...")`, and the failure prints the engine's
    /// account of what it refused, not a `unwrap` panic.
    fn parse_fixture(text: &str) -> Result<AgentCard, Failure> {
        Ok(serde_json::from_str(text)?)
    }

    /// A URL with no path component normalises to the
    /// origin the well-known card is fetched from.
    #[test]
    fn a_bare_origin_is_returned_unchanged() -> Result<(), TestError> {
        let base = normalise_base("http://agent.example")?;
        assert_eq!(base, "http://agent.example");
        Ok(())
    }

    /// A URL with a path strips the path; the well-known
    /// card is always at the origin, not the operator's
    /// path.
    #[test]
    fn a_path_is_stripped_to_the_origin() -> Result<(), TestError> {
        let base = normalise_base("https://host.example/a2a/custom")?;
        assert_eq!(base, "https://host.example");
        Ok(())
    }

    /// A query string and fragment are stripped with the
    /// path. A peer URL that has been copy-pasted from a
    /// browser address bar often carries a session
    /// token or anchor; the probe must not forward
    /// those into the well-known path.
    #[test]
    fn a_query_and_fragment_are_stripped() -> Result<(), TestError> {
        let base = normalise_base("https://host/a2a?token=secret#card")?;
        assert_eq!(base, "https://host");
        Ok(())
    }

    /// A port is preserved when the operator named one.
    #[test]
    fn a_port_is_preserved() -> Result<(), TestError> {
        let base = normalise_base("http://host:9000")?;
        assert_eq!(base, "http://host:9000");
        Ok(())
    }

    /// A non-URL server name is a hard refusal; the
    /// operator gets a clear error, not a silent miss.
    #[test]
    fn a_non_url_is_a_typed_error() -> Result<(), Failure> {
        let err = match normalise_base("not a url") {
            Ok(bad) => {
                return Err(Failure::Other(format!(
                    "normalise_base accepted 'not a url' and returned {bad}"
                )));
            }
            Err(err) => err,
        };
        if err.to_string().contains("not a parseable URL") {
            Ok(())
        } else {
            Err(Failure::Other(format!(
                "the refusal did not mention 'not a parseable URL': {err}"
            )))
        }
    }

    /// The protocol version is taken from the first
    /// declared interface, in preference order. A card
    /// with no interfaces has no protocol version, and
    /// the function returns `None` rather than guessing.
    #[test]
    fn a_card_with_an_interface_reports_its_version() -> Result<(), Failure> {
        let card = parse_fixture(
            r#"{
                "name": "agent",
                "description": "test",
                "version": "0.0.0",
                "supportedInterfaces": [
                    {"url": "https://agent/a2a", "protocolBinding": "jsonrpc", "protocolVersion": "1.0"}
                ],
                "capabilities": {"streaming": false, "pushNotifications": false, "extensions": []},
                "defaultInputModes": ["text/plain"],
                "defaultOutputModes": ["text/plain"],
                "skills": []
            }"#,
        )?;
        let version = extract_protocol_version(&card);
        assert_eq!(version.as_deref(), Some("1.0"));
        Ok(())
    }

    /// A card with no `supportedInterfaces` reports no
    /// version. The function does not invent one.
    #[test]
    fn a_card_without_interfaces_reports_no_version() -> Result<(), Failure> {
        let card = parse_fixture(
            r#"{
                "name": "agent",
                "description": "test",
                "version": "0.0.0",
                "supportedInterfaces": [],
                "capabilities": {"streaming": false, "pushNotifications": false, "extensions": []},
                "defaultInputModes": ["text/plain"],
                "defaultOutputModes": ["text/plain"],
                "skills": []
            }"#,
        )?;
        let version = extract_protocol_version(&card);
        assert!(version.is_none());
        Ok(())
    }

    /// Security-scheme names are sorted so a deterministic
    /// output lets the JSON test stay golden.
    #[test]
    fn security_schemes_are_sorted_for_a_stable_report() -> Result<(), Failure> {
        let card = parse_fixture(
            r#"{
                "name": "agent",
                "description": "test",
                "version": "0.0.0",
                "supportedInterfaces": [
                    {"url": "https://agent/a2a", "protocolBinding": "jsonrpc", "protocolVersion": "1.0"}
                ],
                "capabilities": {"streaming": false, "pushNotifications": false, "extensions": []},
                "defaultInputModes": ["text/plain"],
                "defaultOutputModes": ["text/plain"],
                "skills": [],
                "securitySchemes": {
                    "z-bearer": {"type": "http", "scheme": "bearer"},
                    "a-api-key": {"type": "apiKey", "in": "header", "name": "X-Key"}
                }
            }"#,
        )?;
        let names = collect_security_schemes(&card);
        assert_eq!(names, vec!["a-api-key", "z-bearer"]);
        Ok(())
    }

    /// The `NotA2a` arm renders as a single line so a
    /// non-A2A peer is unmistakable on a terminal.
    #[test]
    fn the_not_a2a_arm_renders_a_single_line() {
        let report = A2aProbeReport::NotA2a {
            server: "http://mcp.example".to_owned(),
            reason: "the well-known card returned 404".to_owned(),
        };
        let rendered = format!("{report}");
        assert!(rendered.contains("mcp.example"));
        assert!(rendered.contains("not an A2A agent"));
    }

    /// The `Fingerprint` arm renders name, version, and
    /// skill count. A consumer (operator or agent loop)
    /// reads the first line to know which agent is at
    /// the URL.
    #[test]
    fn the_fingerprint_arm_renders_name_and_count() {
        let report = A2aProbeReport::Fingerprint {
            server: "http://agent.example".to_owned(),
            name: Arc::from("echo-agent"),
            protocol_version: Some("1.0".to_owned()),
            skill_count: 3,
            security_schemes: vec!["bearer".to_owned()],
            note: "echo-agent declares 3 skill(s) and 1 security scheme(s)".to_owned(),
        };
        let rendered = format!("{report}");
        assert!(rendered.contains("echo-agent"));
        assert!(rendered.contains("skills: 3 declared"));
        assert!(rendered.contains("security: bearer"));
    }
}
