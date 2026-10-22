//! How a handler talks back to the peer.
//!
//! [`CallbackExchange`] is the port a transport implements to answer the
//! server-originated requests a handler makes — sampling, elicitation, roots —
//! and [`DynExchange`] is its boxed form for the callers that need dynamic
//! dispatch. [`ExchangeError`] is what either returns when an exchange does
//! not complete.
//!
//! It lives here rather than beside the adapters that implement it because
//! these are port traits and a port belongs to its consumer. The consumer is
//! [`PeerSession`](super::context::PeerSession), which hands one to every
//! request; a transport's `impl` should be written against the shape that
//! consumer asked for, and reading the consumer and the port in one file is
//! what makes that check possible.

use crate::domain::ir::JsonObject;

/// Why a server-initiated request could not complete.
///
/// Distinct from [`GatedError`](super::error::GatedError): the gate already
/// passed by the time an exchange runs, so this is the transport or the peer
/// failing rather than the capability being absent. They are separate types
/// because a handler that sees one should retry and a handler that sees the
/// other should not.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the exchange did not complete: {reason}")]
pub struct ExchangeError {
    /// What went wrong, in the adapter's words.
    ///
    /// A `String` because the transport names its own failures (a closed
    /// stream, a cancelled request, a timeout) and the application layer has no
    /// business enumerating them. The rule is that application code never names
    /// a _third-party_ error type; a transport's own description is data the
    /// adapter chose to hand over, not a type this signature commits to.
    reason: String,
}

impl ExchangeError {
    /// Builds an exchange failure from whatever the transport reported.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// A round trip to the peer that a server-initiated request needs.
///
/// The port belongs to the consumer. It is declared here rather than in an
/// adapter because the shape of what a server needs to ask a peer is a decision
/// this module makes: a method name, a params object, and an answer back. A
/// transport implements it; nothing in `adapters/` defines it.
///
/// Not object-safe by design — it is an RPITIT `async fn`, so a caller that
/// knows its concrete exchange gets it inlined with no allocation. Callers that
/// genuinely need a boxed one get [`DynExchange`].
pub trait CallbackExchange {
    /// Sends `method` to the peer and returns its answer.
    ///
    /// The implementation owns framing: it knows the negotiated revision, it
    /// knows the method's wire name, and it parses the reply. Nothing above
    /// this trait ever sees a byte.
    ///
    /// # Errors
    ///
    /// [`ExchangeError`] when the request could not be delivered or the peer
    /// did not answer. Whether a non-answer is an error or a hang is the
    /// adapter's decision, and a server that waits forever is a server whose
    /// connection never closes.
    fn exchange(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> impl Future<Output = Result<JsonObject, ExchangeError>> + Send;
}

/// The boxed spelling of [`CallbackExchange`], for a caller holding a transport
/// of unknown concrete type.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::server::context::{DynExchange, ExchangeError};
/// use protocol_droid::domain::ir::JsonObject;
///
/// let exchange: Arc<dyn DynExchange> = Arc::new(MyExchange);
/// // ...handed to a PeerSession, which calls it through the trait object.
/// # struct MyExchange;
/// # impl DynExchange for MyExchange {
/// #     fn exchange_boxed<'a>(&'a self, method: &'a str, params: &'a JsonObject)
/// #         -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>
/// #     { Box::pin(async move { let _ = (method, params); Err(ExchangeError::new("unimplemented")) }) }
/// # }
/// ```
pub trait DynExchange: Send + Sync {
    /// The boxed form of [`CallbackExchange::exchange`].
    ///
    /// Written out rather than derived, for the same reason `DynFrameSink` is:
    /// the box belongs to callers that need dynamic dispatch, not to the ones
    /// that do not.
    fn exchange_boxed<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>;
}

impl<T: CallbackExchange + Send + Sync> DynExchange for T {
    fn exchange_boxed<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>
    {
        Box::pin(self.exchange(method, params))
    }
}
