//! A2A domain — the version negotiation model, the per-revision
//! `A2aFeatureProfile`, and the value objects that the wire
//! codecs (T28) lower into.
//!
//! # Where this fits in the protocol-neutral core
//!
//! The protocol-neutral core in `domain/jsonrpc` and
//! `application/session` already speaks JSON-RPC 2.0 — the same
//! envelope A2A uses. T27 is a *feature* on top of that core: a
//! per-revision codec (T08-style) and a per-revision
//! `A2aFeatureProfile` (T05-style) that mirrors the MCP gating
//! model. A future MCP↔A2A bridge (T28) is the consumer of both
//! profiles; this module is what the bridge translates *between*.
//!
//! # The seam
//!
//! `A2aVersion` is the only thing this module re-exports at the
//! top level. The `A2aFeatureProfile` lives in `a2a::feature` and
//! the per-revision wire types live under `a2a::wire::v1_0`. A
//! codec for v1.0 implements the protocol-neutral
//! `application/jsonrpc::Codec` trait so the dispatcher routes
//! the same way it does for MCP.

pub mod agent_card;
pub mod extended_agent_card;
pub mod feature;
pub mod list_tasks;
pub mod message;
pub mod push_notification;
pub mod push_notification_methods;
pub mod send_message;
pub mod task;
pub mod task_lifecycle;
pub mod version;
pub mod well_known;

#[cfg(test)]
mod tests;

pub use agent_card::{
    AgentCapabilities, AgentCard, AgentCardBuilder, AgentCardError, AgentInterface,
    AgentSkill, ProtocolBinding, GRPC, HTTP_JSON, JSONRPC,
};
pub use extended_agent_card::{
    GetExtendedAgentCardRequest, GetExtendedAgentCardResponse,
};
pub use list_tasks::{ListTasksRequest, ListTasksResponse};
pub use message::{DataPart, FileContent, FilePart, Message, MessageRole, Part, TextPart};
pub use push_notification::{
    PushNotificationAuthenticationInfo, TaskPushNotificationConfig,
};
pub use push_notification_methods::{
    CreateTaskPushNotificationConfigRequest, CreateTaskPushNotificationConfigResponse,
    DeleteTaskPushNotificationConfigRequest, DeleteTaskPushNotificationConfigResponse,
    GetTaskPushNotificationConfigRequest, GetTaskPushNotificationConfigResponse,
    ListTaskPushNotificationConfigsRequest, ListTaskPushNotificationConfigsResponse,
};
pub use send_message::{MessageSendConfiguration, SendMessageRequest, SendMessageResponse};
pub use task::{Artifact, Task, TaskState, TaskStatus};
pub use task_lifecycle::{
    CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse,
};
pub use version::{A2aVersion, ParseA2aVersionError};
pub use well_known::{AGENT_CARD_WELL_KNOWN_PATH, is_agent_card_well_known_path};

