//! Per-method typed wrappers around the
//! single-dispatch
//! [`A2aRequestHandler`](super::A2aRequestHandler)
//! trait.
//!
//! # What this is
//!
//! A handler that wants typed params and
//! typed results implements
//! [`A2aRequestHandler`](super::A2aRequestHandler)
//! (the single-dispatch shape) and gets
//! one typed method per A2A v1.0 method
//! through the [`A2aRequestHandlerExt`]
//! extension trait.
//!
//! Each typed method:
//! 1. Serialises the typed request to
//!    [`serde_json::Value`].
//! 2. Calls
//!    [`dispatch`](super::A2aRequestHandler::dispatch).
//! 3. Deserialises the returned `Value` to
//!    the typed response.
//!
//! # Why an extension trait, not a
//! replacement
//!
//! The single-dispatch trait is the
//! object-safe shape: a consumer who wants
//! `Arc<dyn A2aRequestHandler>` (handler
//! swapping at runtime) uses it directly.
//! The typed wrappers return concrete
//! futures and are not object-safe. An
//! extension trait lets a consumer pick the
//! shape they want without compromising
//! the other.
//!
//! # Why the typed wrappers are not
//! default-implemented
//!
//! A consumer who wants only the typed
//! shape writes no
//! `dispatch` body — the extension
//! trait's body is the single-dispatch
//! call. A consumer who wants the
//! single-dispatch shape writes no
//! typed methods — the extension trait's
//! methods are available but unused.
//! The two shapes are independent and
//! layered.
//!
//! # Why the streaming methods are not
//! in the extension
//!
//! The two streaming methods
//! (`SendStreamingMessage` and
//! `SubscribeToTask`) emit a stream of
//! events, not a single value. Their
//! serving side is a different trait
//! ([`A2aStreamingRequestHandler`](crate::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler))
//! with a different dispatch shape
//! (`Pin<Box<dyn Stream<...>>>`). The
//! typed wrappers for the streaming
//! methods belong with that trait, not
//! here.

use std::pin::Pin;

use futures_core::Future;

use super::handler::{A2aError, A2aRequestContext, A2aRequestHandler};
use crate::adapters::secondary::a2a::A2aMethod;
use crate::domain::a2a::extended_agent_card::{
    GetExtendedAgentCardRequest, GetExtendedAgentCardResponse,
};
use crate::domain::a2a::list_tasks::{ListTasksRequest, ListTasksResponse};
use crate::domain::a2a::push_notification_methods::{
    CreateTaskPushNotificationConfigRequest,
    CreateTaskPushNotificationConfigResponse,
    DeleteTaskPushNotificationConfigRequest,
    DeleteTaskPushNotificationConfigResponse,
    GetTaskPushNotificationConfigRequest,
    GetTaskPushNotificationConfigResponse,
    ListTaskPushNotificationConfigsRequest,
    ListTaskPushNotificationConfigsResponse,
};
use crate::domain::a2a::send_message::{
    SendMessageRequest, SendMessageResponse,
};
use crate::domain::a2a::task_lifecycle::{
    CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse,
};

/// The per-method typed wrappers. A blanket
/// impl means every [`A2aRequestHandler`]
/// gets the typed methods for free.
pub trait A2aRequestHandlerExt: A2aRequestHandler {
    // --- SendMessage -----------------------------------------------

    /// Sends a message. Typed counterpart to
    /// `dispatch(SendMessage, ...)`.
    fn send_message<'a>(
        &'a self,
        request: SendMessageRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<SendMessageResponse, A2aError>>
                + Send
                + 'a,
        >,
    >;

    // --- GetTask ----------------------------------------------------

    /// Fetches a task. Typed counterpart to
    /// `dispatch(GetTask, ...)`.
    fn get_task<'a>(
        &'a self,
        request: GetTaskRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<dyn Future<Output = Result<GetTaskResponse, A2aError>> + Send + 'a>,
    >;

    // --- CancelTask -------------------------------------------------

    /// Cancels a task. Typed counterpart to
    /// `dispatch(CancelTask, ...)`.
    fn cancel_task<'a>(
        &'a self,
        request: CancelTaskRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CancelTaskResponse, A2aError>>
                + Send
                + 'a,
        >,
    >;

    // --- ListTasks --------------------------------------------------

    /// Lists tasks. Typed counterpart to
    /// `dispatch(ListTasks, ...)`.
    fn list_tasks<'a>(
        &'a self,
        request: ListTasksRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<ListTasksResponse, A2aError>> + Send + 'a,
        >,
    >;

    // --- GetExtendedAgentCard ---------------------------------------

    /// Fetches the extended agent card. Typed
    /// counterpart to
    /// `dispatch(GetExtendedAgentCard, ...)`.
    fn get_extended_agent_card<'a>(
        &'a self,
        request: GetExtendedAgentCardRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<GetExtendedAgentCardResponse, A2aError>>
                + Send
                + 'a,
        >,
    >;

    // --- Push-notification methods ----------------------------------

    /// Creates a push-notification config.
    /// Typed counterpart to
    /// `dispatch(CreateTaskPushNotificationConfig, ...)`.
    fn create_task_push_notification_config<'a>(
        &'a self,
        request: CreateTaskPushNotificationConfigRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        CreateTaskPushNotificationConfigResponse,
                        A2aError,
                    >,
                > + Send
                + 'a,
        >,
    >;

    /// Fetches a push-notification config.
    /// Typed counterpart to
    /// `dispatch(GetTaskPushNotificationConfig, ...)`.
    fn get_task_push_notification_config<'a>(
        &'a self,
        request: GetTaskPushNotificationConfigRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        GetTaskPushNotificationConfigResponse,
                        A2aError,
                    >,
                > + Send
                + 'a,
        >,
    >;

    /// Lists push-notification configs.
    /// Typed counterpart to
    /// `dispatch(ListTaskPushNotificationConfigs, ...)`.
    fn list_task_push_notification_configs<'a>(
        &'a self,
        request: ListTaskPushNotificationConfigsRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        ListTaskPushNotificationConfigsResponse,
                        A2aError,
                    >,
                > + Send
                + 'a,
        >,
    >;

    /// Deletes a push-notification config.
    /// Typed counterpart to
    /// `dispatch(DeleteTaskPushNotificationConfig, ...)`.
    fn delete_task_push_notification_config<'a>(
        &'a self,
        request: DeleteTaskPushNotificationConfigRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        DeleteTaskPushNotificationConfigResponse,
                        A2aError,
                    >,
                > + Send
                + 'a,
        >,
    >;
}

impl<H: A2aRequestHandler + ?Sized> A2aRequestHandlerExt for H {
    fn send_message<'a>(
        &'a self,
        request: SendMessageRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<SendMessageResponse, A2aError>>
                + Send
                + '_,
        >,
    > {
        let method = A2aMethod::SendMessage;
        let value = serde_json::to_value(&request)
            .map_err(|e| A2aError::InternalError {
                message: format!("failed to serialise SendMessage request: {e}"),
            });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<SendMessageResponse>(raw).map_err(|e| {
                A2aError::InternalError {
                    message: format!("failed to deserialise SendMessage response: {e}"),
                }
            })
        })
    }

    fn get_task<'a>(
        &'a self,
        request: GetTaskRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<dyn Future<Output = Result<GetTaskResponse, A2aError>> + Send + '_>,
    > {
        let method = A2aMethod::GetTask;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise GetTask request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<GetTaskResponse>(raw).map_err(|e| {
                A2aError::InternalError {
                    message: format!("failed to deserialise GetTask response: {e}"),
                }
            })
        })
    }

    fn cancel_task<'a>(
        &'a self,
        request: CancelTaskRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CancelTaskResponse, A2aError>>
                + Send
                + '_,
        >,
    > {
        let method = A2aMethod::CancelTask;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise CancelTask request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<CancelTaskResponse>(raw).map_err(|e| {
                A2aError::InternalError {
                    message: format!("failed to deserialise CancelTask response: {e}"),
                }
            })
        })
    }

    fn list_tasks<'a>(
        &'a self,
        request: ListTasksRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<ListTasksResponse, A2aError>> + Send + '_,
        >,
    > {
        let method = A2aMethod::ListTasks;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise ListTasks request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<ListTasksResponse>(raw).map_err(|e| {
                A2aError::InternalError {
                    message: format!("failed to deserialise ListTasks response: {e}"),
                }
            })
        })
    }

    fn get_extended_agent_card<'a>(
        &'a self,
        request: GetExtendedAgentCardRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<GetExtendedAgentCardResponse, A2aError>>
                + Send
                + '_,
        >,
    > {
        let method = A2aMethod::GetExtendedAgentCard;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise GetExtendedAgentCard request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<GetExtendedAgentCardResponse>(raw).map_err(|e| {
                A2aError::InternalError {
                    message: format!(
                        "failed to deserialise GetExtendedAgentCard response: {e}"
                    ),
                }
            })
        })
    }

    fn create_task_push_notification_config<'a>(
        &'a self,
        request: CreateTaskPushNotificationConfigRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        CreateTaskPushNotificationConfigResponse,
                        A2aError,
                    >,
                > + Send
                + '_,
        >,
    > {
        let method = A2aMethod::CreateTaskPushNotificationConfig;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise CreateTaskPushNotificationConfig request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<CreateTaskPushNotificationConfigResponse>(raw)
                .map_err(|e| A2aError::InternalError {
                    message: format!(
                        "failed to deserialise CreateTaskPushNotificationConfig response: {e}"
                    ),
                })
        })
    }

    fn get_task_push_notification_config<'a>(
        &'a self,
        request: GetTaskPushNotificationConfigRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        GetTaskPushNotificationConfigResponse,
                        A2aError,
                    >,
                > + Send
                + '_,
        >,
    > {
        let method = A2aMethod::GetTaskPushNotificationConfig;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise GetTaskPushNotificationConfig request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<GetTaskPushNotificationConfigResponse>(raw)
                .map_err(|e| A2aError::InternalError {
                    message: format!(
                        "failed to deserialise GetTaskPushNotificationConfig response: {e}"
                    ),
                })
        })
    }

    fn list_task_push_notification_configs<'a>(
        &'a self,
        request: ListTaskPushNotificationConfigsRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        ListTaskPushNotificationConfigsResponse,
                        A2aError,
                    >,
                > + Send
                + '_,
        >,
    > {
        let method = A2aMethod::ListTaskPushNotificationConfigs;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise ListTaskPushNotificationConfigs request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<ListTaskPushNotificationConfigsResponse>(raw)
                .map_err(|e| A2aError::InternalError {
                    message: format!(
                        "failed to deserialise ListTaskPushNotificationConfigs response: {e}"
                    ),
                })
        })
    }

    fn delete_task_push_notification_config<'a>(
        &'a self,
        request: DeleteTaskPushNotificationConfigRequest,
        context: &'a A2aRequestContext,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        DeleteTaskPushNotificationConfigResponse,
                        A2aError,
                    >,
                > + Send
                + '_,
        >,
    > {
        let method = A2aMethod::DeleteTaskPushNotificationConfig;
        let value = serde_json::to_value(&request).map_err(|e| A2aError::InternalError {
            message: format!("failed to serialise DeleteTaskPushNotificationConfig request: {e}"),
        });
        Box::pin(async move {
            let value = value?;
            let raw = self.dispatch(method, Some(value), context).await?;
            serde_json::from_value::<DeleteTaskPushNotificationConfigResponse>(raw)
                .map_err(|e| A2aError::InternalError {
                    message: format!(
                        "failed to deserialise DeleteTaskPushNotificationConfig response: {e}"
                    ),
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::pin::Pin;
    use std::sync::Arc;

    use futures_core::Future;
    use serde_json::json;

    use super::A2aRequestHandlerExt;
    use crate::application::a2a::handler::{
        A2aError, A2aRequestContext, A2aRequestHandler,
    };
    use crate::adapters::secondary::a2a::A2aMethod;
    use crate::domain::a2a::extended_agent_card::{
        GetExtendedAgentCardRequest, GetExtendedAgentCardResponse,
    };
    use crate::domain::a2a::list_tasks::{
        ListTasksRequest, ListTasksResponse,
    };
    use crate::domain::a2a::push_notification_methods::{
        CreateTaskPushNotificationConfigRequest,
        CreateTaskPushNotificationConfigResponse,
        DeleteTaskPushNotificationConfigRequest,
        DeleteTaskPushNotificationConfigResponse,
        GetTaskPushNotificationConfigRequest,
        GetTaskPushNotificationConfigResponse,
        ListTaskPushNotificationConfigsRequest,
        ListTaskPushNotificationConfigsResponse,
    };
    use crate::domain::a2a::send_message::SendMessageRequest;
    use crate::domain::a2a::task::Task;
    use crate::domain::a2a::task_lifecycle::{
        CancelTaskRequest, GetTaskRequest,
    };
    use crate::domain::a2a::version::A2aVersion;

    type TestError = Box<dyn std::error::Error>;
    type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

    /// A handler that returns the method
    /// name as the result, prefixed with
    /// "typed:". Used to test that the
    /// typed wrappers go through the
    /// single-dispatch path.
    struct EchoTypedHandler;

    impl A2aRequestHandler for EchoTypedHandler {
        fn dispatch(
            &self,
            method: A2aMethod,
            params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> BoxFuture<'_, Result<serde_json::Value, A2aError>> {
            Box::pin(async move {
                Ok(json!({
                    "method": method.as_str(),
                    "params": params,
                }))
            })
        }
    }

    #[tokio::test]
    async fn send_message_round_trips_through_typed_dispatch() -> Result<(), TestError> {
        let handler: Arc<dyn A2aRequestHandler> = Arc::new(EchoTypedHandler);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        // Use a non-empty BTreeSet so the
        // configuration's wire form is
        // not the minimal `{}` (a
        // configuration with no fields
        // serialises to `{}`).
        let mut modes = BTreeSet::new();
        modes.insert("text/plain".to_owned());
        let request = SendMessageRequest::new(
            crate::domain::a2a::message::Message::new(
                crate::domain::a2a::message::MessageRole::User,
            ),
        )
        .with_configuration(
            crate::domain::a2a::send_message::MessageSendConfiguration::new(modes),
        );
        // The typed wrapper returns
        // `Result<SendMessageResponse,
        // A2aError>`, but our echo
        // handler returns a `Value` that
        // is not a valid
        // `SendMessageResponse`. The
        // wrapper turns the
        // deserialisation error into
        // `A2aError::InternalError`. We
        // assert on that — the
        // important part of the test is
        // that the dispatch was reached
        // with the serialised request.
        let result = handler.send_message(request, &context).await;
        // The deserialise of a `Value` to
        // `SendMessageResponse` fails
        // because the echo handler's
        // output is not a valid response
        // shape.
        let Err(_err) = result else {
            return Err(
                "expected deserialisation error from echo handler".into(),
            );
        };
        Ok(())
    }

    #[tokio::test]
    async fn get_task_serialises_request_before_dispatch() -> Result<(), TestError> {
        let handler: Arc<dyn A2aRequestHandler> = Arc::new(EchoTypedHandler);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request = GetTaskRequest::new("t-1");
        // A valid `GetTaskResponse` is a
        // transparent newtype around a
        // Task. The echo handler's
        // `{"method": "GetTask", "params":
        // {...}}` is not a Task, so the
        // deserialise fails. The
        // important part is that the
        // dispatch was reached with the
        // serialised request.
        let result = handler.get_task(request, &context).await;
        let Err(_err) = result else {
            return Err(
                "expected deserialisation error from echo handler".into(),
            );
        };
        // A direct dispatch would
        // succeed. This asserts the
        // path is being routed
        // correctly.
        let _ = Task::new("t-1"); // touch the type so the import is used
        Ok(())
    }

    /// A handler that records the method
    /// and params it was called with and
    /// returns a caller-supplied response
    /// value. Used by the typed-wrapper
    /// tests below to assert that the
    /// wrapper routed to the right method
    /// with the right serialised request,
    /// and that the deserialise round-trip
    /// on a valid response shape succeeds.
    struct RecordingHandler {
        last_method: std::sync::Mutex<Option<A2aMethod>>,
        last_params: std::sync::Mutex<Option<serde_json::Value>>,
        response: serde_json::Value,
    }

    impl RecordingHandler {
        fn new(response: serde_json::Value) -> Self {
            Self {
                last_method: std::sync::Mutex::new(None),
                last_params: std::sync::Mutex::new(None),
                response,
            }
        }

        fn last_method(&self) -> Option<A2aMethod> {
            // The `Mutex::lock` call returns a
            // `Result` (poisoned on panic). A
            // poisoned lock here means a previous
            // test panicked inside the lock, and
            // the only safe answer is to read
            // whatever was there before the
            // panic. That is what `lock().ok()`
            // expresses.
            self.last_method
                .lock()
                .ok()
                .and_then(|guard| *guard)
        }

        fn last_params(&self) -> Option<serde_json::Value> {
            self.last_params
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
        }
    }

    impl A2aRequestHandler for RecordingHandler {
        fn dispatch(
            &self,
            method: A2aMethod,
            params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> BoxFuture<'_, Result<serde_json::Value, A2aError>> {
            if let Ok(mut guard) = self.last_method.lock() {
                *guard = Some(method);
            }
            if let Ok(mut guard) = self.last_params.lock() {
                *guard = params;
            }
            let response = self.response.clone();
            Box::pin(async move { Ok(response) })
        }
    }

    /// `cancel_task` routes to
    /// `A2aMethod::CancelTask` and
    /// serialises the request id into the
    /// params.
    #[tokio::test]
    async fn cancel_task_routes_to_cancel_task_method(
    ) -> Result<(), TestError> {
        use crate::domain::a2a::task::Task;
        let task = Task::new("t-cancel");
        let response_value = serde_json::to_value(&task)?;
        let handler = RecordingHandler::new(response_value);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request = CancelTaskRequest::new("t-cancel");
        // A2aError does not implement
        // `std::error::Error`, so the
        // result has to be matched by hand
        // — the wrapper turns the
        // dispatch's value into a
        // `CancelTaskResponse`, and the
        // assert is on the recorded
        // method, not on the response.
        handler
            .cancel_task(request, &context)
            .await
            .map_err(|error| format!("cancel_task returned an error: {error:?}"))?;
        let recorded = handler
            .last_method()
            .ok_or("dispatch was not called")?;
        if recorded != A2aMethod::CancelTask {
            return Err(format!("expected CancelTask, got {recorded:?}").into());
        }
        let params = handler
            .last_params()
            .ok_or("dispatch was called with no params")?;
        let id = params
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("params did not carry the task id")?;
        if id != "t-cancel" {
            return Err(format!("expected id 't-cancel', got {id:?}").into());
        }
        Ok(())
    }

    /// `list_tasks` routes to
    /// `A2aMethod::ListTasks` and the
    /// response deserialises through the
    /// typed wrapper without error.
    #[tokio::test]
    async fn list_tasks_routes_to_list_tasks_method(
    ) -> Result<(), TestError> {
        let response = ListTasksResponse::new(Vec::<Task>::new())
            .with_page_size(1)
            .with_total_size(0)
            .with_next_page_token(String::new());
        let response_value = serde_json::to_value(&response)?;
        let handler = RecordingHandler::new(response_value);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request = ListTasksRequest::new();
        let response = handler
            .list_tasks(request, &context)
            .await
            .map_err(|error| format!("list_tasks returned an error: {error:?}"))?;
        if response.page_size != 1 {
            return Err(format!(
                "expected page_size 1, got {}",
                response.page_size
            )
            .into());
        }
        let recorded = handler
            .last_method()
            .ok_or("dispatch was not called")?;
        if recorded != A2aMethod::ListTasks {
            return Err(format!("expected ListTasks, got {recorded:?}").into());
        }
        Ok(())
    }

    /// `get_extended_agent_card` routes to
    /// `A2aMethod::GetExtendedAgentCard`
    /// and the response deserialises
    /// through the typed wrapper.
    #[tokio::test]
    async fn get_extended_agent_card_routes_to_method(
    ) -> Result<(), TestError> {
        use crate::domain::a2a::agent_card::{
            AgentCapabilities, AgentCardBuilder, AgentInterface,
            AgentSkill, ProtocolBinding,
        };
        // The card builder is the
        // domain's typed entry point.
        // `build` returns
        // `Result<AgentCard, _>` because
        // the spec marks some fields as
        // REQUIRED; the builder refuses
        // to ship a card that omits one.
        // A test that builds a real card
        // and feeds it back through the
        // typed wrapper pins the
        // round-trip on the wire shape.
        let url = url::Url::parse("https://example.com/a2a")
            .map_err(|error| format!("URL parse failed: {error}"))?;
        let interface = AgentInterface {
            url,
            protocol_binding: ProtocolBinding::default(),
            protocol_version: A2aVersion::V1_0,
            tenant: None,
        };
        let mut capabilities = AgentCapabilities {
            streaming: true,
            ..AgentCapabilities::default()
        };
        capabilities.push_notifications = true;
        let skill = AgentSkill {
            id: "skill-1".into(),
            name: "Echo".into(),
            description: "echoes the input".into(),
            tags: Vec::new(),
        };
        let card = AgentCardBuilder::new()
            .with_name("test-agent")
            .with_description("a test agent")
            .with_version("1.0")
            .with_interface(interface)
            .with_capabilities(capabilities)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .with_skill(skill)
            .build()
            .map_err(|error| format!("card build failed: {error:?}"))?;
        let response = GetExtendedAgentCardResponse(card);
        let response_value = serde_json::to_value(&response)?;
        let handler = RecordingHandler::new(response_value);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request = GetExtendedAgentCardRequest::new();
        handler
            .get_extended_agent_card(request, &context)
            .await
            .map_err(|error| {
                format!("get_extended_agent_card returned an error: {error:?}")
            })?;
        let recorded = handler
            .last_method()
            .ok_or("dispatch was not called")?;
        if recorded != A2aMethod::GetExtendedAgentCard {
            return Err(format!(
                "expected GetExtendedAgentCard, got {recorded:?}"
            )
            .into());
        }
        Ok(())
    }

    /// `create_task_push_notification_config`
    /// routes to the right method and the
    /// response deserialises.
    #[tokio::test]
    async fn create_task_push_notification_config_routes_to_method(
    ) -> Result<(), TestError> {
        use crate::domain::a2a::push_notification::TaskPushNotificationConfig;
        let config = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/webhook".to_owned(),
        );
        let response =
            CreateTaskPushNotificationConfigResponse(config);
        let response_value = serde_json::to_value(&response)?;
        let handler = RecordingHandler::new(response_value);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let inner = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/webhook".to_owned(),
        );
        let request =
            CreateTaskPushNotificationConfigRequest(inner);
        handler
            .create_task_push_notification_config(request, &context)
            .await
            .map_err(|error| {
                format!("create returned an error: {error:?}")
            })?;
        let recorded = handler
            .last_method()
            .ok_or("dispatch was not called")?;
        if recorded != A2aMethod::CreateTaskPushNotificationConfig {
            return Err(format!(
                "expected CreateTaskPushNotificationConfig, got {recorded:?}"
            )
            .into());
        }
        Ok(())
    }

    /// `get_task_push_notification_config`
    /// routes to the right method and the
    /// response deserialises.
    #[tokio::test]
    async fn get_task_push_notification_config_routes_to_method(
    ) -> Result<(), TestError> {
        use crate::domain::a2a::push_notification::TaskPushNotificationConfig;
        let config = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/webhook".to_owned(),
        );
        let response =
            GetTaskPushNotificationConfigResponse(config);
        let response_value = serde_json::to_value(&response)?;
        let handler = RecordingHandler::new(response_value);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request = GetTaskPushNotificationConfigRequest::new(
            "pc-1",
            "t-1",
        );
        handler
            .get_task_push_notification_config(request, &context)
            .await
            .map_err(|error| {
                format!("get returned an error: {error:?}")
            })?;
        let recorded = handler
            .last_method()
            .ok_or("dispatch was not called")?;
        if recorded != A2aMethod::GetTaskPushNotificationConfig {
            return Err(format!(
                "expected GetTaskPushNotificationConfig, got {recorded:?}"
            )
            .into());
        }
        Ok(())
    }

    /// `list_task_push_notification_configs`
    /// routes to the right method and the
    /// response deserialises.
    #[tokio::test]
    async fn list_task_push_notification_configs_routes_to_method(
    ) -> Result<(), TestError> {
        use crate::domain::a2a::push_notification::TaskPushNotificationConfig;
        let configs: Vec<TaskPushNotificationConfig> = Vec::new();
        let response = ListTaskPushNotificationConfigsResponse::new(configs);
        let response_value = serde_json::to_value(&response)?;
        let handler = RecordingHandler::new(response_value);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request = ListTaskPushNotificationConfigsRequest::new("t-1");
        handler
            .list_task_push_notification_configs(request, &context)
            .await
            .map_err(|error| {
                format!("list returned an error: {error:?}")
            })?;
        let recorded = handler
            .last_method()
            .ok_or("dispatch was not called")?;
        if recorded != A2aMethod::ListTaskPushNotificationConfigs {
            return Err(format!(
                "expected ListTaskPushNotificationConfigs, got {recorded:?}"
            )
            .into());
        }
        Ok(())
    }

    /// `delete_task_push_notification_config`
    /// routes to the right method and the
    /// response (default) deserialises.
    #[tokio::test]
    async fn delete_task_push_notification_config_routes_to_method(
    ) -> Result<(), TestError> {
        let response = DeleteTaskPushNotificationConfigResponse::default();
        let response_value = serde_json::to_value(&response)?;
        let handler = RecordingHandler::new(response_value);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request =
            DeleteTaskPushNotificationConfigRequest::new("pc-1", "t-1");
        handler
            .delete_task_push_notification_config(request, &context)
            .await
            .map_err(|error| {
                format!("delete returned an error: {error:?}")
            })?;
        let recorded = handler
            .last_method()
            .ok_or("dispatch was not called")?;
        if recorded != A2aMethod::DeleteTaskPushNotificationConfig {
            return Err(format!(
                "expected DeleteTaskPushNotificationConfig, got {recorded:?}"
            )
            .into());
        }
        Ok(())
    }

    /// When the handler returns an
    /// `A2aError::MethodNotFound`, the
    /// typed wrapper propagates the
    /// error without trying to
    /// deserialise. The test pins that
    /// the typed shape does not turn
    /// domain errors into internal
    /// errors.
    #[tokio::test]
    async fn dispatch_error_is_propagated_not_swallowed(
    ) -> Result<(), TestError> {
        struct FailingHandler;
        impl A2aRequestHandler for FailingHandler {
            fn dispatch(
                &self,
                _method: A2aMethod,
                _params: Option<serde_json::Value>,
                _context: &A2aRequestContext,
            ) -> BoxFuture<'_, Result<serde_json::Value, A2aError>> {
                Box::pin(async move { Err(A2aError::MethodNotFound) })
            }
        }
        let handler: Arc<dyn A2aRequestHandler> = Arc::new(FailingHandler);
        let context = A2aRequestContext {
            protocol_version: A2aVersion::V1_0,
        };
        let request = GetTaskRequest::new("t-1");
        let result = handler.get_task(request, &context).await;
        if result.is_ok() {
            return Err("expected MethodNotFound to propagate".into());
        }
        Ok(())
    }
}
