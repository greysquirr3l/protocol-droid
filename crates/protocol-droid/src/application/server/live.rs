//! What a server can still change once it is serving, and who hears about it.
//!
//! # Why this is separate from the builder
//!
//! The builder describes a server before anything is connected to it. This
//! module is the part that outlives that moment: the handler table a request
//! looks up, the registry `*/list` reads, and the sessions that asked to be
//! told when either changes. All three move together, which is why they live
//! behind one [`Arc`] and why a [`ServerHandle`] is a clone of that `Arc`
//! rather than a borrow of the [`Server`](super::Server).
//!
//! # The ordering that keeps a listing honest
//!
//! A tool that is listed and not callable is a server telling a peer something
//! false. So an addition installs the handler *before* it publishes the
//! listing, and a removal withdraws the listing *before* it drops the handler.
//! In between, a request can find a handler for something not yet listed —
//! harmless, because nobody was told it exists — but never the reverse.
//!
//! # Why a change is announced after it is published
//!
//! The notification tells a peer to re-list. A peer that re-lists before the
//! publish lands reads the old set and is never told again, so the announcement
//! follows the publish, and only for kinds the publish actually touched — a
//! removal of something that was never there announces nothing.

use std::sync::{Arc, Mutex, PoisonError, Weak};

use arc_swap::ArcSwap;

use crate::application::registry::store::{Published, RegistryStore};
use crate::application::registry::{Fanout, Subscriber, SubscriberSet};
use crate::application::server::builder::{PromptHandler, ResourceHandler, ToolHandler};
use crate::application::server::watch::Watchers;
use crate::application::session::DynFrameSink;
use crate::domain::registry::{
    Primitive, PrimitiveId, PrimitiveKind, Prompt, Registered, Resource, ResourceTemplate, Tool,
};
use crate::domain::{Feature, FeatureProfile};

/// A tool's name and the closure that runs it.
///
/// Handwritten `Debug`: the handler is a boxed closure, and the derived form
/// would print a vtable pointer nobody reading a log line can use.
#[derive(Clone)]
pub(crate) struct ToolEntry {
    pub(crate) name: PrimitiveId,
    pub(crate) handler: ToolHandler,
}

impl core::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Every handler a request can reach, as one immutable value.
///
/// Replaced whole on every change rather than edited in place, for the reason
/// the registry is: a request that loaded this table keeps a consistent view
/// for as long as it runs, however many changes land meanwhile.
#[derive(Clone, Default)]
pub(crate) struct Handlers {
    pub(crate) tools: Vec<ToolEntry>,
    pub(crate) resources: Vec<(PrimitiveId, ResourceHandler)>,
    pub(crate) prompts: Vec<(PrimitiveId, PromptHandler)>,
}

/// The shared, mutable half of a server.
pub(crate) struct Live {
    registry: RegistryStore,
    handlers: ArcSwap<Handlers>,
    subscribers: SubscriberSet,
    watchers: Watchers,
    /// Serialises writers, so a handler swap and the publish that goes with it
    /// are never interleaved with another writer's pair.
    ///
    /// Readers never take it: both halves are `ArcSwap`, and a lookup is a
    /// load. The lock is held across two synchronous swaps and never across an
    /// `.await`, so it cannot stall a request.
    writer: Mutex<()>,
}

impl Live {
    pub(crate) fn new(registry: RegistryStore, handlers: Handlers) -> Self {
        Self {
            registry,
            handlers: ArcSwap::from_pointee(handlers),
            subscribers: SubscriberSet::new(),
            watchers: Watchers::default(),
            writer: Mutex::new(()),
        }
    }

    pub(crate) const fn registry(&self) -> &RegistryStore {
        &self.registry
    }

    pub(crate) fn tool_count(&self) -> usize {
        self.handlers.load().tools.len()
    }

    pub(crate) fn tool(&self, name: &str) -> Option<ToolHandler> {
        self.handlers
            .load()
            .tools
            .iter()
            .find(|entry| entry.name.as_str() == name)
            .map(|entry| Arc::clone(&entry.handler))
    }

    pub(crate) fn reader(&self, uri: &str) -> Option<ResourceHandler> {
        self.handlers
            .load()
            .resources
            .iter()
            .find(|(registered, _)| registered.as_str() == uri)
            .map(|(_, handler)| Arc::clone(handler))
    }

    pub(crate) fn getter(&self, name: &str) -> Option<PromptHandler> {
        self.handlers
            .load()
            .prompts
            .iter()
            .find(|(registered, _)| registered.as_str() == name)
            .map(|(_, handler)| Arc::clone(handler))
    }

    fn swap_handlers(&self, edit: impl FnOnce(&mut Handlers)) {
        let mut next = Handlers::clone(&self.handlers.load());
        edit(&mut next);
        self.handlers.store(Arc::new(next));
    }

    /// Installs a handler, then lists what it serves.
    pub(crate) fn add(
        &self,
        registered: Registered,
        edit: impl FnOnce(&mut Handlers),
    ) -> Published {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        self.swap_handlers(edit);
        self.registry.register(registered)
    }

    /// Withdraws a listing, then drops its handler.
    pub(crate) fn remove(
        &self,
        kind: PrimitiveKind,
        id: &PrimitiveId,
        edit: impl FnOnce(&mut Handlers),
    ) -> Published {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let published = self.registry.unregister(kind, id);
        self.swap_handlers(edit);
        published
    }

    async fn announce(&self, published: &Published) -> Fanout {
        self.subscribers
            .announce(published.snapshot(), published.changed())
            .await
    }
}

/// The list notifications a session can ask for.
///
/// Three, not four: resource templates are announced under
/// `notifications/resources/list_changed`, because the specification files
/// them under the `resources` capability and gives them no notification of
/// their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    /// `notifications/tools/list_changed`.
    Tools,
    /// `notifications/resources/list_changed`, for resources and templates.
    Resources,
    /// `notifications/prompts/list_changed`.
    Prompts,
}

impl ListKind {
    /// All three, for a session that negotiated whatever its profile allows.
    pub const ALL: [Self; 3] = [Self::Tools, Self::Resources, Self::Prompts];

    /// The notification method.
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::Tools => "notifications/tools/list_changed",
            Self::Resources => "notifications/resources/list_changed",
            Self::Prompts => "notifications/prompts/list_changed",
        }
    }

    /// The feature a session's profile must hold to be told.
    #[must_use]
    pub const fn feature(self) -> Feature {
        match self {
            Self::Tools => Feature::ToolsListChanged,
            Self::Resources => Feature::ResourcesListChanged,
            Self::Prompts => Feature::PromptsListChanged,
        }
    }

    fn subscriber(
        self,
        frame: &str,
        profile: FeatureProfile,
        sink: Arc<dyn DynFrameSink>,
    ) -> Subscriber {
        let primary = match self {
            Self::Tools => PrimitiveKind::Tool,
            Self::Resources => PrimitiveKind::Resource,
            Self::Prompts => PrimitiveKind::Prompt,
        };
        let subscriber =
            Subscriber::new(primary, self.feature(), self.method(), frame, profile, sink);
        if self == Self::Resources {
            subscriber.also_for(PrimitiveKind::ResourceTemplate)
        } else {
            subscriber
        }
    }

    /// The notification, optionally carrying a `_meta` object.
    ///
    /// `_meta` is how a `2026-07-28` subscription tags its frames with the
    /// subscription they belong to; every other caller passes `None` and gets
    /// the bare notification every revision defines.
    fn frame(self, meta: Option<&serde_json::Value>) -> String {
        let mut frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": self.method(),
        });
        if let Some(meta) = meta
            && let Some(object) = frame.as_object_mut()
        {
            object.insert("params".to_owned(), serde_json::json!({ "_meta": meta }));
        }
        frame.to_string()
    }
}

/// A session's standing interest in list changes, withdrawn on drop.
///
/// A guard rather than an explicit unsubscribe, because the moments a session
/// ends — a dropped connection, an expired id, a client that closed its
/// stream — are moments no handler is running to remember to call anything.
/// Holding this in the session's own state makes "the session is gone" and
/// "the session is no longer told" the same event.
pub struct ListSubscription {
    live: Weak<Live>,
    sink: Arc<dyn DynFrameSink>,
}

impl core::fmt::Debug for ListSubscription {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ListSubscription").finish_non_exhaustive()
    }
}

impl Drop for ListSubscription {
    fn drop(&mut self) {
        if let Some(live) = self.live.upgrade() {
            live.subscribers.remove_by_sink(&self.sink);
            live.watchers.forget(&self.sink);
        }
    }
}

/// A clone-able handle that changes a server while it serves.
///
/// What a [`Server`](super::Server) hands out from
/// [`Server::handle`](super::Server::handle). It is cheap to clone and `Send +
/// Sync`, so it can be stored in a server's own state and reached from inside a
/// tool: a tool that installs another tool is the shape a plugin loader takes.
///
/// Every change publishes the registry and then tells each session whose
/// negotiated profile includes the matching `listChanged` capability. A session
/// that never negotiated it hears nothing, so a change is never a message the
/// peer has no contract for.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::server::Server;
/// use protocol_droid::domain::ir::JsonObject;
/// use protocol_droid::domain::registry::Tool;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let server = Server::builder("plugins").build();
/// let handle = server.handle();
///
/// let fanout = handle
///     .add_tool(
///         Tool::new("late", r#"{"type":"object"}"#)?,
///         Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() })),
///     )
///     .await;
/// assert!(fanout.is_silent(), "nobody is connected yet");
/// assert_eq!(server.tool_count(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ServerHandle {
    live: Arc<Live>,
}

impl core::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("tool_count", &self.live.tool_count())
            .finish_non_exhaustive()
    }
}

impl ServerHandle {
    pub(crate) const fn new(live: Arc<Live>) -> Self {
        Self { live }
    }

    /// Adds or replaces a tool, and announces it.
    pub async fn add_tool(&self, tool: Tool, handler: ToolHandler) -> Fanout {
        self.add_restricted_tool(Registered::from(Primitive::from(tool)), handler)
            .await
    }

    /// Adds or replaces a tool carrying its own visibility requirements.
    ///
    /// A `Registered` that is not a tool is listed and given no handler, which
    /// is the builder's rule for the same input.
    pub async fn add_restricted_tool(
        &self,
        registered: Registered,
        handler: ToolHandler,
    ) -> Fanout {
        let name = match registered.primitive() {
            Primitive::Tool(tool) => Some(tool.name.clone()),
            _ => None,
        };
        let published = self.live.add(registered, |table| {
            if let Some(name) = name {
                table.tools.retain(|entry| entry.name != name);
                table.tools.push(ToolEntry { name, handler });
            }
        });
        self.live.announce(&published).await
    }

    /// Removes a tool, and announces it if there was one.
    pub async fn remove_tool(&self, name: &str) -> Fanout {
        let id = PrimitiveId::from(name);
        let published = self.live.remove(PrimitiveKind::Tool, &id, |table| {
            table.tools.retain(|entry| entry.name != id);
        });
        self.live.announce(&published).await
    }

    /// Adds or replaces a resource with its reader, and announces it.
    pub async fn add_resource(&self, resource: Resource, reader: ResourceHandler) -> Fanout {
        let uri = resource.uri.clone();
        let published = self
            .live
            .add(Registered::from(Primitive::from(resource)), |table| {
                table.resources.retain(|(registered, _)| *registered != uri);
                table.resources.push((uri, reader));
            });
        self.live.announce(&published).await
    }

    /// Removes a resource, and announces it if there was one.
    pub async fn remove_resource(&self, uri: &str) -> Fanout {
        let id = PrimitiveId::from(uri);
        let published = self.live.remove(PrimitiveKind::Resource, &id, |table| {
            table.resources.retain(|(registered, _)| *registered != id);
        });
        self.live.announce(&published).await
    }

    /// Adds or replaces a resource template, and announces it.
    pub async fn add_resource_template(&self, template: ResourceTemplate) -> Fanout {
        let published = self
            .live
            .add(Registered::from(Primitive::from(template)), |_| {});
        self.live.announce(&published).await
    }

    /// Removes a resource template by its URI template, and announces it.
    pub async fn remove_resource_template(&self, uri_template: &str) -> Fanout {
        let id = PrimitiveId::from(uri_template);
        let published = self
            .live
            .remove(PrimitiveKind::ResourceTemplate, &id, |_| {});
        self.live.announce(&published).await
    }

    /// Adds or replaces a prompt with its getter, and announces it.
    pub async fn add_prompt(&self, prompt: Prompt, getter: PromptHandler) -> Fanout {
        let name = prompt.name.clone();
        let published = self
            .live
            .add(Registered::from(Primitive::from(prompt)), |table| {
                table.prompts.retain(|(registered, _)| *registered != name);
                table.prompts.push((name, getter));
            });
        self.live.announce(&published).await
    }

    /// Removes a prompt, and announces it if there was one.
    pub async fn remove_prompt(&self, name: &str) -> Fanout {
        let id = PrimitiveId::from(name);
        let published = self.live.remove(PrimitiveKind::Prompt, &id, |table| {
            table.prompts.retain(|(registered, _)| *registered != id);
        });
        self.live.announce(&published).await
    }

    /// Subscribes `sink` to the list notifications in `kinds`.
    ///
    /// For a transport, once per session: the session's negotiated `profile`
    /// gates each notification, and `meta` — when present — is carried as the
    /// notification's `params._meta`. The subscription lasts until the
    /// returned guard is dropped.
    #[must_use]
    pub fn subscribe(
        &self,
        sink: &Arc<dyn DynFrameSink>,
        profile: FeatureProfile,
        kinds: &[ListKind],
        meta: Option<&serde_json::Value>,
    ) -> ListSubscription {
        for kind in kinds {
            self.live.subscribers.add(kind.subscriber(
                &kind.frame(meta),
                profile,
                Arc::clone(sink),
            ));
        }
        ListSubscription {
            live: Arc::downgrade(&self.live),
            sink: Arc::clone(sink),
        }
    }

    /// Tells every session subscribed to `uri` that the resource changed.
    ///
    /// `notifications/resources/updated`, sent to each session that asked for
    /// `uri` — through `resources/subscribe` before `2026-07-28`, or through
    /// `resourceSubscriptions` on a listen stream from it on. A resource nobody
    /// subscribed to tells nobody.
    pub async fn notify_resource_updated(&self, uri: &str) -> Fanout {
        self.live.watchers.notify(uri).await
    }

    /// Adds `uri` to what `sink`'s session is told about.
    ///
    /// The watch ends with the session's [`ListSubscription`], or with
    /// [`Self::unwatch_resource`].
    pub fn watch_resource(
        &self,
        sink: &Arc<dyn DynFrameSink>,
        uri: &str,
        meta: Option<&serde_json::Value>,
    ) {
        self.live.watchers.watch(sink, uri, meta);
    }

    /// Stops telling `sink`'s session about `uri`.
    pub fn unwatch_resource(&self, sink: &Arc<dyn DynFrameSink>, uri: &str) {
        self.live.watchers.unwatch(sink, uri);
    }

    /// How many sessions watch at least one resource.
    #[must_use]
    pub fn watcher_count(&self) -> usize {
        self.live.watchers.len()
    }

    /// Whether a primitive of `kind` is listed under `id` right now.
    ///
    /// A read of the live snapshot, so it can be stale the moment it returns
    /// if another handle is changing the server concurrently — fine for a
    /// toggle, wrong for anything that needs the answer to still hold.
    #[must_use]
    pub fn is_listed(&self, kind: PrimitiveKind, id: &str) -> bool {
        self.live
            .registry
            .snapshot()
            .contains(kind, &PrimitiveId::from(id))
    }

    /// How many sessions are currently subscribed, counting one per kind.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.live.subscribers.len()
    }
}

#[cfg(test)]
mod tests;
