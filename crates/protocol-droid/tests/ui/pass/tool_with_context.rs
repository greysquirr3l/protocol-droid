//! A `#[tool]` that takes a leading [`RequestContext`].
//!
//! The context is how a handler reads what the session negotiated — the
//! features it may use, the peer it is talking to — so it is the one parameter
//! the expansion does not turn into a schema entry. It comes first, because
//! the generated closure is handed the context before the decoded arguments.
//!
//! The path is spelled in full on purpose: the expansion matches the last
//! segment, so the long form, `crate::RequestContext` and the bare name are
//! one parameter, and a user should not need a `use` to write one.

/// A server whose one tool reads the session.
struct Session;

#[protocol_droid::server(name = "session")]
impl Session {
    /// Shouts the text back, after reading the context.
    #[protocol_droid::tool]
    async fn shout(
        ctx: protocol_droid::application::server::context::RequestContext,
        text: String,
    ) -> Result<String, String> {
        let _ = ctx;
        Ok(text.to_uppercase())
    }
}

fn main() {
    let _ = Session.into_server();
}
