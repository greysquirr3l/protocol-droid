//! The sink a session's handler-initiated messages go out on.
//!
//! Publishing to the session's tail is what makes a notification — a log
//! record, a progress increment — reach the client: the router streams the
//! tail as the `POST`'s response body, and anything a handler writes here
//! arrives as an SSE frame on that same request.
//!
//! A sink that discarded every frame would fail quietly and loudly elsewhere:
//! a tool that logs correctly looks like a tool that logs nothing, and a
//! client waiting for three log records sees zero and reports the server as
//! broken rather than as unwired.

use serde_json::Value;

use super::Tail;
use crate::application::session::frame_sink::{FrameSink, SinkError};

/// The sink a session's handler-initiated messages go out on.
///
/// Publishing to the session's tail is what makes a notification — a log
/// record, a progress increment — reach the client: the router streams the
/// tail as the `POST`'s response body, and anything a handler writes here
/// arrives as an SSE frame on that same request.
///
/// A sink that discarded every frame would fail quietly and loudly elsewhere: a
/// tool that logs correctly looks like a tool that logs nothing, and a client
/// waiting for three log records sees zero and reports the server as broken
/// rather than as unwired.
#[derive(Debug, Clone)]
pub(in crate::adapters::primary::transport::http) struct TailSink {
    pub(in crate::adapters::primary::transport::http) tail: Tail,
}

impl TailSink {
    /// Publishes one encoded frame, synchronously.
    ///
    /// # Errors
    ///
    /// [`SinkError`] when the frame is not JSON.
    pub(in crate::adapters::primary::transport::http) fn push(
        &self,
        frame: &str,
    ) -> Result<(), SinkError> {
        serde_json::from_str::<Value>(frame)
            .map(|value| {
                self.tail.push(value);
            })
            .map_err(|_| SinkError)
    }
}

impl FrameSink for TailSink {
    /// Publishes one frame to the session's tail.
    ///
    /// Parsed on the way in rather than trusted, because the tail holds
    /// `Value`: a frame that is not JSON has nothing to stream, and refusing it
    /// here leaves a caller that can still be told why.
    ///
    /// # Errors
    ///
    /// [`SinkError`] when the frame is not JSON.
    fn send(
        &self,
        frame: &str,
    ) -> impl core::future::Future<Output = Result<(), SinkError>> + Send {
        core::future::ready(self.push(frame))
    }
}
