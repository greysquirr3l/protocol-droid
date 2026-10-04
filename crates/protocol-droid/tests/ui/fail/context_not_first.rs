//! A `RequestContext` that is not the first parameter.
//!
//! The generated handler is given the context before the decoded arguments,
//! so the context is written first. Moving it after an argument reads fine —
//! it is an ordinary parameter, and the context is the one parameter the
//! expansion drops from the schema — so the position is the kind of thing a
//! user moves without thinking.
//!
//! The path is spelled in full, as in the pass case beside it, so the only
//! diagnostic pinned here is the macro's own. A `use` for the context would
//! go unused — the macro refuses the signature before the type is ever
//! resolved — and the `unused_imports` warning that follows would be pinned
//! alongside it, which is noise in a file whose whole job is to show one
//! message.

/// A server whose tool takes the context last.
struct Session;

#[protocol_droid::server(name = "session")]
impl Session {
    /// Shouts the text back, having written the context after it.
    #[protocol_droid::tool]
    async fn shout(
        text: String,
        ctx: protocol_droid::application::server::context::RequestContext,
    ) -> Result<String, String> {
        let _ = ctx;
        Ok(text.to_uppercase())
    }
}

fn main() {
    let _ = Session;
}
