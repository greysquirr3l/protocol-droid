//! One `#[server]` block mixing all three marks.
//!
//! The three registrations run in one `into_server`, in source order, against
//! one builder. A case that used each mark in its own impl would not notice a
//! macro that mixed them up — a resource falling through to the tool path is
//! exactly the bug that registers it twice and never derives a schema, and it
//! only shows when a tool and a resource sit in the same block.

/// What the tool answers with.
#[derive(protocol_droid::serde::Serialize)]
struct Sum {
    /// The total, which a peer reads out of `structuredContent`.
    total: i32,
}

/// A server offering a tool, a resource and a prompt.
struct Kitchen;

#[protocol_droid::server(name = "kitchen")]
impl Kitchen {
    /// Adds two numbers.
    #[protocol_droid::tool]
    async fn add(a: i32, b: i32) -> Result<Sum, String> {
        Ok(Sum { total: a + b })
    }

    /// Adds two numbers, with the session in hand.
    #[protocol_droid::tool]
    async fn add_and_log(
        ctx: protocol_droid::application::server::context::RequestContext,
        a: i32,
        b: i32,
    ) -> Result<Sum, String> {
        let _ = ctx;
        Ok(Sum { total: a + b })
    }

    /// The project's readme.
    #[protocol_droid::resource(reader = read_readme)]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }

    /// Asks for a review of a diff.
    #[protocol_droid::prompt(getter = get_review)]
    fn review() -> protocol_droid::domain::registry::Prompt {
        protocol_droid::domain::registry::Prompt::new("review")
    }

    /// Serves the readme, by the URI the peer named.
    fn read_readme(
        _context: protocol_droid::application::server::context::RequestContext,
        _uri: &str,
    ) -> Vec<protocol_droid::domain::ir::ResourceContents> {
        Vec::new()
    }

    /// Serves the review prompt's messages.
    fn get_review(
        _context: protocol_droid::application::server::context::RequestContext,
        _arguments: &protocol_droid::domain::ir::JsonObject,
    ) -> Vec<protocol_droid::domain::ir::PromptMessage> {
        Vec::new()
    }
}

fn main() {
    let _ = Kitchen.into_server();
}
