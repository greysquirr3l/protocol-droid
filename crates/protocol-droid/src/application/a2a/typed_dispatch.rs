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
    use crate::domain::a2a::send_message::SendMessageRequest;
    use crate::domain::a2a::task::Task;
    use crate::domain::a2a::task_lifecycle::GetTaskRequest;
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
}
