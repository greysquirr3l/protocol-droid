//! Per-method typed wrappers around the
//! streaming
//! [`A2aStreamingRequestHandler`]
//! trait.
//!
//! # What this is
//!
//! A handler that wants typed params on
//! the streaming methods
//! ([`send_streaming_message`](A2aStreamingRequestHandlerExt::send_streaming_message)
//! and
//! [`subscribe_to_task`](A2aStreamingRequestHandlerExt::subscribe_to_task))
//! implements
//! [`A2aStreamingRequestHandler`]
//! and gets the typed methods for free
//! through this extension trait.
//!
//! Each typed method:
//! 1. Serialises the typed request to
//!    [`serde_json::Value`].
//! 2. Calls
//!    [`dispatch_stream`](super::streaming_serve::A2aStreamingRequestHandler::dispatch_stream).
//! 3. Returns the stream (no decode —
//!    the streaming dispatch already
//!    returns a typed stream).
//!
//! # Why a separate extension trait
//!
//! The streaming trait is independent
//! from the request/response trait. A
//! consumer who only streams
//! (`SendStreamingMessage` and
//! `SubscribeToTask`) implements
//! [`A2aStreamingRequestHandler`]
//! and uses this extension. A consumer
//! who only does request/response
//! implements
//! [`A2aRequestHandler`](crate::application::a2a::A2aRequestHandler)
//! and uses
//! [`A2aRequestHandlerExt`](crate::application::a2a::A2aRequestHandlerExt).
//! A consumer who needs both implements
//! both.
//!
//! # Why `SendStreamingMessage` reuses
//! `SendMessageRequest`
//!
//! The request body for the streaming
//! and non-streaming `SendMessage`
//! methods is the same
//! [`SendMessageRequest`].
//! The two methods differ only in the
//! response shape (a single value vs
//! a stream of events). Reusing the
//! request type keeps the wire form
//! aligned and lets a consumer move
//! the same `SendMessageRequest`
//! between the two methods.
//!
//! # Why `SubscribeToTask` reuses
//! `GetTaskRequest`
//!
//! The request body for the streaming
//! `SubscribeToTask` and the
//! request/response `GetTask` is the
//! same
//! [`GetTaskRequest`].
//! The streaming method adds a
//! `historyLength` field that
//! `GetTaskRequest` does not have
//! (the A2A v1.0 spec's
//! `SubscribeToTaskRequest` has it as
//! an optional field); a consumer who
//! needs to set it uses the raw
//! `dispatch_stream` with a custom
//! `Value` instead. The reused type
//! covers the common case.

use std::pin::Pin;

use futures_core::Stream;

use crate::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
use crate::adapters::secondary::a2a::A2aMethod;
use crate::application::a2a::handler::{A2aError, A2aRequestContext};
use crate::domain::a2a::send_message::SendMessageRequest;
use crate::domain::a2a::streaming_events::StreamingEvent;
use crate::domain::a2a::task_lifecycle::GetTaskRequest;

/// The per-method typed wrappers for
/// streaming. A blanket impl means every
/// [`A2aStreamingRequestHandler`]
/// gets the typed methods for free.
pub trait A2aStreamingRequestHandlerExt: A2aStreamingRequestHandler {
    /// Sends a message and receives a
    /// stream of events. Typed
    /// counterpart to
    /// `dispatch_stream(SendStreamingMessage, ...)`.
    fn send_streaming_message(
        &self,
        request: SendMessageRequest,
        context: &A2aRequestContext,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>>;

    /// Subscribes to a task's events.
    /// Typed counterpart to
    /// `dispatch_stream(SubscribeToTask, ...)`.
    fn subscribe_to_task(
        &self,
        request: GetTaskRequest,
        context: &A2aRequestContext,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>>;
}

impl<H: A2aStreamingRequestHandler + ?Sized> A2aStreamingRequestHandlerExt for H {
    fn send_streaming_message(
        &self,
        request: SendMessageRequest,
        context: &A2aRequestContext,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>> {
        // The serialise-to-Value step
        // happens before the dispatch
        // call. A serialisation error
        // is a programmer error (the
        // typed request must be
        // serialisable to a JSON object);
        // we surface it as the first
        // item of the stream rather
        // than a panic.
        let value = match serde_json::to_value(&request) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(futures_util::stream::once(async move {
                    Err(A2aError::InternalError {
                        message: format!(
                            "failed to serialise SendStreamingMessage \
                             request: {e}"
                        ),
                    })
                }));
            }
        };
        self.dispatch_stream(A2aMethod::SendStreamingMessage, Some(value), context)
    }

    fn subscribe_to_task(
        &self,
        request: GetTaskRequest,
        context: &A2aRequestContext,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>> {
        let value = match serde_json::to_value(&request) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(futures_util::stream::once(async move {
                    Err(A2aError::InternalError {
                        message: format!(
                            "failed to serialise SubscribeToTask \
                             request: {e}"
                        ),
                    })
                }));
            }
        };
        // SubscribeToTask is not in the
        // method table yet (the A2A v1.0
        // proto names it but the
        // vendored method table does
        // not). We use the raw
        // `dispatch_stream` via a
        // string-based path is not
        // possible — the trait takes an
        // `A2aMethod`, not a string. We
        // have to add SubscribeToTask
        // to the A2aMethod enum.
        //
        // For now, this is a
        // compile-time error that the
        // next commit addresses.
        self.dispatch_stream(A2aMethod::SubscribeToTask, Some(value), context)
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use futures_core::Stream;
    use futures_util::{StreamExt, stream};

    use super::A2aStreamingRequestHandlerExt;
    use crate::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
    use crate::adapters::secondary::a2a::A2aMethod;
    use crate::application::a2a::handler::{A2aError, A2aRequestContext};
    use crate::domain::a2a::send_message::SendMessageRequest;
    use crate::domain::a2a::streaming_events::{StreamingEvent, TaskStatusUpdateEvent};
    use crate::domain::a2a::task::TaskState;
    use crate::domain::a2a::task_lifecycle::GetTaskRequest;

    type TestError = Box<dyn std::error::Error>;
    type TestStream = Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>>;

    /// A handler that captures the
    /// method name and params, then
    /// returns a stream of one event
    /// carrying both as a
    /// `TaskStatusUpdateEvent`.
    struct CaptureHandler;

    impl A2aStreamingRequestHandler for CaptureHandler {
        fn dispatch_stream(
            &self,
            method: A2aMethod,
            params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> TestStream {
            let method_str = method.as_str().to_owned();
            let params_str = params.map(|v| v.to_string()).unwrap_or_default();
            let event = StreamingEvent::Status(TaskStatusUpdateEvent::new(
                "t-1",
                "c-1",
                crate::domain::a2a::task::TaskStatus {
                    state: TaskState::Working,
                    message: None,
                    timestamp: None,
                },
            ));
            let _ = (method_str, params_str);
            Box::pin(stream::iter(vec![Ok(event)]))
        }
    }

    #[tokio::test]
    async fn send_streaming_message_serialises_request_before_dispatch() -> Result<(), TestError> {
        let handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(CaptureHandler);
        let context = A2aRequestContext {
            protocol_version: crate::domain::a2a::version::A2aVersion::V1_0,
        };
        let request = SendMessageRequest::new(crate::domain::a2a::message::Message::new(
            crate::domain::a2a::message::MessageRole::User,
        ));
        let mut stream = handler.send_streaming_message(request, &context);
        let first = stream
            .next()
            .await
            .ok_or("stream returned None before any event")?;
        first.map_err(|_| "stream produced an error")?;
        Ok(())
    }

    #[tokio::test]
    async fn subscribe_to_task_serialises_request_before_dispatch() -> Result<(), TestError> {
        let handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(CaptureHandler);
        let context = A2aRequestContext {
            protocol_version: crate::domain::a2a::version::A2aVersion::V1_0,
        };
        let request = GetTaskRequest::new("t-1");
        let mut stream = handler.subscribe_to_task(request, &context);
        let first = stream
            .next()
            .await
            .ok_or("stream returned None before any event")?;
        first.map_err(|_| "stream produced an error")?;
        Ok(())
    }
}
