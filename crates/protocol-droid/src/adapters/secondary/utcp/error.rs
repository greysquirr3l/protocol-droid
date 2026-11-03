//! Why a UTCP operation failed.

use crate::application::client::ClientError;
use crate::application::client::ConnectError;
use crate::application::utcp::call::CallError;
use crate::domain::utcp::ManualError;
use crate::domain::utcp::endpoint::EndpointError;

/// Why a UTCP operation failed.
#[derive(Debug, thiserror::Error)]
pub enum UtcpError {
    /// The URL could not be read.
    #[error("`{url}` is not a URL this build can use: {detail}")]
    Url {
        /// The URL as it was given.
        url: String,
        /// What was wrong with it.
        detail: String,
    },
    /// The request itself failed.
    #[error("the request failed: {detail}")]
    Transport {
        /// What the HTTP client said.
        detail: String,
    },
    /// The peer answered with a status that is not a success.
    #[error("the peer answered {status}")]
    Status {
        /// The status code.
        status: u16,
    },
    /// The peer answered with something that is not a document.
    #[error("the peer answered with `{found}`, which is not JSON")]
    MediaType {
        /// The content type the peer sent.
        found: String,
    },
    /// The document is not a manual.
    #[error(transparent)]
    Manual(#[from] ManualError),
    /// The call could not be planned.
    #[error(transparent)]
    Plan(#[from] CallError),
    /// The MCP session refused the call.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// The MCP session could not be opened.
    #[error(transparent)]
    Connect(#[from] ConnectError),
    /// The server could not be spawned.
    #[error("the MCP server could not be spawned: {detail}")]
    Spawn {
        /// What went wrong.
        detail: String,
    },
    /// The token endpoint refused to issue a token.
    #[error("the token endpoint refused: {detail}")]
    Token {
        /// What the token endpoint said.
        detail: String,
    },
    /// The token endpoint is not one this build will talk to.
    #[error("the token endpoint `{url}` is not one this build will talk to: {reason}")]
    TokenEndpoint {
        /// The token endpoint.
        url: String,
        /// Why it was refused.
        reason: String,
    },
    /// This build could not decide which revisions to offer.
    #[error("this build could not decide which protocol revisions to offer: {detail}")]
    Versions {
        /// What went wrong.
        detail: String,
    },
    /// The transport specification could not be read.
    #[error("`{spec}` is not a transport this build speaks: {detail}")]
    Spec {
        /// The specification as it was given.
        spec: String,
        /// What was wrong with it.
        detail: String,
    },
    /// The URL is not one this build will connect to.
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    /// A redirect was not followed.
    #[error("the redirect to `{url}` was not followed: {reason}")]
    Redirect {
        /// Where the redirect pointed.
        url: String,
        /// Why it was not followed.
        reason: String,
    },
    /// The request took longer than the client allows.
    #[error("the request to `{url}` timed out after {seconds}s")]
    Timeout {
        /// The URL that was requested.
        url: String,
        /// How long the client waited.
        seconds: u64,
    },
}
