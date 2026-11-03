//! Per-method typed wrappers around the single-dispatch
//! [`AcpRequestHandler`](super::AcpRequestHandler)
//! trait.
//!
//! # What this is
//!
//! A handler that wants typed params and typed
//! results implements
//! [`AcpRequestHandler`](super::AcpRequestHandler)
//! (the single-dispatch shape) and gets one typed
//! method per ACP method through the
//! [`AcpRequestHandlerExt`] extension trait.
//!
//! Each typed method:
//! 1. Serialises the typed request to
//!    [`serde_json::Value`].
//! 2. Calls
//!    [`dispatch`](super::AcpRequestHandler::dispatch).
//! 3. Deserialises the returned `Value` to the
//!    typed response.
//!
//! # Why an extension trait, not a replacement
//!
//! The single-dispatch trait is the object-safe
//! shape: a consumer who wants
//! `Arc<dyn AcpRequestHandler>` (handler swapping
//! at runtime) uses it directly. The typed
//! wrappers return concrete futures and are not
//! object-safe. An extension trait lets a consumer
//! pick the shape they want without compromising
//! the other.
//!
//! # Why the typed wrappers are not
//! default-implemented on the trait
//!
//! A consumer who wants only the typed shape
//! writes no `dispatch` body — the extension
//! trait's body is the single-dispatch call. A
//! consumer who wants the single-dispatch shape
//! writes no typed methods — the extension
//! trait's methods are available but unused. The
//! two shapes are independent and layered.
//!
//! # Why this does not cover all 13 agent methods
//!
//! The v1.25.0 schema has 13 agent methods. This
//! module covers all 13: `initialize`, `authenticate`,
//! `session/new`, `session/load`, `session/list`,
//! `session/delete`, `session/resume`, `session/close`,
//! `session/set_mode`, `session/set_config_option`,
//! `session/prompt`, `session/cancel`, and `logout`.
//! `session/cancel` is a *notification* rather than a
//! request, so it has no typed wrapper here — it rides
//! the notification stream instead.

use core::pin::Pin;

use futures_core::Future;

use super::dispatch::{AcpError, AcpRequestContext, AcpRequestHandler};
use crate::domain::acp::config::{SetSessionConfigOptionRequest, SetSessionConfigOptionResponse};
use crate::domain::acp::initialize::{
    AuthenticateRequest, AuthenticateResponse, InitializeRequest, InitializeResponse,
};
use crate::domain::acp::method::AgentMethod;
use crate::domain::acp::session::{
    CloseSessionRequest, CloseSessionResponse, DeleteSessionRequest, DeleteSessionResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, LoadSessionResponse,
    LogoutRequest, LogoutResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, ResumeSessionRequest, ResumeSessionResponse, SetSessionModeRequest,
    SetSessionModeResponse,
};

/// The per-method typed wrappers. A blanket impl
/// means every [`AcpRequestHandler`] gets the
/// typed methods for free.
pub trait AcpRequestHandlerExt: AcpRequestHandler {
    // --- initialize ----------------------------------------------

    /// Handshake. Typed counterpart to
    /// `dispatch(Initialize, ...)`.
    fn initialize<'a>(
        &'a self,
        request: InitializeRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<InitializeResponse, AcpError>> + Send + 'a>>;

    // --- authenticate --------------------------------------------

    /// Proves the editor's identity with one of the auth
    /// methods the agent advertised. Typed counterpart to
    /// `dispatch(Authenticate, ...)`.
    fn authenticate<'a>(
        &'a self,
        request: AuthenticateRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<AuthenticateResponse, AcpError>> + Send + 'a>>;

    // --- session/new ---------------------------------------------

    /// Opens a new session. Typed counterpart to
    /// `dispatch(SessionNew, ...)`.
    fn session_new<'a>(
        &'a self,
        request: NewSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<NewSessionResponse, AcpError>> + Send + 'a>>;

    // --- session/load --------------------------------------------

    /// Resumes a session from a checkpoint. Typed
    /// counterpart to `dispatch(SessionLoad, ...)`.
    fn session_load<'a>(
        &'a self,
        request: LoadSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<LoadSessionResponse, AcpError>> + Send + 'a>>;

    // --- session/list --------------------------------------------

    /// Enumerates sessions. Typed counterpart to
    /// `dispatch(SessionList, ...)`.
    fn session_list<'a>(
        &'a self,
        request: ListSessionsRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<ListSessionsResponse, AcpError>> + Send + 'a>>;

    // --- session/delete ------------------------------------------

    /// Removes a session. Typed counterpart to
    /// `dispatch(SessionDelete, ...)`.
    fn session_delete<'a>(
        &'a self,
        request: DeleteSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<DeleteSessionResponse, AcpError>> + Send + 'a>>;

    // --- session/resume ------------------------------------------

    /// Resumes a previously-closed session. Typed
    /// counterpart to `dispatch(SessionResume, ...)`.
    fn session_resume<'a>(
        &'a self,
        request: ResumeSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<ResumeSessionResponse, AcpError>> + Send + 'a>>;

    // --- session/close -------------------------------------------

    /// Closes a session without deleting it. Typed
    /// counterpart to `dispatch(SessionClose, ...)`.
    fn session_close<'a>(
        &'a self,
        request: CloseSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<CloseSessionResponse, AcpError>> + Send + 'a>>;

    // --- session/prompt ------------------------------------------

    /// Sends a user turn. Typed counterpart to
    /// `dispatch(SessionPrompt, ...)`.
    fn session_prompt<'a>(
        &'a self,
        request: PromptRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<PromptResponse, AcpError>> + Send + 'a>>;

    // --- session/set_mode ----------------------------------------

    /// Switches the session's mode. Typed counterpart to
    /// `dispatch(SessionSetMode, ...)`.
    fn session_set_mode<'a>(
        &'a self,
        request: SetSessionModeRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<SetSessionModeResponse, AcpError>> + Send + 'a>>;

    // --- session/set_config_option -------------------------------

    /// Sets one session configuration option. Typed
    /// counterpart to
    /// `dispatch(SessionSetConfigOption, ...)`.
    fn session_set_config_option<'a>(
        &'a self,
        request: SetSessionConfigOptionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<SetSessionConfigOptionResponse, AcpError>> + Send + 'a>>;

    // --- logout --------------------------------------------------

    /// Terminates the auth session. Typed
    /// counterpart to `dispatch(Logout, ...)`.
    fn logout<'a>(
        &'a self,
        request: LogoutRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<LogoutResponse, AcpError>> + Send + 'a>>;
}

impl<H: AcpRequestHandler + ?Sized> AcpRequestHandlerExt for H {
    fn initialize<'a>(
        &'a self,
        request: InitializeRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<InitializeResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::Initialize;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise Initialize request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<InitializeResponse>(raw).map_err(|e| AcpError::InternalError {
                message: format!("failed to deserialise Initialize response: {e}"),
            })
        })
    }

    fn authenticate<'a>(
        &'a self,
        request: AuthenticateRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<AuthenticateResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::Authenticate;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise authenticate request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<AuthenticateResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!("failed to deserialise authenticate response: {e}"),
                }
            })
        })
    }

    fn session_new<'a>(
        &'a self,
        request: NewSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<NewSessionResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionNew;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/new request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<NewSessionResponse>(raw).map_err(|e| AcpError::InternalError {
                message: format!("failed to deserialise session/new response: {e}"),
            })
        })
    }

    fn session_load<'a>(
        &'a self,
        request: LoadSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<LoadSessionResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionLoad;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/load request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<LoadSessionResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!("failed to deserialise session/load response: {e}"),
                }
            })
        })
    }

    fn session_list<'a>(
        &'a self,
        request: ListSessionsRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<ListSessionsResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionList;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/list request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<ListSessionsResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!("failed to deserialise session/list response: {e}"),
                }
            })
        })
    }

    fn session_delete<'a>(
        &'a self,
        request: DeleteSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<DeleteSessionResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionDelete;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/delete request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<DeleteSessionResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!("failed to deserialise session/delete response: {e}"),
                }
            })
        })
    }

    fn session_resume<'a>(
        &'a self,
        request: ResumeSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<ResumeSessionResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionResume;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/resume request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<ResumeSessionResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!("failed to deserialise session/resume response: {e}"),
                }
            })
        })
    }

    fn session_close<'a>(
        &'a self,
        request: CloseSessionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<CloseSessionResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionClose;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/close request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<CloseSessionResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!("failed to deserialise session/close response: {e}"),
                }
            })
        })
    }

    fn session_prompt<'a>(
        &'a self,
        request: PromptRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<PromptResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionPrompt;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/prompt request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<PromptResponse>(raw).map_err(|e| AcpError::InternalError {
                message: format!("failed to deserialise session/prompt response: {e}"),
            })
        })
    }

    fn session_set_mode<'a>(
        &'a self,
        request: SetSessionModeRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<SetSessionModeResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::SessionSetMode;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/set_mode request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<SetSessionModeResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!("failed to deserialise session/set_mode response: {e}"),
                }
            })
        })
    }

    fn session_set_config_option<'a>(
        &'a self,
        request: SetSessionConfigOptionRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<SetSessionConfigOptionResponse, AcpError>> + Send + 'a>>
    {
        let method = AgentMethod::SessionSetConfigOption;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise session/set_config_option request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<SetSessionConfigOptionResponse>(raw).map_err(|e| {
                AcpError::InternalError {
                    message: format!(
                        "failed to deserialise session/set_config_option response: {e}"
                    ),
                }
            })
        })
    }

    fn logout<'a>(
        &'a self,
        request: LogoutRequest,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<LogoutResponse, AcpError>> + Send + 'a>> {
        let method = AgentMethod::Logout;
        let value = serde_json::to_value(&request).map_err(|e| AcpError::InternalError {
            message: format!("failed to serialise logout request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<LogoutResponse>(raw).map_err(|e| AcpError::InternalError {
                message: format!("failed to deserialise logout response: {e}"),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    //! The typed wrappers are mostly mechanical
    //! serialise / dispatch / deserialise plumbing
    //! around the single-dispatch trait, so the
    //! tests live with the trait they extend and
    //! focus on a single representative method
    //! (initialize). The conformance runner is
    //! the integration test that exercises every
    //! typed method against a real reference
    //! agent.
    //!
    //! Tests return `Result<(), Failure>` so the
    //! fallible calls can use `?`; the `Failure`
    //! enum covers the wrong-Result shape the
    //! assertions can produce. The project's
    //! `no-unwrap / no-expect / no-panic` rule
    //! applies to test code the same as
    //! production code, so a test that
    //! pattern-matches a `Result` and panics
    //! on the wrong arm is rewritten to
    //! return a typed failure instead.

    use core::future::Future;
    use core::pin::Pin;
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::super::dispatch::{AcpError, AcpRequestContext, AcpRequestHandler};
    use super::AcpRequestHandlerExt;
    use crate::domain::acp::config::SetSessionConfigOptionRequest;
    use crate::domain::acp::implementation::Implementation;
    use crate::domain::acp::initialize::{
        AuthenticateRequest, ClientCapabilities, InitializeRequest,
    };
    use crate::domain::acp::method::AgentMethod;
    use crate::domain::acp::session::{PromptRequest, SessionId, SetSessionModeRequest};

    /// Test-failure shape. Every fallible call in
    /// the tests below propagates through `?`,
    /// turning into a `Failure` via the `From`
    /// impls at the bottom of the module.
    #[derive(Debug)]
    enum Failure {
        /// A typed wrapper returned `Ok` when the
        /// test expected `Err` (or vice versa).
        /// The string identifies which call
        /// produced the unexpected outcome.
        WrongResult(&'static str),
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::WrongResult(label) => write!(formatter, "wrong result: {label}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<AcpError> for Failure {
        fn from(error: AcpError) -> Self {
            Self::WrongResult("acp error").tap_message(|| format!("acp error: {error}"))
        }
    }

    /// `BTreeMap` / `Value` round-trips cannot
    /// fail in a way the tests care about, but
    /// the `From<serde_json::Error>` impl is
    /// here for completeness.
    impl From<serde_json::Error> for Failure {
        fn from(_error: serde_json::Error) -> Self {
            Self::WrongResult("serde_json error")
        }
    }

    /// A small extension trait that lets a
    /// `Failure` carry a one-line context message
    /// without inventing a new variant. Used by
    /// the `From<AcpError>` impl above.
    trait FailureExt {
        fn tap_message(self, f: impl FnOnce() -> String) -> Self;
    }

    impl FailureExt for Failure {
        fn tap_message(self, f: impl FnOnce() -> String) -> Self {
            // Discard the message — the test
            // surfaces the label. The closure
            // exists so the call site is
            // self-documenting.
            let _ = f();
            self
        }
    }

    /// A stub editor. The typed wrappers never call
    /// the editor — the agent-to-editor direction is
    /// the serving adapter's job — so the stub
    /// refuses every call. It exists because the
    /// context carries the editor port, and the
    /// context is what the wrappers hand the
    /// handler.
    struct StubEditor;

    impl super::super::editor::AcpEditorCaller for StubEditor {
        fn request(
            &self,
            _method: crate::domain::acp::method::ClientMethod,
            _params: Value,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Value, super::super::editor::AcpEditorError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async {
                Err(super::super::editor::AcpEditorError::Disconnected {
                    reason: "the stub editor has no connection".to_owned(),
                })
            })
        }

        fn notify(
            &self,
            _method: crate::domain::acp::method::ClientMethod,
            _params: Value,
        ) -> Pin<
            Box<dyn Future<Output = Result<(), super::super::editor::AcpEditorError>> + Send + '_>,
        > {
            Box::pin(async {
                Err(super::super::editor::AcpEditorError::Disconnected {
                    reason: "the stub editor has no connection".to_owned(),
                })
            })
        }
    }

    /// A handler that answers `initialize` with a
    /// hand-built response and `MethodNotImplemented`
    /// for everything else. Used to exercise the
    /// typed wrapper's serialise / dispatch /
    /// deserialise round-trip.
    struct InitializeOnly;

    impl AcpRequestHandler for InitializeOnly {
        fn dispatch<'a>(
            &'a self,
            method: AgentMethod,
            params: Option<Value>,
            _context: &'a AcpRequestContext,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AcpError>> + Send + 'a>> {
            Box::pin(async move {
                match method {
                    AgentMethod::Initialize => {
                        // The handler asserts the
                        // serialise step did produce
                        // a params object. Returning
                        // a typed failure here would
                        // be overkill for a unit
                        // test of a serialise /
                        // dispatch / deserialise
                        // round-trip; the `match`
                        // turns a missing params
                        // into a JSON-RPC
                        // `InvalidParams` error
                        // that the test would
                        // observe as a typed
                        // failure anyway.
                        if params.is_none() {
                            return Err(AcpError::InvalidParams {
                                reason: "initialize requires params".to_owned(),
                            });
                        }
                        Ok(json!({
                            "protocolVersion": 1,
                            "agentInfo": {
                                "name": "test",
                                "title": "Test Agent",
                                "version": "0.1.0"
                            }
                        }))
                    }
                    other => Err(AcpError::MethodNotImplemented { method: other }),
                }
            })
        }
    }

    /// A handler that records the serialised params it
    /// was handed and answers with a minimal valid
    /// response. It exists so the tests below can
    /// assert the *wire* form of a request — a typed
    /// round-trip through `serde_json::Value` can pass
    /// while the JSON object on the wire has the wrong
    /// shape.
    struct EchoParams {
        recorded: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
    }

    impl EchoParams {
        fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Option<Value>>>) {
            let recorded = std::sync::Arc::new(std::sync::Mutex::new(None));
            (
                Self {
                    recorded: std::sync::Arc::clone(&recorded),
                },
                recorded,
            )
        }

        /// Records the params the wrapper handed to
        /// dispatch. The lock guard is released before
        /// this returns — the recorded value outlives
        /// it.
        fn record(&self, params: Option<Value>) -> Result<(), AcpError> {
            {
                let mut guard = self.recorded.lock().map_err(|_| AcpError::InternalError {
                    message: "the recorded-params lock was poisoned".to_owned(),
                })?;
                *guard = params;
            }
            Ok(())
        }
    }

    impl AcpRequestHandler for EchoParams {
        fn dispatch<'a>(
            &'a self,
            method: AgentMethod,
            params: Option<Value>,
            _context: &'a AcpRequestContext,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AcpError>> + Send + 'a>> {
            Box::pin(async move {
                match method {
                    AgentMethod::Authenticate
                    | AgentMethod::SessionSetMode
                    | AgentMethod::SessionSetConfigOption => {
                        self.record(params)?;
                        // Each method needs a response its
                        // typed wrapper can deserialise.
                        Ok(match method {
                            AgentMethod::SessionSetConfigOption => json!({"configOptions": []}),
                            _ => json!({}),
                        })
                    }
                    other => Err(AcpError::MethodNotImplemented { method: other }),
                }
            })
        }
    }

    /// Reads back the params a handler recorded.
    fn recorded_params(
        recorded: &std::sync::Arc<std::sync::Mutex<Option<Value>>>,
    ) -> Result<Value, Failure> {
        let guard = recorded
            .lock()
            .map_err(|_| Failure::WrongResult("the recorded-params lock was poisoned"))?;
        guard
            .clone()
            .ok_or(Failure::WrongResult("the handler recorded no params"))
    }

    fn context() -> AcpRequestContext {
        AcpRequestContext::new(
            crate::domain::acp::protocol_version::AcpProtocolVersion::CURRENT,
            std::sync::Arc::new(StubEditor),
        )
    }

    #[tokio::test]
    async fn initialize_round_trip_typed() -> Result<(), Failure> {
        // The typed wrapper takes a typed
        // request, calls dispatch with the
        // serialised form, and returns a typed
        // response. The hand-built response is
        // shaped to satisfy the v1.25.0
        // schema's required fields.
        let request = InitializeRequest {
            protocol_version: crate::domain::acp::protocol_version::AcpProtocolVersion::CURRENT,
            client_capabilities: ClientCapabilities::default(),
            client_info: Some(Implementation {
                name: "test".to_owned(),
                title: Some("Test".to_owned()),
                version: "0.1.0".to_owned(),
                meta: BTreeMap::new(),
            }),
        };
        let response = InitializeOnly.initialize(request, &context()).await?;
        if response.protocol_version
            != crate::domain::acp::protocol_version::AcpProtocolVersion::CURRENT
        {
            return Err(Failure::WrongResult(
                "initialize: protocol_version mismatch",
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn session_prompt_propagates_method_not_implemented() -> Result<(), Failure> {
        // A handler that does not implement
        // session/prompt surfaces the typed
        // wrapper's error path: the dispatch
        // returns `MethodNotImplemented`, the
        // wrapper propagates it as-is.
        let request = PromptRequest {
            session_id: SessionId::new("s-1"),
            prompt: vec![],
        };
        let response = InitializeOnly.session_prompt(request, &context()).await;
        match response {
            Err(AcpError::MethodNotImplemented {
                method: AgentMethod::SessionPrompt,
            }) => Ok(()),
            Err(_) => Err(Failure::WrongResult("session_prompt: wrong error variant")),
            Ok(_) => Err(Failure::WrongResult(
                "session_prompt: handler unexpectedly succeeded",
            )),
        }
    }

    /// The `authenticate` wrapper puts `methodId` at
    /// the top level of the params object — the shape
    /// the schema's `AuthenticateRequest` describes.
    #[tokio::test]
    async fn authenticate_sends_method_id_at_the_top_level() -> Result<(), Failure> {
        let request = AuthenticateRequest {
            method_id: crate::domain::acp::initialize::AuthMethodId::new("dev-terminal"),
            meta: BTreeMap::new(),
        };
        let (handler, recorded) = EchoParams::new();
        handler.authenticate(request, &context()).await?;
        if recorded_params(&recorded)? != json!({"methodId": "dev-terminal"}) {
            return Err(Failure::WrongResult("authenticate: wrong wire shape"));
        }
        Ok(())
    }

    /// The `session/set_mode` wrapper puts `sessionId`
    /// and `modeId` at the top level of the params
    /// object.
    #[tokio::test]
    async fn session_set_mode_sends_the_session_and_mode() -> Result<(), Failure> {
        let request = SetSessionModeRequest {
            session_id: SessionId::new("s-1"),
            mode_id: crate::domain::acp::session::SessionModeId::new("plan"),
            meta: BTreeMap::new(),
        };
        let (handler, recorded) = EchoParams::new();
        handler.session_set_mode(request, &context()).await?;
        if recorded_params(&recorded)? != json!({"sessionId": "s-1", "modeId": "plan"}) {
            return Err(Failure::WrongResult("session_set_mode: wrong wire shape"));
        }
        Ok(())
    }

    /// The `session/set_config_option` wrapper puts
    /// `sessionId`, `configId` and the value at the top
    /// level of the params object. The value-id form
    /// carries no `type` discriminator — the schema's
    /// "the simpler form is the default" rule.
    #[tokio::test]
    async fn session_set_config_option_sends_the_value_flat() -> Result<(), Failure> {
        let request = SetSessionConfigOptionRequest {
            session_id: SessionId::new("s-1"),
            config_id: crate::domain::acp::config::SessionConfigId::new("model"),
            value: crate::domain::acp::config::SessionConfigOptionValue::ValueId {
                value: crate::domain::acp::config::SessionConfigValueId::new("opus"),
            },
            meta: BTreeMap::new(),
        };
        let (handler, recorded) = EchoParams::new();
        handler
            .session_set_config_option(request, &context())
            .await?;
        if recorded_params(&recorded)?
            != json!({"sessionId": "s-1", "configId": "model", "value": "opus"})
        {
            return Err(Failure::WrongResult(
                "session_set_config_option: wrong wire shape",
            ));
        }
        Ok(())
    }

    /// The boolean form of the config-option value
    /// carries the `type` discriminator alongside the
    /// value.
    #[tokio::test]
    async fn session_set_config_option_sends_a_boolean_with_its_type() -> Result<(), Failure> {
        let request = SetSessionConfigOptionRequest {
            session_id: SessionId::new("s-1"),
            config_id: crate::domain::acp::config::SessionConfigId::new("brave"),
            value: crate::domain::acp::config::SessionConfigOptionValue::Boolean { value: true },
            meta: BTreeMap::new(),
        };
        let (handler, recorded) = EchoParams::new();
        handler
            .session_set_config_option(request, &context())
            .await?;
        if recorded_params(&recorded)?
            != json!({
                "sessionId": "s-1",
                "configId": "brave",
                "type": "boolean",
                "value": true
            })
        {
            return Err(Failure::WrongResult(
                "session_set_config_option: wrong boolean wire shape",
            ));
        }
        Ok(())
    }
}
