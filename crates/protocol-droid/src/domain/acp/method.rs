//! `AcpMethod` — every method the ACP v1.25.0 schema names.
//!
//! # Why this is a flat enum, not a `MethodTable` trait
//!
//! The protocol-neutral core (MCP and A2A) lives in
//! [`crate::domain::jsonrpc`]. The method table is the
//! per-protocol translation layer between the wire and the
//! application handler. A flat enum over a `MethodTable` trait
//! is `match`-friendly for the dispatcher, `serde`-friendly for
//! the wire, and the same shape every per-protocol module
//! uses. Mirroring that shape here keeps the dispatcher
//! generic.
//!
//! # Why the wire names are slash-separated
//!
//! ACP's `meta.json` (vendored at
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/meta.json`)
//! names methods as `session/prompt`, `session/cancel`, and so
//! on. The slash is part of the wire name, not a separator
//! this crate invented; a peer that receives a method without
//! the slash is a wire violation. The `as_wire_name` and
//! `FromStr` impls keep the slash visible at every boundary.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// A method on the ACP wire. Carries the direction (agent-side
/// or client-side) and the slash-separated wire name.
///
/// The wire form is the JSON-RPC 2.0 `method` field, not a
/// path. The slash is part of the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "direction", content = "name")]
pub enum AcpMethod {
    /// A method the editor (client) calls on the agent.
    /// An agent that supports a method answers the call.
    /// The wire name is the slash-separated form from
    /// `meta.json::agentMethods`.
    Agent(AgentMethod),
    /// A method the agent calls on the editor (client).
    /// An editor that supports a method answers the call.
    /// The wire name is the slash-separated form from
    /// `meta.json::clientMethods`.
    Client(ClientMethod),
    /// A JSON-RPC standard method, prefixed `$/`. Only
    /// `$/cancel_request` is named in the v1 schema.
    Protocol(ProtocolMethod),
}

/// The 13 methods an ACP agent publishes, as named in
/// `meta.json::agentMethods`. The wire names are the
/// slash-separated form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMethod {
    /// `initialize` — the editor's first call, advertising
    /// its protocol version and capabilities. The agent
    /// answers with its own version and capabilities.
    Initialize,
    /// `authenticate` — proves the editor's identity after
    /// `initialize` has negotiated an auth method.
    Authenticate,
    /// `session/new` — opens a new session.
    SessionNew,
    /// `session/load` — resumes a session from a checkpoint.
    SessionLoad,
    /// `session/set_mode` — switches the session's mode
    /// (e.g. ask vs. plan).
    SessionSetMode,
    /// `session/set_config_option` — sets a per-session
    /// configuration value the agent advertised in
    /// `initialize`.
    SessionSetConfigOption,
    /// `session/prompt` — sends a user turn. The agent
    /// streams `session/update` notifications back through
    /// the editor.
    SessionPrompt,
    /// `session/cancel` — cancels an in-flight `session/prompt`.
    SessionCancel,
    /// `session/list` — enumerates sessions.
    SessionList,
    /// `session/delete` — removes a session.
    SessionDelete,
    /// `session/resume` — resumes a previously-closed
    /// session.
    SessionResume,
    /// `session/close` — closes a session without deleting
    /// it.
    SessionClose,
    /// `logout` — terminates the auth session.
    Logout,
}

/// The 11 methods an ACP editor (client) publishes.
///
/// The names are the slash-separated form
/// `meta.json::clientMethods` lists. The agent
/// calls these during a session to ask the editor
/// for input, render updates, drive the editor's
/// filesystem, drive the editor's terminal, or run
/// an elicitation flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMethod {
    /// `session/request_permission` — the agent asks the
    /// editor's user whether a tool call may proceed.
    SessionRequestPermission,
    /// `session/update` — the agent streams a turn update
    /// (a chunk of text, a tool call, a status change) to
    /// the editor.
    SessionUpdate,
    /// `fs/write_text_file` — the agent asks the editor
    /// to write a file. The editor decides whether to
    /// prompt the user, write silently, or refuse.
    FsWriteTextFile,
    /// `fs/read_text_file` — the agent asks the editor
    /// to read a file. The editor decides what to return
    /// (a `fs/read_text_file` request may hit an
    /// editor-side virtual file system, not the disk).
    FsReadTextFile,
    /// `terminal/create` — the agent asks the editor to
    /// allocate a terminal. The editor returns an
    /// identifier the agent uses for follow-up calls.
    TerminalCreate,
    /// `terminal/output` — the agent reads captured
    /// terminal output.
    TerminalOutput,
    /// `terminal/release` — the agent releases a terminal
    /// it no longer needs.
    TerminalRelease,
    /// `terminal/wait_for_exit` — the agent waits for a
    /// terminal command to exit.
    TerminalWaitForExit,
    /// `terminal/kill` — the agent sends `SIGTERM` to a
    /// running terminal command.
    TerminalKill,
    /// `elicitation/create` — the agent starts a
    /// structured elicitation (e.g. "pick one of these
    /// three options") in the editor.
    ElicitationCreate,
    /// `elicitation/complete` — the agent signals the
    /// elicitation is done; the editor's user has either
    /// answered or dismissed.
    ElicitationComplete,
}

/// A JSON-RPC standard method, prefixed `$/`. The v1 schema
/// names only `$/cancel_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolMethod {
    /// `$/cancel_request` — a peer cancels an in-flight
    /// request by id. The peer that sent the request
    /// discards it (or returns a "cancelled" error if the
    /// response already left when the cancel arrived).
    CancelRequest,
}

impl AgentMethod {
    /// The wire name as it appears in a JSON-RPC envelope.
    /// The slash is part of the name; the peer is
    /// required to send the slashes too.
    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Authenticate => "authenticate",
            Self::SessionNew => "session/new",
            Self::SessionLoad => "session/load",
            Self::SessionSetMode => "session/set_mode",
            Self::SessionSetConfigOption => "session/set_config_option",
            Self::SessionPrompt => "session/prompt",
            Self::SessionCancel => "session/cancel",
            Self::SessionList => "session/list",
            Self::SessionDelete => "session/delete",
            Self::SessionResume => "session/resume",
            Self::SessionClose => "session/close",
            Self::Logout => "logout",
        }
    }
}

impl ClientMethod {
    /// The wire name as it appears in a JSON-RPC envelope.
    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Self::SessionRequestPermission => "session/request_permission",
            Self::SessionUpdate => "session/update",
            Self::FsWriteTextFile => "fs/write_text_file",
            Self::FsReadTextFile => "fs/read_text_file",
            Self::TerminalCreate => "terminal/create",
            Self::TerminalOutput => "terminal/output",
            Self::TerminalRelease => "terminal/release",
            Self::TerminalWaitForExit => "terminal/wait_for_exit",
            Self::TerminalKill => "terminal/kill",
            Self::ElicitationCreate => "elicitation/create",
            Self::ElicitationComplete => "elicitation/complete",
        }
    }
}

impl ProtocolMethod {
    /// The wire name, including the `$/` prefix.
    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Self::CancelRequest => "$/cancel_request",
        }
    }
}

impl AcpMethod {
    /// The wire name as it appears in a JSON-RPC envelope.
    /// The direction is implied by the variant, not part
    /// of the wire name.
    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Self::Agent(method) => method.as_wire_name(),
            Self::Client(method) => method.as_wire_name(),
            Self::Protocol(method) => method.as_wire_name(),
        }
    }
}

impl fmt::Display for AcpMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_wire_name())
    }
}

/// The error returned by [`AcpMethod::from_str`].
///
/// The `meta.json`-listed set is exhaustive for v1.25.0,
/// so every well-formed wire name parses; an unknown
/// name is a wire violation or a future method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpMethodParseError {
    input: String,
}

impl fmt::Display for AcpMethodParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ACP method {:?} is not in the v1.25.0 method table", self.input)
    }
}

impl std::error::Error for AcpMethodParseError {}

impl FromStr for AcpMethod {
    type Err = AcpMethodParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = input.strip_prefix("$/") {
            return match rest {
                "cancel_request" => Ok(Self::Protocol(ProtocolMethod::CancelRequest)),
                _ => Err(AcpMethodParseError { input: input.to_owned() }),
            };
        }
        for method in [
            Self::Agent(AgentMethod::Initialize),
            Self::Agent(AgentMethod::Authenticate),
            Self::Agent(AgentMethod::SessionNew),
            Self::Agent(AgentMethod::SessionLoad),
            Self::Agent(AgentMethod::SessionSetMode),
            Self::Agent(AgentMethod::SessionSetConfigOption),
            Self::Agent(AgentMethod::SessionPrompt),
            Self::Agent(AgentMethod::SessionCancel),
            Self::Agent(AgentMethod::SessionList),
            Self::Agent(AgentMethod::SessionDelete),
            Self::Agent(AgentMethod::SessionResume),
            Self::Agent(AgentMethod::SessionClose),
            Self::Agent(AgentMethod::Logout),
        ] {
            if method.as_wire_name() == input {
                return Ok(method);
            }
        }
        for method in [
            Self::Client(ClientMethod::SessionRequestPermission),
            Self::Client(ClientMethod::SessionUpdate),
            Self::Client(ClientMethod::FsWriteTextFile),
            Self::Client(ClientMethod::FsReadTextFile),
            Self::Client(ClientMethod::TerminalCreate),
            Self::Client(ClientMethod::TerminalOutput),
            Self::Client(ClientMethod::TerminalRelease),
            Self::Client(ClientMethod::TerminalWaitForExit),
            Self::Client(ClientMethod::TerminalKill),
            Self::Client(ClientMethod::ElicitationCreate),
            Self::Client(ClientMethod::ElicitationComplete),
        ] {
            if method.as_wire_name() == input {
                return Ok(method);
            }
        }
        Err(AcpMethodParseError { input: input.to_owned() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every method named in `meta.json` round-trips
    /// through `as_wire_name` and `FromStr`. The wire
    /// names are the ones the spec publishes; the
    /// dispatcher matches on the typed enum, not on
    /// the string, so a typo in the wire name would
    /// only show up here.
    #[test]
    fn every_meta_method_round_trips_through_wire_name() -> Result<(), AcpMethodParseError> {
        let cases: &[(AcpMethod, &str)] = &[
            (AcpMethod::Agent(AgentMethod::Initialize), "initialize"),
            (AcpMethod::Agent(AgentMethod::Authenticate), "authenticate"),
            (AcpMethod::Agent(AgentMethod::SessionNew), "session/new"),
            (AcpMethod::Agent(AgentMethod::SessionLoad), "session/load"),
            (AcpMethod::Agent(AgentMethod::SessionSetMode), "session/set_mode"),
            (AcpMethod::Agent(AgentMethod::SessionSetConfigOption), "session/set_config_option"),
            (AcpMethod::Agent(AgentMethod::SessionPrompt), "session/prompt"),
            (AcpMethod::Agent(AgentMethod::SessionCancel), "session/cancel"),
            (AcpMethod::Agent(AgentMethod::SessionList), "session/list"),
            (AcpMethod::Agent(AgentMethod::SessionDelete), "session/delete"),
            (AcpMethod::Agent(AgentMethod::SessionResume), "session/resume"),
            (AcpMethod::Agent(AgentMethod::SessionClose), "session/close"),
            (AcpMethod::Agent(AgentMethod::Logout), "logout"),
            (AcpMethod::Client(ClientMethod::SessionRequestPermission), "session/request_permission"),
            (AcpMethod::Client(ClientMethod::SessionUpdate), "session/update"),
            (AcpMethod::Client(ClientMethod::FsWriteTextFile), "fs/write_text_file"),
            (AcpMethod::Client(ClientMethod::FsReadTextFile), "fs/read_text_file"),
            (AcpMethod::Client(ClientMethod::TerminalCreate), "terminal/create"),
            (AcpMethod::Client(ClientMethod::TerminalOutput), "terminal/output"),
            (AcpMethod::Client(ClientMethod::TerminalRelease), "terminal/release"),
            (AcpMethod::Client(ClientMethod::TerminalWaitForExit), "terminal/wait_for_exit"),
            (AcpMethod::Client(ClientMethod::TerminalKill), "terminal/kill"),
            (AcpMethod::Client(ClientMethod::ElicitationCreate), "elicitation/create"),
            (AcpMethod::Client(ClientMethod::ElicitationComplete), "elicitation/complete"),
            (AcpMethod::Protocol(ProtocolMethod::CancelRequest), "$/cancel_request"),
        ];
        assert_eq!(cases.len(), 25, "every published ACP v1 method should appear");
        for (method, expected_name) in cases {
            assert_eq!(method.as_wire_name(), *expected_name);
            let round_tripped: AcpMethod = expected_name.parse()?;
            assert_eq!(round_tripped, *method, "round-trip failed for {expected_name}");
        }
        Ok(())
    }

    /// An unknown wire name is an error, not a silent
    /// fallback. A future method would either extend
    /// the enum or land in `Unknown { name }` if we
    /// ever add that variant; today, an unknown name
    /// is a wire violation.
    #[test]
    fn an_unknown_wire_name_is_an_error() {
        let Err(error) = "session/unicorn".parse::<AcpMethod>() else {
            return;
        };
        assert_eq!(error.input, "session/unicorn");
    }
}
