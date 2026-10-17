//! A whole MCP server written with the four registration macros, and the
//! first thing to copy when starting a new one.
//!
//! Run it with `cargo run --example server`. It builds the server and
//! prints the registry the macros filled in — including each tool's
//! `inputSchema`, which nobody wrote: the schema is derived from the handler's
//! signature, so the listing printed here is the listing a peer would get.
//!
//! # The four macros, and what each one is
//!
//! `#[server]` goes on the **impl block**. It is the only one that generates
//! anything: it reads the marks below, and adds one method beside the block,
//! `into_server`, which registers everything in source order.
//!
//! `#[tool]`, `#[resource]` and `#[prompt]` are marks. Each expands *inside*
//! the impl block, where Rust forbids declaring items, so they generate
//! nothing on their own and a mark outside a `#[server]` block is inert.
//!
//! # The two things that surprise people
//!
//! A `#[tool]` handler is `async` — including the ones here, which await
//! nothing. The `async` is what makes the call the generated code makes
//! produce a future; without it the macro's own `.await` would be awaiting a
//! plain value. Every other `#[tool]` you will write should await something.
//!
//! A `#[resource]` or `#[prompt]` method is a plain `fn`. Both are registered
//! once, while the server is being built, so there is nothing to await — and
//! an `async fn` here would hand `into_server` a future where a `Resource`
//! belongs.

#![expect(
    clippy::unused_async,
    reason = "the generated handler awaits the call, and a plain value has nothing to await"
)]
#![expect(
    clippy::unused_async_trait_impl,
    reason = "the generated handler awaits the call, and a plain value has nothing to await"
)]

use protocol_droid::application::server::context::RequestContext;
use protocol_droid::domain::ir::{
    Content, JsonObject, JsonObjectError, PromptMessage, ResourceContents, Role, TextContent,
};
use protocol_droid::domain::registry::{Primitive, PrimitiveKind, Prompt, Resource};
use protocol_droid::serde::Serialize;
use protocol_droid::serde_json;

/// What `shout` answers with.
#[derive(Serialize)]
struct Shout {
    /// The text, uppercased.
    text: String,
    /// How many characters went in.
    length: usize,
}

/// A scratchpad: a server with one tool, one resource and one prompt.
struct Scratchpad;

/// The three marks, in one block, registered by one `into_server`.
///
/// All three in one `impl` rather than one each, because that is the shape
/// that has to work: three separate blocks would never notice a registration
/// that read the wrong builder method.
#[protocol_droid::server(name = "scratchpad")]
impl Scratchpad {
    /// Shouts the text back, with its length.
    ///
    /// The doc comment is what the tool's `description` becomes, and each
    /// parameter becomes a property in the `inputSchema`. Returning a
    /// `Result` is the fallible path: the error is rendered into the tool
    /// result a peer reads rather than propagated to the transport.
    #[protocol_droid::tool]
    async fn shout(text: String) -> Result<Shout, String> {
        let length = text.chars().count();
        Ok(Shout {
            text: text.to_uppercase(),
            length,
        })
    }

    /// Reports the protocol revision this session negotiated.
    ///
    /// A leading `RequestContext` is the way a handler reads the session, and
    /// it is the one parameter that does not become a schema property. This
    /// handler returns a plain value rather than a `Result` — the infallible
    /// path, which needs no error type.
    #[protocol_droid::tool]
    async fn negotiated(ctx: RequestContext) -> String {
        ctx.peer().negotiated().to_string()
    }

    /// The scratchpad's own readme.
    ///
    /// A `#[resource]` is a value, and the value carries its own description
    /// and mime type, so those are set in the body rather than in the
    /// attribute. The body is ordinary Rust, so anything computable — a
    /// config value, a `match` — is available here.
    #[protocol_droid::resource(reader = read_readme)]
    fn readme() -> Resource {
        Resource {
            description: Some("How the scratchpad is meant to be used.".to_string()),
            mime_type: Some("text/markdown".to_string()),
            ..Resource::new("file:///scratchpad/README.md", "readme")
        }
    }

    /// Asks for a digest of the notes written since a date.
    ///
    /// A prompt's arguments are an ordered list with a required flag each, and
    /// a comma-separated attribute list cannot express that without a second
    /// parallel spelling of every name. They are declared in the body instead.
    #[protocol_droid::prompt(getter = get_digest)]
    fn digest() -> Prompt {
        let mut prompt = Prompt::new("digest");
        prompt.description = Some("Summarise the notes written since a date.".to_string());
        prompt.with_argument("since", true)
    }

    /// Serves the readme's contents.
    ///
    /// Named by `reader = ..` on the attribute above rather than inferred, and
    /// separate from it because the two answer different methods: the listing
    /// is read by `resources/list`, this by `resources/read`. A reader took
    /// the URI from the listing it was registered with it would be answering a
    /// different question from the one the peer asked.
    ///
    /// The URI is a parameter rather than a captured value so the reader sees
    /// the URI the peer actually sent.
    fn read_readme(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        vec![ResourceContents::text(uri, "# Scratchpad\n\nNotes go in `notes/`.\n")
            .with_mime_type("text/markdown")]
    }

    /// Builds the digest prompt's messages.
    ///
    /// Takes the arguments the peer sent, so a template can be filled in here
    /// rather than by the macro guessing which of the prompt's declared
    /// arguments a given one used.
    fn get_digest(_context: RequestContext, arguments: &JsonObject) -> Vec<PromptMessage> {
        // `JsonObject` holds a validated document rather than a map, so a
        // lookup is a parse. Prompt arguments are read once per request, which
        // is not a path worth optimising.
        let parsed: serde_json::Value = match serde_json::from_str(arguments.as_str()) {
            Ok(parsed) => parsed,
            Err(_) => return Vec::new(),
        };
        let since = parsed
            .get("since")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("yesterday");
        vec![PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new(format!("Summarise the notes written since {since}."))),
        )]
    }
}

/// Builds the server and prints what the macros registered.
///
/// Walks the registry rather than the handler list, because the registry is
/// what a peer sees: `tools/list`, `resources/list` and `prompts/list` are
/// answered from it, so anything printed here is something a peer would get.
fn main() -> Result<(), JsonObjectError> {
    let server = Scratchpad.into_server()?;

    println!("server: {}", server.name());
    println!("tools:  {}", server.tool_count());

    let snapshot = server.registry().snapshot();
    for kind in PrimitiveKind::ALL {
        for entry in snapshot.list(kind) {
            println!("\n  {}: {}", kind.as_str(), entry.id());
            if let Primitive::Tool(tool) = entry.primitive() {
                if let Some(description) = &tool.description {
                    println!("    description: {description}");
                }
                println!("    inputSchema: {}", tool.input_schema.as_str());
            }
        }
    }

    Ok(())
}
