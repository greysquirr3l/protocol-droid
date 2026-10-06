//! The client: a running conversation with a child server.
//!
//! # What a client is, here
//!
//! [`StdioClient`] is the child process, the two ends of its conversation, and
//! the request documents that go between them. It is not a session manager and
//! it does not negotiate: [`Client`](crate::application::client::Client) is
//! the thing that holds a negotiated revision and a feature profile, and this
//! is the transport it makes its calls over. What the client adds to the
//! transport is three things the byte layer cannot know — a program to start,
//! a process to take down, and a deadline after which "no answer" is an
//! answer.
//!
//! # A non-answer is an error, not a hang
//!
//! [`ClientTransport`] is a trait a caller cannot interrupt once it has
//! entered, and a server that never answers is a real thing — it crashed on
//! the request, it is blocked, it is a different program entirely. So
//! [`call`](StdioClient::call) takes a deadline from the client's
//! [`ClientConfig`] and reports [`StdioClientError::TimedOut`] when it passes.
//! A caller that genuinely wants to wait forever says so with
//! [`without_request_timeout`](ClientConfig::without_request_timeout), and
//! says it on purpose.
//!
//! # The process dies with the client
//!
//! Nothing here implements `Drop` to do it. [`StdioClient`] owns a
//! [`Child`](tokio::process::Child) that was spawned with `kill_on_drop(true)`
//! by [`child::spawn`], so dropping the client kills the server — including
//! on the path where a request failed and the caller gave up, which is the path
//! that would otherwise leave an orphan.
//!
//! [`terminate`](StdioClient::terminate) is the polite version, for a caller
//! that wants the exit status and the reap rather than the kill and the
//! silence.

use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicI64, Ordering};
use core::time::Duration;
use std::process::ExitStatus;
use std::sync::Arc;

use tokio::io::AsyncWrite;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;

use crate::adapters::primary::stdio::{
    Diagnostics, StdioConfig, StdioError, StdioSink, StdioSource,
};
use crate::application::client::{ClientTransport, TransportError};
use crate::application::jsonrpc::SourceError;
use crate::domain::RequestId;
use crate::domain::ir::{JsonObject, MethodName};

use super::child::{self, SpawnError};
use super::spec::ChildSpec;
use crate::application::jsonrpc::document::RequestError;
use crate::application::jsonrpc::document::{
    notification_document, reply_result, request_document,
};

/// How long a request waits for its answer before the client calls it a
/// failure.
///
/// A default and not a protocol requirement: the specification puts no
/// deadline on a `tools/call`, and what a tool does is not this crate's to
/// know. It is here because the alternative is a client that cannot be
/// interrupted, and thirty seconds is long enough that a legitimate slow tool
/// is not failed and short enough that a dead one is noticed while the caller
/// is still waiting.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The first request id this client mints.
///
/// One rather than zero because a caller reading a log will have seen a server
/// reply with id `0` before and will spend a moment deciding whether it is
/// this client's.
const FIRST_REQUEST_ID: i64 = 1;

/// How many frames a reply may sit behind before the client stops looking.
///
/// A frame that is not this request's answer is skipped rather than treated
/// as a failure: a peer may send a notification, or the progress for this
/// very request, before the result. The count is a floor, not the bound that
/// matters — [`read_reply`](StdioClient::read_reply) carries the deadline, and
/// a peer that never answers ends as [`StdioClientError::TimedOut`] long
/// before this is reached. It exists so the skip loop is visibly finite even
/// with no deadline configured.
const FRAMES_A_REPLY_MAY_SIT_BEHIND: usize = 32;

/// How a client session is built.
///
/// Two settings, and they are different settings: the frame ceiling is a
/// property of the conversation — the same [`StdioConfig`] the source and the
/// sink are built from, so a client cannot be given one ceiling for what it
/// reads and another for what it writes — and the request timeout is a
/// property of the caller, who knows whether their tool is fast.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use protocol_droid::adapters::primary::stdio::StdioConfig;
/// use protocol_droid::adapters::secondary::stdio::ClientConfig;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = ClientConfig::new(StdioConfig::default())
///     .with_request_timeout(Duration::from_secs(5));
///
/// assert_eq!(config.request_timeout(), Some(Duration::from_secs(5)));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientConfig {
    transport: StdioConfig,
    request_timeout: Option<Duration>,
}

impl ClientConfig {
    /// The frame ceiling, and a thirty-second request deadline.
    #[must_use]
    pub const fn new(transport: StdioConfig) -> Self {
        Self {
            transport,
            request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
        }
    }

    /// Replaces the request deadline.
    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Waits for a non-answer forever.
    ///
    /// For a caller that has its own deadline and does not want two of them —
    /// a probe that must observe exactly how long a wedged server stays
    /// wedged, say. Everything else wants the default.
    #[must_use]
    pub const fn without_request_timeout(mut self) -> Self {
        self.request_timeout = None;
        self
    }

    /// The settings the source and the sink are built from.
    #[must_use]
    pub const fn transport(&self) -> StdioConfig {
        self.transport
    }

    /// The request deadline, when there is one.
    #[must_use]
    pub const fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }
}

impl Default for ClientConfig {
    /// [`ClientConfig::new`], which is the only implementation: there is no
    /// other set of defaults to choose between.
    fn default() -> Self {
        Self::new(StdioConfig::default())
    }
}

/// A running conversation with a child MCP server.
///
/// Generic over the diagnostics channel for the same reason the server's
/// [`ServeLoop`](crate::adapters::primary::stdio::ServeLoop) is: "somewhere
/// that is not the protocol stream" is a decision the caller makes, and a test
/// needs to be able to read it back.
///
/// See the module docs for what this is and is not.
pub struct StdioClient<DW> {
    child: Child,
    source: StdioSource<ChildStdout>,
    sink: StdioSink<ChildStdin>,
    diagnostics: Arc<Diagnostics<DW>>,
    forwarder: Option<JoinHandle<()>>,
    config: ClientConfig,
    next_id: AtomicI64,
}

impl<DW> core::fmt::Debug for StdioClient<DW> {
    /// Prints the process and the settings, and nothing about the streams.
    ///
    /// The same reasoning as the server's `ServeLoop`: a child's buffered
    /// bytes and a pipe's internals say nothing about which session is
    /// misbehaving, and requiring `DW: Debug` to print the one field that
    /// would be a bound nobody wants.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StdioClient")
            .field("child_id", &self.child_id())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl<DW> StdioClient<DW> {
    /// The child's process id, while it is alive.
    ///
    /// For a log line. It is not proof of anything: a caller that needs to
    /// know the process is *gone* wants
    /// [`take_stderr_forwarder`](Self::take_stderr_forwarder), because
    /// end-of-file on the child's own pipe is the kernel saying so and a
    /// process id is not.
    ///
    /// Unbounded deliberately — reading the process id touches the `Child`
    /// and nothing else, so putting it behind the `DW` bounds would mean
    /// `Debug` had to carry them too.
    #[must_use]
    pub fn child_id(&self) -> Option<u32> {
        self.child.id()
    }
}

impl<DW> StdioClient<DW>
where
    DW: AsyncWrite + Unpin + Send + 'static,
{
    /// Starts a child server and opens a conversation with it.
    ///
    /// The child's stdout becomes a [`StdioSource`], its stdin a
    /// [`StdioSink`], and its stderr is drained into `diagnostics` by a task
    /// that lives as long as the child does. All three of those adapters take
    /// the same [`StdioConfig`], which is the point: a client that could be
    /// given one frame ceiling for what it reads and another for what it
    /// writes would be a client whose two halves disagree about what a frame
    /// is.
    ///
    /// Dropping the returned client kills the child. That is the
    /// `kill_on_drop(true)` set in [`child::spawn`], and it is the reason this
    /// function is the only way to get a client.
    ///
    /// # Errors
    ///
    /// [`SpawnError::NotStarted`] when the program could not be started, and
    /// [`SpawnError::NoPipe`] when it started without one of the three pipes
    /// the conversation needs. In the second case the child is dropped on the
    /// way out, so the failure does not leave a process behind.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use protocol_droid::adapters::primary::stdio::{Diagnostics, StdioConfig};
    /// use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let spec = ChildSpec::new("my-server")?;
    /// let config = ClientConfig::new(StdioConfig::default());
    /// let client = StdioClient::spawn(&spec, config, Diagnostics::stderr()).await?;
    ///
    /// assert!(client.child_id().is_some());
    /// # Ok(())
    /// # }
    /// ```
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the public constructor is async on purpose: it is the caller's \
                   first await point in a client whose every other method is \
                   async, and dropping the `async` would change the shape of \
                   the API this module is meant to present. The body awaits \
                   nothing yet because the child is spawned synchronously and \
                   the only async work it starts (the stderr forwarder) is \
                   handed to a task."
    )]
    pub async fn spawn(
        spec: &ChildSpec,
        config: ClientConfig,
        diagnostics: Diagnostics<DW>,
    ) -> Result<Self, SpawnError> {
        let spawned = child::spawn(spec)?;
        let Some(stdin) = spawned.stdin else {
            return Err(SpawnError::NoPipe { stream: "stdin" });
        };
        let Some(stdout) = spawned.stdout else {
            return Err(SpawnError::NoPipe { stream: "stdout" });
        };
        let Some(stderr) = spawned.stderr else {
            return Err(SpawnError::NoPipe { stream: "stderr" });
        };
        let diagnostics = Arc::new(diagnostics);
        let forwarder = child::forward_stderr(stderr, Arc::clone(&diagnostics));
        Ok(Self {
            child: spawned.child,
            source: StdioSource::new(stdout, config.transport()),
            sink: StdioSink::new(stdin, config.transport()),
            diagnostics,
            forwarder: Some(forwarder),
            config,
            next_id: AtomicI64::new(FIRST_REQUEST_ID),
        })
    }

    /// The settings this client was built with.
    #[must_use]
    pub const fn config(&self) -> ClientConfig {
        self.config
    }

    /// The child's stderr channel.
    ///
    /// The child does not write to this — [`child::forward_stderr`] does, one
    /// line at a time — and neither does anything else this crate knows of.
    /// It is here so a caller can add its own, the same way the server's
    /// diagnostics channel is.
    #[must_use]
    pub fn diagnostics(&self) -> &Diagnostics<DW> {
        &self.diagnostics
    }

    /// The read half, for a caller driving the conversation itself.
    #[must_use]
    pub const fn source(&self) -> &StdioSource<ChildStdout> {
        &self.source
    }

    /// The write half, for a caller driving the conversation itself.
    #[must_use]
    pub const fn sink(&self) -> &StdioSink<ChildStdin> {
        &self.sink
    }

    /// Detaches the stderr forwarder and hands back its handle.
    ///
    /// Awaiting the handle completes when the child's stderr reaches
    /// end-of-file, which is the point: the kernel closes a process's
    /// descriptors when the process is torn down, so this is a caller being
    /// told by the operating system that the child is gone — not a caller
    /// observing that a handle was dropped.
    ///
    /// Taken rather than merely borrowed because the question it answers is
    /// asked *after* the client, and a borrow of a field would keep the client
    /// alive. Detaching the task does not stop it: it holds its own
    /// reference to the diagnostics channel and to the child's stderr, and it
    /// ends when the pipe does.
    #[must_use]
    pub const fn take_stderr_forwarder(&mut self) -> Option<JoinHandle<()>> {
        self.forwarder.take()
    }

    /// Writes one already-encoded document to the child.
    ///
    /// The framing — the delimiter, the ceiling, the flush — is
    /// [`StdioSink`]'s, which is the same code the server writes its replies
    /// with. A caller reaching for this is assembling a document itself; a
    /// caller with a method and some parameters wants [`call`](Self::call).
    ///
    /// # Errors
    ///
    /// [`StdioError::ReplyTooLarge`] when the document does not fit the
    /// configured ceiling, and [`StdioError::Transport`] when the child's
    /// stdin refused it — which, for a child that has exited, is the normal
    /// way to find out.
    pub async fn send_document(&self, document: &str) -> Result<(), StdioError> {
        StdioSink::send(&self.sink, document).await
    }

    /// Reads the child's next frame.
    ///
    /// `Ok(None)` is the child closing, and nothing else: it is
    /// [`StdioSource`]'s answer passed through unaltered, because the decoder
    /// underneath it answers `Ok(None)` for the opposite reason — a line with
    /// no terminator *yet* — and forwarding that up would turn "read more" into
    /// "the server is gone". A caller that skips a `None` here will conclude
    /// the child died on a request that was merely slow, and
    /// [`call`](Self::call) is where that mistake is most expensive.
    ///
    /// # Errors
    ///
    /// [`SourceError`] — see [`StdioSource::next_frame`].
    pub async fn next_frame(&self) -> Result<Option<String>, SourceError> {
        StdioSource::next_frame(&self.source).await
    }

    /// A fresh request id from this client's counter.
    ///
    /// A counter and not a random value, for the reason everything else in
    /// this crate that needs an identity takes one: the domain does not call
    /// a generator, so a client's ids are as reproducible as its config, and
    /// two requests in flight on one client can never collide.
    #[must_use]
    pub fn next_request_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Sends a request and returns the peer's answer.
    ///
    /// The three steps — write the document, read the next frame, match its id
    /// — are a request and a reply, and a caller that wants a different order
    /// wants [`send_document`](Self::send_document) and
    /// [`next_frame`](Self::next_frame) instead.
    ///
    /// # Errors
    ///
    /// [`StdioClientError::Document`] when the request document could not be
    /// assembled, [`StdioClientError::Stdio`] and
    /// [`StdioClientError::Source`] when the pipe refused, and
    /// [`StdioClientError::PeerClosed`] when the child closed its stdout
    /// before answering. A child that answers with a failure is
    /// [`StdioClientError::Request`], because that is a result of the request
    /// rather than a fault in the transport.
    pub async fn call(
        &self,
        id: &RequestId,
        method: &MethodName,
        params: &JsonObject,
    ) -> Result<JsonObject, StdioClientError> {
        let document = request_document(id, method, params).map_err(StdioClientError::Document)?;
        self.exchange(id, method, &document.to_string()).await
    }

    /// Writes an already-assembled document and reads the answer to it.
    ///
    /// Split out of [`call`](Self::call) so [`ClientTransport::send`] can
    /// assemble its document before the future exists. The port's return
    /// lifetime is `&self` alone, so a boxed future that borrowed the caller's
    /// `method` and `params` would not satisfy it; the document is the one
    /// thing built from both, so it is built first and the future carries
    /// only the id and the method name, which it owns.
    async fn exchange(
        &self,
        id: &RequestId,
        method: &MethodName,
        document: &str,
    ) -> Result<JsonObject, StdioClientError> {
        self.send_document(document).await?;

        // Read until this request's answer arrives, rather than taking the
        // first frame on the stream. A peer may legitimately send something
        // else first — a `notifications/message`, the progress for this very
        // request, or in this suite's case the `running 1 test` line libtest
        // writes to a child's stdout before the helper's body runs — and a
        // client that answered from frame zero would be reading whichever of
        // those happened to land first.
        //
        // Both refusals keep reading rather than propagating: a frame that is
        // not a JSON object at all is not this conversation's business, and a
        // frame carrying some other id is a real answer to something else.
        // Only the deadline in `read_reply` bounds the loop, so a peer that
        // never answers ends as `TimedOut` instead of spinning.
        for _ in 0..FRAMES_A_REPLY_MAY_SIT_BEHIND {
            let frame = self.read_reply(method).await?;
            match reply_result(&frame, id) {
                Ok(result) => return Ok(result),
                Err(RequestError::UnreadableFrame | RequestError::IdMismatch { .. }) => {}
                Err(error) => return Err(StdioClientError::Request(error)),
            }
        }
        Err(StdioClientError::NoAnswer {
            skipped: FRAMES_A_REPLY_MAY_SIT_BEHIND,
        })
    }

    /// Sends a notification, which never gets an answer.
    ///
    /// # Errors
    ///
    /// As [`send_document`](Self::send_document), plus
    /// [`StdioClientError::Document`] if the document cannot be assembled.
    pub async fn notify(
        &self,
        method: &MethodName,
        params: &JsonObject,
    ) -> Result<(), StdioClientError> {
        let document = crate::application::jsonrpc::document::notification_document(method, params)
            .map_err(StdioClientError::Document)?;
        self.send_document(&document.to_string()).await?;
        Ok(())
    }

    /// Kills the child and waits for it to be reaped.
    ///
    /// The deliberate counterpart to dropping the client, for a caller that
    /// wants the exit status and for the reap to have happened before it
    /// moves on. A dropped client kills the child too, and the operating
    /// system reaps it; it just does not tell anyone how it ended.
    ///
    /// Calling this twice is not a second kill: the second call reports
    /// `Ok(None)`, because there is nothing left to wait for.
    ///
    /// # Errors
    ///
    /// [`StdioClientError::Terminate`] when the child could not be waited for.
    pub async fn terminate(&mut self) -> Result<Option<ExitStatus>, StdioClientError> {
        let _started = self.child.start_kill();
        self.child
            .wait()
            .await
            .map(Some)
            .map_err(|_| StdioClientError::Terminate)
    }

    /// Reads one frame for [`call`](Self::call), within the deadline.
    ///
    /// Split out because the deadline is the only thing here that is not
    /// plumbing, and a reader that forgot it would be indistinguishable from
    /// one that had it.
    async fn read_reply(&self, method: &MethodName) -> Result<String, StdioClientError> {
        match self.config.request_timeout() {
            Some(limit) => match tokio::time::timeout(limit, self.next_frame()).await {
                Ok(Ok(Some(frame))) => Ok(frame),
                // The child closed, which is the end of the conversation and
                // not "not yet" — see `next_frame`.
                Ok(Ok(None)) => Err(StdioClientError::PeerClosed {
                    method: method.clone(),
                }),
                Ok(Err(source)) => Err(StdioClientError::Source(source)),
                Err(_elapsed) => Err(StdioClientError::TimedOut {
                    method: method.clone(),
                    after: limit,
                }),
            },
            None => self.next_frame().await?.map_or_else(
                || {
                    Err(StdioClientError::PeerClosed {
                        method: method.clone(),
                    })
                },
                Ok,
            ),
        }
    }
}

impl<DW> ClientTransport for StdioClient<DW>
where
    DW: AsyncWrite + Unpin + Send + 'static,
{
    /// Sends `method` with `params` under a fresh id and returns the result.
    ///
    /// The boxed future because the port asks for one: a client's transport is
    /// chosen at run time from a configuration, so there is no monomorphisation
    /// to win here and holding a `dyn` is the honest shape.
    ///
    /// The document is assembled before the future is built rather than inside
    /// it. The port elides the return lifetime to `&self`, so a boxed future
    /// cannot hold the caller's `method` or `params`; assembling first is what
    /// leaves the future owning only the id and the method name.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] naming whatever the client refused. The
    /// port is protocol-neutral and must not learn that a stdio client has a
    /// frame ceiling, so the detail is rendered rather than kept.
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>> {
        let prepared = (|| {
            let name = MethodName::new(method).map_err(unavailable)?;
            let id = self.next_request_id();
            let document = request_document(&id, &name, params)
                .map_err(|error| unavailable(StdioClientError::Document(error)))?
                .to_string();
            Ok::<_, TransportError>((id, name, document))
        })();

        Box::pin(async move {
            let (id, name, document) = prepared?;
            self.exchange(&id, &name, &document)
                .await
                .map_err(unavailable)
        })
    }

    /// Writes a notification and returns.
    ///
    /// Write-only by construction: there is no id to match a reply against, so
    /// there is nothing to wait for. The document is assembled before the
    /// future is built, for the same reason [`send`](Self::send) does it — the
    /// port elides the return lifetime, so a boxed future cannot borrow the
    /// caller's method name.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] if the document cannot be assembled or
    /// the pipe refuses the write.
    fn notify(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let prepared = (|| {
            let name = MethodName::new(method).map_err(unavailable)?;
            let document = notification_document(&name, params)
                .map_err(|error| unavailable(StdioClientError::Document(error)))?
                .to_string();
            Ok::<_, TransportError>(document)
        })();

        Box::pin(async move {
            let document = prepared?;
            self.send_document(&document).await.map_err(unavailable)
        })
    }
}

/// Anything this client refuses, as the port's own error.
fn unavailable(error: impl core::fmt::Display) -> TransportError {
    TransportError::Unavailable(error.to_string())
}

/// Why a client call did not complete.
///
/// Deliberately not
/// [`ClientError`](crate::application::client::ClientError), which is the
/// layer above: that one adds "this session cannot do that", and this one
/// cannot know. A caller that has a negotiated profile can add the fact; a
/// caller holding a transport and a pipe can only report the pipe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StdioClientError {
    /// The request document could not be assembled.
    ///
    /// Kept separate from [`Request`](Self::Request) because they happen at
    /// opposite ends of the call: this one is this client's fault and happens
    /// before a byte is written, and that one is the peer's answer arriving
    /// wrong. A caller that logged both as "bad request" would be wrong about
    /// one of them.
    #[error("the request document could not be assembled: {0}")]
    Document(crate::application::jsonrpc::document::RequestError),

    /// A frame could not be written to the child.
    #[error("the request could not be written to the child: {0}")]
    Stdio(#[from] StdioError),

    /// The child's bytes could not be read.
    #[error("the child's answer could not be read: {0}")]
    Source(#[from] SourceError),

    /// The child closed its stdout before answering.
    ///
    /// Its own account of having exited, as distinct from a child that has not
    /// answered *yet* — which is [`TimedOut`](Self::TimedOut), and is the
    /// failure a caller is most likely to meet and least likely to recognise.
    #[error("the child closed its stream before answering {method}")]
    PeerClosed {
        /// The method that went unanswered.
        method: MethodName,
    },

    /// The child did not answer within the client's deadline.
    #[error("the child did not answer {method} within {after:?}")]
    TimedOut {
        /// The method that went unanswered.
        method: MethodName,
        /// The deadline that passed.
        after: Duration,
    },

    /// The peer's answer was not a result.
    #[error("the child's answer was not a result: {0}")]
    Request(#[from] crate::application::jsonrpc::document::RequestError),

    /// A run of frames arrived that were all somebody else's.
    ///
    /// Separate from [`TimedOut`](Self::TimedOut) because the child is
    /// demonstrably alive and sending: it is answering, just never this
    /// request. A caller reading a log should not conclude the peer went away
    /// when it is in fact answering everything except what was asked.
    #[error("{skipped} frames went by and none of them was the answer")]
    NoAnswer {
        /// How many frames were read and passed over.
        skipped: usize,
    },

    /// The child could not be waited for after being killed.
    #[error("the child could not be shut down")]
    Terminate,
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use assert_matches::assert_matches;
    use serde_json::Value;
    use tokio::task::JoinError;

    use super::{ClientConfig, SpawnError, StdioClient, StdioClientError, child};
    use crate::adapters::primary::stdio::{Diagnostics, StdioConfig, StdioError};
    use crate::application::client::{ClientTransport, TransportError};
    use crate::application::jsonrpc::SourceError;
    use crate::domain::RequestId;
    use crate::domain::ir::{JsonObject, MethodName, MethodNameError};

    use super::super::spec::{ChildSpec, ChildSpecError};
    use super::super::testing::{Capture, Role, STDERR_MARKER, SpawnFailure, child_spec};
    use crate::application::jsonrpc::document::{RequestError, reply_result, request_document};

    /// Far larger than any child needs to start, answer, or be killed.
    ///
    /// Every wait in this module is wrapped in it, so a regression that turns
    /// "the peer did not answer" into a hang becomes a failed test rather than
    /// a stuck suite. It is deliberately not a realistic budget: reaching it
    /// means nothing woke the task at all, not that it was slow.
    const GUARD: Duration = Duration::from_secs(20);

    /// Short enough that the timeout it tests is observable, long enough that
    /// a loaded machine does not produce it by accident.
    const NO_ANSWER_DEADLINE: Duration = Duration::from_millis(300);

    /// What a `?` in a test here can produce.
    ///
    /// The fallible calls span four layers — the process, the byte layer, the
    /// frame parser, and this client's own error — and a test that unwrapped
    /// each one would be testing its fixtures. The payloads are carried rather
    /// than discarded so a failing test prints the layer's own account.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// A stream under the test failed.
        Io(io::Error),
        /// A child could not be described.
        ChildSpec(SpawnFailure),
        /// A child could not be started or shaped.
        Spawn(SpawnError),
        /// A frame could not be written to the child.
        Stdio(StdioError),
        /// The child's bytes could not be read.
        Source(SourceError),
        /// A document this crate built did not read back.
        Request(RequestError),
        /// A call this client refused.
        Client(StdioClientError),
        /// The [`ClientTransport`] port refused or could not deliver.
        ///
        /// A layer of its own from [`Client`](Self::Client): that one is this
        /// adapter's own answer and keeps its variants, while this is the
        /// application port's deliberately opaque refusal. Collapsing them
        /// would lose which half of the path said no.
        Transport(TransportError),
        /// A method name the IR would not admit.
        Method(MethodNameError),
        /// A spec for a child could not be built.
        Spec(ChildSpecError),
        /// A result this test could not read back as JSON.
        UnreadableResult(serde_json::Error),
        /// The stderr forwarder was still running when the guard expired.
        ///
        /// Which is the kill-on-drop failure, stated as a test outcome: the
        /// child's pipe never reached end-of-file, so the process was never
        /// killed.
        ChildOutlivedItsHandle,
        /// A child that was supposed to still be running had exited.
        ExitedEarly(std::process::ExitStatus),
        /// The child closed its stream before answering.
        ClosedEarly,
        /// Nothing in eight frames was a reply to the request.
        NoReplyInEightFrames,
    }

    impl From<io::Error> for Failure {
        fn from(error: io::Error) -> Self {
            Self::Io(error)
        }
    }

    impl From<SpawnFailure> for Failure {
        fn from(error: SpawnFailure) -> Self {
            Self::ChildSpec(error)
        }
    }

    impl From<SpawnError> for Failure {
        fn from(error: SpawnError) -> Self {
            Self::Spawn(error)
        }
    }

    impl From<StdioError> for Failure {
        fn from(error: StdioError) -> Self {
            Self::Stdio(error)
        }
    }

    impl From<SourceError> for Failure {
        fn from(error: SourceError) -> Self {
            Self::Source(error)
        }
    }

    impl From<RequestError> for Failure {
        fn from(error: RequestError) -> Self {
            Self::Request(error)
        }
    }

    impl From<StdioClientError> for Failure {
        fn from(error: StdioClientError) -> Self {
            Self::Client(error)
        }
    }

    impl From<TransportError> for Failure {
        fn from(error: TransportError) -> Self {
            Self::Transport(error)
        }
    }

    impl From<MethodNameError> for Failure {
        fn from(error: MethodNameError) -> Self {
            Self::Method(error)
        }
    }

    impl From<JoinError> for Failure {
        fn from(error: JoinError) -> Self {
            Self::Io(io::Error::other(error))
        }
    }

    impl From<ChildSpecError> for Failure {
        fn from(error: ChildSpecError) -> Self {
            Self::Spec(error)
        }
    }

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::UnreadableResult(error)
        }
    }

    /// A result read back as JSON, so a test can name a field in it.
    ///
    /// `JsonObject` is opaque by design — that is the point of it — so an
    /// assertion worth making has to parse. A substring match over `as_str`
    /// would pass on a document carrying the right characters in the wrong
    /// shape, which is a different bug from the one these tests are about.
    fn fields(result: &JsonObject) -> Result<Value, Failure> {
        Ok(serde_json::from_str(result.as_str())?)
    }

    /// The child's next real reply, with the harness's own opening line
    /// skipped.
    ///
    /// libtest prints `running 1 test` to the child's stdout before the
    /// helper's body runs, and the child's stdout *is* the protocol stream, so
    /// frame zero is always harness output. This is the one place that
    /// accounts for it — see the module docs on `testing` for why the adapter
    /// itself stays strict and the accommodation lives in the test.
    ///
    /// Only a frame that is not JSON at all is skipped. A frame that parses
    /// but is the wrong answer is a real failure and comes straight back, so
    /// this cannot paper over a child that replies with the wrong id.
    async fn next_reply(client: &StdioClient<Capture>, id: &RequestId) -> Result<String, Failure> {
        for _ in 0..8 {
            let Some(frame) = client.next_frame().await? else {
                return Err(Failure::ClosedEarly);
            };
            match reply_result(&frame, id) {
                Ok(_) => return Ok(frame),
                Err(RequestError::UnreadableFrame) => {}
                Err(error) => return Err(Failure::Request(error)),
            }
        }
        Err(Failure::NoReplyInEightFrames)
    }

    /// A running client on the given role, with a deadline this test chooses.
    async fn client_on(
        role: Role,
        timeout: Option<Duration>,
    ) -> Result<(StdioClient<Capture>, Capture), Failure> {
        let spec = child_spec(role)?;
        let mut config = ClientConfig::new(StdioConfig::default());
        if let Some(limit) = timeout {
            config = config.with_request_timeout(limit);
        } else {
            config = config.without_request_timeout();
        }
        let capture = Capture::shared();
        let client = tokio::time::timeout(
            GUARD,
            StdioClient::spawn(&spec, config, Diagnostics::new(capture.clone())),
        )
        .await
        .map_err(|_| Failure::Spawn(SpawnError::NotStarted))??;
        Ok((client, capture))
    }

    /// The default config is a thirty-second deadline over the default
    /// transport, and both knobs are reachable from the outside.
    #[test]
    fn a_config_carries_the_transport_and_the_deadline() {
        let config = ClientConfig::new(StdioConfig::default());

        assert_eq!(config.transport(), StdioConfig::default());
        assert_eq!(config.request_timeout(), Some(Duration::from_secs(30)));
        assert_eq!(config, ClientConfig::default());

        let quick = config.with_request_timeout(Duration::from_millis(5));
        assert_eq!(quick.request_timeout(), Some(Duration::from_millis(5)));
        assert_eq!(quick.transport(), StdioConfig::default());
    }

    /// A caller with its own deadline says so once, and gets no second one
    /// underneath it.
    #[test]
    fn a_config_with_no_deadline_says_so() {
        let config = ClientConfig::new(StdioConfig::default()).without_request_timeout();

        assert_eq!(config.request_timeout(), None);
    }

    /// A request goes out over the sink, comes back over the source, and the
    /// two halves are the byte layer's own — the same code the server writes
    /// its replies with, which is the whole reason this module reuses it.
    #[tokio::test]
    async fn a_call_reaches_the_child_and_comes_back() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Echo, Some(GUARD)).await?;
        let id = client.next_request_id();
        let method = MethodName::new("tools/list")?;

        let document = request_document(&id, &method, &JsonObject::default())?.to_string();
        let frame = tokio::time::timeout(GUARD, async {
            client.send_document(&document).await?;
            next_reply(&client, &id).await
        })
        .await
        .map_err(|_| Failure::ChildOutlivedItsHandle)??;

        let result = fields(&reply_result(&frame, &id)?)?;
        assert_eq!(result.get("child"), Some(&Value::Bool(true)));
        assert_eq!(result.get("method"), Some(&Value::from("tools/list")));
        Ok(())
    }

    /// The convenience call assembles the same document and takes the same
    /// answer, so a caller using it is not on a second code path.
    #[tokio::test]
    async fn a_convenience_call_returns_the_same_object() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Echo, Some(GUARD)).await?;
        let method = MethodName::new("tools/list")?;

        let result = tokio::time::timeout(
            GUARD,
            client.call(&client.next_request_id(), &method, &JsonObject::default()),
        )
        .await
        .map_err(|_| Failure::ChildOutlivedItsHandle)??;

        assert_eq!(fields(&result)?.get("role"), Some(&Value::from("echo")));
        Ok(())
    }

    /// A reply the child wrote in two pieces is one reply. The client reads a
    /// stream, not a message, and this is where that is worth a test.
    #[tokio::test]
    async fn a_reply_split_across_two_writes_is_reassembled() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Split, Some(GUARD)).await?;
        let method = MethodName::new("tools/list")?;

        let result = tokio::time::timeout(
            GUARD,
            client.call(&client.next_request_id(), &method, &JsonObject::default()),
        )
        .await
        .map_err(|_| Failure::ChildOutlivedItsHandle)??;

        assert_eq!(fields(&result)?.get("role"), Some(&Value::from("split")));
        Ok(())
    }

    /// A notification carries no id, so the child has nothing to answer and
    /// the client waits for nothing. The next request's reply is therefore the
    /// *next* frame, which is what makes this an assertion rather than a
    /// smoke test.
    #[tokio::test]
    async fn a_notification_is_written_and_never_awaited() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Echo, Some(GUARD)).await?;
        let method = MethodName::new("notifications/initialized")?;

        client.notify(&method, &JsonObject::default()).await?;

        let id = client.next_request_id();
        let listed = MethodName::new("tools/list")?;
        let frame = tokio::time::timeout(GUARD, next_reply_after(&client, &id, &listed))
            .await
            .map_err(|_| Failure::ChildOutlivedItsHandle)??;

        let result = fields(&reply_result(&frame, &id)?)?;
        assert_eq!(result.get("method"), Some(&Value::from("tools/list")));
        Ok(())
    }

    /// A child that reads the request and says nothing is a non-answer, and
    /// the client turns it into an error rather than hanging: this is the
    /// module's central promise.
    #[tokio::test]
    async fn a_child_that_never_answers_is_reported_as_a_timeout() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Silent, Some(NO_ANSWER_DEADLINE)).await?;
        let method = MethodName::new("tools/list")?;

        let outcome = tokio::time::timeout(
            GUARD,
            client.call(&client.next_request_id(), &method, &JsonObject::default()),
        )
        .await
        .map_err(|_| Failure::ChildOutlivedItsHandle)?;

        assert_matches!(
            outcome,
            Err(StdioClientError::TimedOut { method: reported, after })
                if reported == method && after == NO_ANSWER_DEADLINE
        );
        Ok(())
    }

    /// The child's stderr is not the protocol. The marker is on stderr, the
    /// reply is on stdout, and the assertion is that the marker never appears
    /// in a frame — which is the leak the forwarding code could cause, and the
    /// only one a test here can honestly make.
    #[tokio::test]
    async fn the_childs_stderr_never_reaches_the_protocol_stream() -> Result<(), Failure> {
        let (client, capture) = client_on(Role::Stderr, Some(GUARD)).await?;
        let id = client.next_request_id();
        let method = MethodName::new("tools/list")?;

        // The raw frame rather than the parsed result, because the claim is
        // about bytes: a marker the decoder happened to skip would still be a
        // marker on the wire.
        let frame = tokio::time::timeout(GUARD, next_reply_after(&client, &id, &method))
            .await
            .map_err(|_| Failure::ChildOutlivedItsHandle)??;

        // The forwarder is a separate task, so the marker arrives when it
        // arrives. Polled rather than slept, and bounded well past any real
        // latency, so a failure is fast and a pass is not luck.
        let mut seen = String::new();
        for _ in 0..200 {
            seen = capture.contents();
            if seen.contains(STDERR_MARKER) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            seen.contains(STDERR_MARKER),
            "the marker never reached the forwarder: {seen:?}"
        );
        assert!(
            !frame.contains(STDERR_MARKER),
            "the marker reached the protocol stream: {frame}"
        );
        Ok(())
    }

    /// A caller can add its own channel beside the forwarder's, which is why
    /// the client hands the channel back rather than owning it privately.
    #[tokio::test]
    async fn a_caller_can_add_its_own_diagnostics_channel() -> Result<(), Failure> {
        let (client, capture) = client_on(Role::Echo, Some(GUARD)).await?;

        client
            .diagnostics()
            .write_all(b"a line the caller chose\n")
            .await?;

        assert!(capture.contents().contains("a line the caller chose"));
        Ok(())
    }

    /// A program that cannot be started is refused, and the refusal says the
    /// program rather than leaking the operating system's error.
    #[tokio::test]
    async fn a_program_that_cannot_be_started_is_refused() -> Result<(), Failure> {
        let spec = ChildSpec::new("protocol-droid-no-such-program-for-the-tests")?;
        let config = ClientConfig::new(StdioConfig::default());

        let outcome = StdioClient::spawn(&spec, config, Diagnostics::new(Capture::shared()))
            .await
            .map_err(Failure::Spawn);

        assert_matches!(outcome, Err(Failure::Spawn(SpawnError::NotStarted)));
        Ok(())
    }

    /// `terminate` is the polite version: the caller asked, so the caller is
    /// told how it ended, and the reap has already happened when it returns.
    #[tokio::test]
    async fn terminate_kills_the_child_and_reports_the_exit() -> Result<(), Failure> {
        let (mut client, _capture) = client_on(Role::Silent, Some(GUARD)).await?;
        let pid = client.child_id();

        let status = tokio::time::timeout(GUARD, client.terminate())
            .await
            .map_err(|_| Failure::ChildOutlivedItsHandle)??;

        // No exit status to assert on beyond "it is one": a killed process is
        // a signal on Unix and an exit code elsewhere, and the claim here is
        // that the wait completed and produced a status at all.
        let Some(_status) = status else {
            return Err(Failure::ClosedEarly);
        };
        assert!(pid.is_some(), "a running client has a process id");
        Ok(())
    }

    /// Two calls on one client cannot collide on an id, which is what makes
    /// matching the answer to the request sound.
    #[tokio::test]
    async fn request_ids_advance_and_never_repeat() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Echo, Some(GUARD)).await?;

        let first = client.next_request_id();
        let second = client.next_request_id();

        assert_ne!(first, second);
        Ok(())
    }

    /// The port and the convenience call are the same conversation: the port
    /// takes a method as a string, and everything after that is identical.
    #[tokio::test]
    async fn the_port_sends_a_request_the_same_way_the_call_does() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Echo, Some(GUARD)).await?;

        let result = tokio::time::timeout(GUARD, client.send("tools/list", &JsonObject::default()))
            .await
            .map_err(|_| Failure::ChildOutlivedItsHandle)??;

        assert_eq!(
            fields(&result)?.get("method"),
            Some(&Value::from("tools/list"))
        );
        Ok(())
    }

    /// A method the IR would not admit is refused by the port without a byte
    /// being written, rather than being sent and refused by the peer.
    #[tokio::test]
    async fn the_port_refuses_a_method_the_ir_will_not_admit() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Echo, Some(GUARD)).await?;

        let outcome = client.send("", &JsonObject::default()).await;

        // Matched on the variant, not on the rendered text.
        // `TransportError::Unavailable` is a transparent newtype over its
        // payload, so its `Display` is the reason alone and the word
        // "Unavailable" never appears in it — a substring assertion here would
        // be testing the `#[error]` attribute rather than the refusal.
        assert_matches!(outcome, Err(TransportError::Unavailable(reason)) if reason.contains("empty"));
        Ok(())
    }

    /// The client's own report of itself names the process and the settings,
    /// and nothing about the pipes.
    #[tokio::test]
    async fn a_client_names_its_child_and_its_configuration() -> Result<(), Failure> {
        let (client, _capture) = client_on(Role::Echo, Some(GUARD)).await?;

        assert!(
            client.child_id().is_some(),
            "a running client has a process id"
        );
        assert_eq!(client.config().request_timeout(), Some(GUARD));
        assert!(format!("{client:?}").contains("StdioClient"));

        // The halves are handed over rather than kept private, so a caller
        // driving the conversation itself gets the byte layer's own types.
        let _source = client.source();
        let _sink = client.sink();
        Ok(())
    }

    /// The load-bearing claim in `child.rs`: dropping the process handle kills
    /// the process, and the proof is the kernel closing the child's own pipe.
    ///
    /// The obvious version of this test proves nothing. It drops a whole
    /// `StdioClient` and waits for the same forwarder — and passes either way,
    /// because the client owns the child's stdin, so dropping it closes the
    /// write end, the child reads end-of-file, and it exits without anyone
    /// killing it. That is a test of stdin closure wearing kill-on-drop's
    /// name, and it would keep passing if the flag were removed.
    ///
    /// So this one keeps the write end and drops only the handle. With stdin
    /// held, the child cannot reach its `Ok(0) => break`, and the kill is the
    /// only thing left that can end it.
    #[tokio::test]
    async fn dropping_the_child_handle_kills_the_process() -> Result<(), Failure> {
        let spec = child_spec(Role::Silent)?;
        let spawned = child::spawn(&spec)?;

        let Some(stdin) = spawned.stdin else {
            return Err(Failure::Spawn(SpawnError::NoPipe { stream: "stdin" }));
        };
        let Some(_stdout) = spawned.stdout else {
            return Err(Failure::Spawn(SpawnError::NoPipe { stream: "stdout" }));
        };
        let Some(stderr) = spawned.stderr else {
            return Err(Failure::Spawn(SpawnError::NoPipe { stream: "stderr" }));
        };
        // Named and bound so the intent survives review: this handle is the
        // only thing keeping the child off its end-of-file path, and it must
        // outlive the drop below.
        let _stdin_held_open = stdin;

        let forwarder =
            child::forward_stderr(stderr, Arc::new(Diagnostics::new(Capture::shared())));

        // The only statement in this test that could end the child.
        drop(spawned.child);

        // End-of-file on the child's own stderr is the operating system
        // saying the process is gone — not a handle's `Drop` having run, which
        // is the distinction the module docs make. The guard is far larger
        // than a kill needs, so reaching it means the kill never happened.
        tokio::time::timeout(GUARD, forwarder)
            .await
            .map_err(|_| Failure::ChildOutlivedItsHandle)??;
        Ok(())
    }

    /// The control for the test above, and the reason it means anything.
    ///
    /// The same child, started the same way but *without* `kill_on_drop`, is
    /// still running half a second later. Without this, the forwarder
    /// completing above would be equally consistent with a child that exited
    /// on its own — which is exactly the false proof the test above is shaped
    /// to avoid, and this rules that out from the other side by showing the
    /// child does not exit by itself.
    #[tokio::test]
    async fn a_child_that_was_not_killed_is_still_running() -> Result<(), Failure> {
        let spec = child_spec(Role::Silent)?;
        let mut command = spec.to_command();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // No `kill_on_drop` on this one, and that is the entire difference
        // between this process and the one the test above drops.
        let mut child = command.spawn()?;

        // As above: holding stdin is what keeps the child off its EOF path.
        let _stdin_held_open = child.stdin.take();
        let _stdout = child.stdout.take();
        let _stderr = child.stderr.take();

        tokio::time::sleep(Duration::from_millis(500)).await;

        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => return Err(Failure::ExitedEarly(status)),
            Err(error) => return Err(Failure::Io(error)),
        }

        // Nothing else will kill this child but this test, so it is cleaned
        // up here rather than left for the machine to reap.
        let _killed = child.start_kill();
        let _reaped = child.wait().await;
        Ok(())
    }

    /// Writes a request the long way and reads its answer, so a test can hold
    /// the write and the read separately.
    async fn next_reply_after(
        client: &StdioClient<Capture>,
        id: &RequestId,
        method: &MethodName,
    ) -> Result<String, Failure> {
        let document = request_document(id, method, &JsonObject::default())?.to_string();
        client.send_document(&document).await?;
        next_reply(client, id).await
    }
}
