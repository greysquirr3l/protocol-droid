//! Reading a server's event stream as the SSE standard defines it.
//!
//! The server side of this crate writes ids as numbers, and [`SseEvent`]
//! enforces that on both ends because it is the one thing this crate controls.
//! A *client* does not control the server it talks to. For `Last-Event-ID` the
//! standard calls the id an opaque string, and `2025-11-25` (SEP-1699) has
//! servers send `id: event-7` and `retry: 500` on a priming event with no data,
//! then close the stream and expect the client to come back after the interval
//! with the last id. A reader that rejected a non-numeric id, or dropped an
//! event for having no data, could never resume against such a server.
//!
//! The rules here are the standard's (HTML Living Standard, "Server-sent
//! events", *parsing an event stream*):
//!
//! - `id` sets the last event id, and is ignored if it contains NUL;
//! - `retry` sets the reconnection time, and is ignored unless it is all ASCII
//!   digits;
//! - an event with no `data` dispatches nothing but its `id` and `retry` still
//!   take effect — which is exactly what a priming event is.
//!
//! [`SseEvent`]: crate::adapters::primary::transport::http::SseEvent

use std::collections::VecDeque;
use std::time::Duration;

use super::error::HttpClientError;
use super::frames::{FrameError, split_block};

/// The longest block accepted before one is declared runaway.
const MAX_BLOCK: usize = 64 * 1024;

/// One event off the wire, with whatever it changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WireEvent {
    /// The event's id, verbatim, when it carried a usable one.
    pub(super) id: Option<String>,
    /// The reconnection time it set, when it set one.
    pub(super) retry: Option<Duration>,
    /// The event's data, or `None` when it dispatched none.
    pub(super) data: Option<String>,
}

/// Splits a byte stream into [`WireEvent`]s.
#[derive(Debug, Default)]
pub(super) struct WireReader {
    buffer: Vec<u8>,
}

impl WireReader {
    /// An empty reader.
    #[must_use]
    pub(super) const fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Adds `chunk` and returns every event it completed.
    ///
    /// # Errors
    ///
    /// [`FrameError::BlockTooLong`] for a peer that sends a huge block with no
    /// terminator.
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<WireEvent>, FrameError> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > MAX_BLOCK {
            self.buffer.clear();
            return Err(FrameError::BlockTooLong { limit: MAX_BLOCK });
        }
        let mut events = Vec::new();
        let mut consumed = 0_usize;
        while let Some(tail) = self.buffer.get(consumed..) {
            let Some((block, used)) = split_block(tail) else {
                break;
            };
            if let Some(event) = parse(block) {
                events.push(event);
            }
            consumed = consumed.saturating_add(used);
        }
        if consumed > 0 {
            let _ = self.buffer.drain(..consumed);
        }
        Ok(events)
    }
}

/// Parses one block. `None` for a block that is only comments.
fn parse(block: &str) -> Option<WireEvent> {
    let mut event = WireEvent::default();
    let mut data: Vec<&str> = Vec::new();
    let mut any = false;
    for line in block.split(['\n', '\r']) {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        any = true;
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "id" if !value.contains('\0') => event.id = Some(value.to_owned()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                event.retry = value.parse::<u64>().ok().map(Duration::from_millis);
            }
            "data" => data.push(value),
            _ => {}
        }
    }
    if !any {
        return None;
    }
    let joined = data.join("\n");
    event.data = (!joined.is_empty()).then_some(joined);
    Some(event)
}

/// An open response, read as a sequence of [`WireEvent`]s.
pub(super) struct WireStream {
    response: reqwest::Response,
    reader: WireReader,
    ready: VecDeque<WireEvent>,
}

impl WireStream {
    /// Wraps a response whose body is an event stream.
    #[must_use]
    pub(super) const fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            reader: WireReader::new(),
            ready: VecDeque::new(),
        }
    }

    /// The next event, or `None` once the peer has ended the body.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::Io`] for a transport failure mid-body and
    /// [`HttpClientError::Frame`] for a runaway block.
    pub(super) async fn next(&mut self) -> Result<Option<WireEvent>, HttpClientError> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Ok(Some(event));
            }
            let chunk = self
                .response
                .chunk()
                .await
                .map_err(|error| HttpClientError::Io {
                    method: "POST",
                    error,
                })?;
            let Some(chunk) = chunk else {
                return Ok(None);
            };
            self.ready.extend(self.reader.push(&chunk)?);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{WireEvent, WireReader, parse};

    #[test]
    fn a_priming_event_sets_id_and_retry_and_dispatches_no_data() {
        let event = parse("id: event-1\nretry: 500\ndata: ");
        assert_eq!(
            event,
            Some(WireEvent {
                id: Some("event-1".to_owned()),
                retry: Some(Duration::from_millis(500)),
                data: None,
            })
        );
    }

    #[test]
    fn ids_are_opaque_text() {
        assert_eq!(
            parse("id: abc-123\ndata: x").and_then(|event| event.id),
            Some("abc-123".to_owned())
        );
    }

    #[test]
    fn a_retry_that_is_not_digits_is_ignored() {
        assert_eq!(parse("retry: 5s\ndata: x").and_then(|event| event.retry), None);
        assert_eq!(parse("retry: -1\ndata: x").and_then(|event| event.retry), None);
    }

    #[test]
    fn an_id_with_nul_is_ignored() {
        assert_eq!(parse("id: a\0b\ndata: x").and_then(|event| event.id), None);
    }

    #[test]
    fn comments_alone_are_nothing() {
        assert_eq!(parse(": keep-alive"), None);
    }

    #[test]
    fn multi_line_data_joins_with_newlines() {
        assert_eq!(
            parse("data: a\ndata: b").and_then(|event| event.data),
            Some("a\nb".to_owned())
        );
    }

    #[test]
    fn events_split_across_chunks_reassemble() {
        let mut reader = WireReader::new();
        assert!(reader.push(b"id: 1\nda").unwrap_or_default().is_empty());
        let events = reader.push(b"ta: hi\n\n").unwrap_or_default();
        assert_eq!(events.len(), 1);
        assert_eq!(events.first().and_then(|e| e.data.as_deref()), Some("hi"));
    }
}
