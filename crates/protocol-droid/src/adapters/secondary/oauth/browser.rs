//! A user agent for a person with a browser.
//!
//! [`LoopbackBrowser`] opens the authorization URL in the system browser and
//! waits for the authorization server to send the browser back to a loopback
//! address, which is how RFC 8252 has a native app receive an authorization
//! response.
//!
//! The redirect URI has to be known before the client registers, so the
//! listener is bound first and the caller reads [`redirect_uri`] off it:
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! use protocol_droid::adapters::secondary::oauth::LoopbackBrowser;
//! use protocol_droid::application::oauth::authorizer::OAuthConfig;
//! use protocol_droid::domain::ProtocolVersion;
//!
//! let browser = LoopbackBrowser::bind().await?;
//! let config = OAuthConfig::new("https://mcp.example.com/mcp", ProtocolVersion::V2025_11_25)?
//!     .redirect_to(browser.redirect_uri());
//! # let _ = config;
//! # Ok(())
//! # }
//! ```
//!
//! [`redirect_uri`]: LoopbackBrowser::redirect_uri
//!
//! The listener accepts connections from this machine only, answers a single
//! path, reads at most a few kilobytes of any request, and treats anything it
//! cannot parse as not the redirect and keeps waiting. The `state` check that
//! ties a response to the request that started it happens in the flow, not
//! here: this type delivers whatever arrives at the callback path.

use core::future::Future;
use core::pin::Pin;
use core::time::Duration;
use std::sync::Arc;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

use crate::adapters::secondary::oauth::environment::{UserAgent, callback_from};
use crate::application::oauth::callback::AuthorizationCallback;
use crate::application::oauth::environment::EnvironmentError;

/// The path the authorization server is sent back to.
const CALLBACK_PATH: &str = "/callback";

/// The most of a request this listener reads before giving up on it.
const MAX_REQUEST: usize = 8 * 1024;

/// How long one connection may take to send its request line.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the user has to finish at the authorization server.
const DEFAULT_PATIENCE: Duration = Duration::from_secs(300);

const PAGE: &str = "<!doctype html><title>Signed in</title>\
<p>You can close this tab and return to the terminal.</p>";

/// Opens a URL for the user.
type Opener = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// Receives an authorization response on `127.0.0.1`.
pub struct LoopbackBrowser {
    listener: TcpListener,
    redirect_uri: String,
    patience: Duration,
    open: Opener,
}

impl core::fmt::Debug for LoopbackBrowser {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LoopbackBrowser")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl LoopbackBrowser {
    /// Binds a loopback port chosen by the operating system.
    ///
    /// # Errors
    ///
    /// [`EnvironmentError::Unavailable`] when no port could be bound.
    pub async fn bind() -> Result<Self, EnvironmentError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| EnvironmentError::Unavailable(error.to_string()))?;
        let port = listener
            .local_addr()
            .map_err(|error| EnvironmentError::Unavailable(error.to_string()))?
            .port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
            patience: DEFAULT_PATIENCE,
            open: Arc::new(open_in_browser),
        })
    }

    /// The URI to register as this client's redirect.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// How long to wait for the user. Defaults to five minutes.
    #[must_use]
    pub const fn waiting(mut self, patience: Duration) -> Self {
        self.patience = patience;
        self
    }

    /// Replaces how the URL is shown to the user, for a headless machine or a
    /// test. The default starts the system browser.
    #[must_use]
    pub fn opening_with(
        mut self,
        open: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.open = Arc::new(open);
        self
    }

    /// Accepts connections until one is the redirect.
    async fn await_redirect(&self) -> Result<AuthorizationCallback, EnvironmentError> {
        loop {
            let (mut stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|error| EnvironmentError::Declined(error.to_string()))?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let Some(target) = read_target(&mut stream).await else {
                respond(&mut stream, "400 Bad Request", "").await;
                continue;
            };
            let (path, _) = target.split_once('?').unwrap_or((target.as_str(), ""));
            if path != CALLBACK_PATH {
                respond(&mut stream, "404 Not Found", "").await;
                continue;
            }
            // Parsed against the loopback origin: only the query matters.
            let callback = callback_from(&format!("http://127.0.0.1{target}"));
            respond(&mut stream, "200 OK", PAGE).await;
            return callback;
        }
    }
}

impl UserAgent for LoopbackBrowser {
    fn authorize<'a>(
        &'a self,
        authorization_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<AuthorizationCallback, EnvironmentError>> + Send + 'a>>
    {
        Box::pin(async move {
            // The URL comes from a document the authorization server served, so
            // only a web URL is handed to the operating system.
            if !(authorization_url.starts_with("https://")
                || authorization_url.starts_with("http://"))
            {
                return Err(EnvironmentError::Declined(
                    "the authorization URL is not a web URL".to_owned(),
                ));
            }
            // Always shown, so a machine with no browser can be driven by pasting.
            eprintln!("Sign in at: {authorization_url}");
            if let Err(reason) = (self.open)(authorization_url) {
                eprintln!("(could not start a browser: {reason})");
            }
            tokio::time::timeout(self.patience, self.await_redirect())
                .await
                .map_err(|_| {
                    EnvironmentError::Declined("no one completed the sign-in in time".to_owned())
                })?
        })
    }
}

/// Reads the request target out of the request line, or `None` for anything
/// that is not a small `GET`.
async fn read_target(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(512);
    let mut chunk = [0_u8; 512];
    let read = tokio::time::timeout(READ_TIMEOUT, async {
        loop {
            let count = stream.read(&mut chunk).await.ok()?;
            if count == 0 {
                return None;
            }
            buffer.extend_from_slice(chunk.get(..count)?);
            if buffer.windows(2).any(|pair| pair == b"\r\n") || buffer.len() > MAX_REQUEST {
                return Some(());
            }
        }
    })
    .await;
    read.ok()??;
    let line = buffer.split(|byte| *byte == b'\r').next()?;
    let line = core::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    let (method, target) = (parts.next()?, parts.next()?);
    (method == "GET" && target.starts_with('/')).then(|| target.to_owned())
}

/// Writes a minimal response and closes. Failures are ignored: the browser
/// going away after the redirect changes nothing about the answer.
async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Starts the platform's URL handler without a shell.
fn open_in_browser(url: &str) -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
