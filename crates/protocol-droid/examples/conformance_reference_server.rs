//! The reference MCP server the conformance suite exercises.
//!
//! # What this is
//!
//! One runnable, fully-formed MCP server exposing a representative slice of
//! what the official `@modelcontextprotocol/conformance` suite's
//! server-scenarios want from a peer: a `ping`, a couple of `tools`,
//! `prompts` with descriptions and arguments, and `resources`. The names,
//! descriptions, and shapes are the ones a conforming client reads back.
//!
//! The server is intentionally **not** the project's own example server.
//! `stdio_server.rs` proves a tiny server is enough to round-trip a
//! handshake; this one proves the rest of the spec the conformance suite
//! reads is also wired. The two examples would diverge the moment either
//! shape changes, and that divergence is the test failure mode the
//! conformance suite catches.
//!
//! # What this is not
//!
//! Not a profile. The suite runs every scenario for the chosen revision
//! against this server, and a profile that hides a feature behind a
//! downgrade does the suite no favours — it would be the project's own
//! server that fails the matrix leg, not the feature. This server exposes
//! everything the `#[server]` macro supports today, and the
//! `conformance self` command points the suite at it.
//!
//! # Running
//!
//! ```text
//! cargo run --example conformance_reference_server -- --addr 127.0.0.1:8080
//! ```
//!
//! `stdout` carries nothing — this server runs on Streamable HTTP, where
//! stdout is not the wire. The conformance suite connects to
//! `http://<addr>/mcp`, runs every server scenario for the chosen
//! revision, and reports its findings through the wrapper.

use std::net::SocketAddr;

// `#[tool]` is called once per request rather than once per server, and
// the function is `async fn` because the call site `.await`s it. A body
// that did no real work would still hand back a future, and the
// compiler would warn about an unused-async body. `tokio::task::yield_now`
// is the one-byte future the lint accepts: it costs one task queue
// turn and changes nothing else, so a fixture whose tools are hot-paths
// in a benchmark does not regress here.
async fn yield_to_runtime() {
    tokio::task::yield_now().await;
}

use protocol_droid::application::server::Server;
use protocol_droid::application::server::context::RequestContext;
use protocol_droid::domain::ir::{
    AudioContent, Content, EmbeddedResource, ImageContent, JsonObject, LogLevel, PromptMessage,
    ResourceContents, Role, TextContent, ToolResult,
};
use protocol_droid::domain::registry::{Prompt, PromptArgument, Resource};
use protocol_droid::serde::Serialize;
use protocol_droid::serde_json;

/// A 1x1 transparent PNG, base64-encoded.
///
/// A real image rather than a placeholder string, because the suite decodes
/// what the tool returns and an image block carrying `"aGk="` decodes to `hi`,
/// which is not a PNG and would fail the scenario for a reason that has
/// nothing to do with the shape under test.
const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// A 44-byte silent WAV header, base64-encoded.
///
/// The audio counterpart to [`PNG_1X1`]: an `audio/wav` block whose payload is
/// not a WAV is a block that decodes cleanly and is still the wrong content,
/// and the difference is invisible until something parses it.
const WAV_SILENT: &str = "UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=";

/// The name this server reports in its `initialize` reply.
///
/// Surfaced here rather than passed in, because a fixture server's identity
/// is part of its contract with the conformance suite: the suite reads it
/// from the wire and asserts on it, and a fixture whose name is whatever
/// happened to be in `argv` would not be a fixture any more. The literal
/// is also inlined into the `#[server(name = "...")]` attribute, where the
/// macro only accepts string literals — the constant and the literal are
/// the same fact, and keeping them in one block is the rule.
const SERVER_NAME: &str = "conformance-reference-server";

/// A tool result for `echo` and `add`, with a small structured payload.
///
/// `protocol-droid`'s `Result<T, E>` types are typed on the wire: `Ok(T)`
/// produces a `content` array of text/image entries plus a typed
/// `structuredContent`, `Err(E)` produces an `isError: true` reply with
/// the error's `Display`. The conformance suite's tools scenario asserts
/// the `isError` flag and the `content` shape, and a type that is a
/// string is enough for the two tools the suite needs.
#[derive(Serialize)]
struct EchoResult {
    /// The text the tool returned, verbatim.
    text: String,
    /// How many characters the input had — a counter proves the handler
    /// ran rather than short-circuited on an empty input.
    length: usize,
}

/// A tool result for `add`, with the same shape `EchoResult` uses.
///
/// Two concrete result types rather than one generic one because the
/// conformance suite's `tools/call` scenario checks that a structured
/// result's fields are *the* fields the tool described in its schema —
/// and a generic `Value` would let a regression in field naming slip
/// through the schema check.
#[derive(Serialize)]
struct AddResult {
    /// The sum, which a peer reads out of `structuredContent`.
    total: i64,
}

/// The reference server, in one `#[server]` block.
///
/// `into_server` registers the three marks against one builder, in source
/// order, and the test for the macro is that they all appear in
/// `tools/list`, `prompts/list`, and `resources/list` afterwards. A
/// registration that compiles but never lands in the registry would be
/// a passing test that exercises nothing, and the conformance suite
/// would not see the server as having a `prompts/list` reply at all.
struct ConformanceFixture;

#[protocol_droid::server(name = "conformance-reference-server")]
impl ConformanceFixture {
    /// Uppercases `text` and reports its length.
    ///
    /// The pair is the smallest one that proves a tool with a required
    /// string argument round-trips both its schema (the string is
    /// `required: ["text"]`) and its result (the JSON has a `text` and a
    /// `length` field). The conformance suite's tools scenario asserts
    /// both, and one tool that returns `42` would not.
    #[protocol_droid::tool]
    async fn echo(text: String) -> Result<EchoResult, String> {
        yield_to_runtime().await;
        Ok(EchoResult {
            length: text.chars().count(),
            text: text.to_uppercase(),
        })
    }

    /// Adds two integers and reports the sum.
    ///
    /// Two-number tool so the `tools/call` scenario has at least one
    /// tool whose arguments are typed integers. The conformance suite
    /// does not assert the *value* of the sum — it asserts the schema
    /// and that `isError` is `false` for a well-formed call.
    #[protocol_droid::tool]
    async fn add(a: i64, b: i64) -> Result<AddResult, String> {
        yield_to_runtime().await;
        Ok(AddResult { total: a + b })
    }

    /// The smallest conforming result: one text block and nothing else.
    ///
    /// Separate from `echo` on purpose. `echo` returns a structured object, so
    /// its answer carries *both* `structuredContent` and a text block, and a
    /// scenario asserting "the content array is exactly one plain text block"
    /// cannot tell that answer from a tool with no structured output. This one
    /// is the only shape with nothing to hide behind.
    #[protocol_droid::tool]
    async fn test_simple_text() -> ToolResult {
        yield_to_runtime().await;
        ToolResult::text("This is a simple text response.")
    }

    /// An image block, which is the first content type `success` could not
    /// produce at all.
    #[protocol_droid::tool]
    async fn test_image_content() -> ToolResult {
        yield_to_runtime().await;
        ToolResult::new(vec![Content::Image(ImageContent::new(
            PNG_1X1,
            "image/png",
        ))])
    }

    /// An audio block. `2025-03-26` and later — at `2024-11-05` this tool's
    /// answer is refused rather than degraded, which is the behaviour the
    /// revision gap is meant to expose.
    #[protocol_droid::tool]
    async fn test_audio_content() -> ToolResult {
        yield_to_runtime().await;
        ToolResult::new(vec![Content::Audio(AudioContent::new(
            WAV_SILENT,
            "audio/wav",
        ))])
    }

    /// A resource inline in the result, rather than linked to.
    ///
    /// The inline form rather than a `ResourceLink` because they are different
    /// blocks that arrived in different revisions, and a scenario reading one
    /// while the fixture answers with the other reports a shape mismatch that
    /// reads as a codec bug.
    ///
    /// `mime_type` is set explicitly. `EmbeddedResource::text` leaves it
    /// `None`, which is correct for the general case — the spec makes it
    /// optional — but the suite's `tools-call-embedded-resource` scenario
    /// asserts it is present and fails an embedded block without one. The
    /// failure is reported as `No resource content found` rather than
    /// "mimeType missing", because the suite finds the block by `type` and
    /// then checks its members in sequence, aborting on the first failure:
    /// a block with no `mimeType` and a block that was never found produce
    /// the same headline error, and the first is the far likelier cause of a
    /// scenario that otherwise looks right on the wire.
    #[protocol_droid::tool]
    async fn test_embedded_resource() -> ToolResult {
        yield_to_runtime().await;
        let mut embedded = EmbeddedResource::text(
            "test://embedded-resource",
            "This is an embedded resource's text.",
        );
        embedded.mime_type = Some("text/plain".to_owned());
        ToolResult::new(vec![Content::EmbeddedResource(embedded)])
    }

    /// `test_logging_tool` / `test_tool_with_logging` — sends the three log
    /// notifications the suite counts.
    ///
    /// The scenario needs at least three `notifications/message` records
    /// emitted *during* the call, and it reads them off a client handler that
    /// is registered before the tool runs. It sets the level to `debug` first,
    /// so these pass the gate this server applies — `setLoggingLevel` records
    /// nothing, so the level is whatever the session negotiated, and an
    /// `info` notification is emitted at the default.
    #[protocol_droid::tool]
    async fn test_tool_with_logging(ctx: RequestContext) -> ToolResult {
        yield_to_runtime().await;
        for message in [
            "Tool execution started",
            "Tool processing data",
            "Tool execution completed",
        ] {
            log_message(&ctx, message).await;
        }
        ToolResult::text("Logged three messages.")
    }

    /// `test_tool_with_progress` — emits progress notifications for the token
    /// the caller supplied in `_meta.progressToken`.
    ///
    /// `report_progress` is what makes the token observable: without a
    /// progress token on the call there is nothing to report against, and the
    /// helper declines rather than inventing a token. The scenario sends
    /// `progress-test-1`, so this returns the increments it asks for.
    #[protocol_droid::tool]
    async fn test_tool_with_progress(ctx: RequestContext) -> ToolResult {
        yield_to_runtime().await;
        for (progress, total) in [(0, 3), (1, 3), (2, 3)] {
            progress_message(&ctx, progress, total).await;
        }
        ToolResult::text("Reported progress.")
    }

    /// `test_sampling` — asks the peer to sample from an LLM.
    ///
    /// The scenario registers a handler on its *client* and fails unless the
    /// server issues `sampling/createMessage`. The answer this server gets
    /// back is whatever the peer chose; the tool reports the prompt it sent
    /// and whether the exchange completed, so a refusal is visible as a
    /// refusal rather than as a tool that quietly said nothing happened.
    #[protocol_droid::tool]
    async fn test_sampling(ctx: RequestContext, prompt: String) -> ToolResult {
        yield_to_runtime().await;
        // The request is built before the call so a bad prompt is reported as a
        // refusal rather than sent to the peer as malformed JSON: the
        // scenario's client answers whatever it is handed, so an unserialisable
        // request would otherwise surface as a confusing peer-side error
        // instead of this tool's own.
        let Some(request) = sampling_request(&prompt) else {
            return ToolResult::text("The sampling request could not be built.");
        };
        match ctx.sample(&request).await {
            Ok(_) => ToolResult::text("Sampling completed."),
            // The reason is reported rather than discarded: a client that
            // declines the request and a server whose gate refused it are the
            // same `Err` here, and the suite only tells them apart by whether
            // it saw a `sampling/createMessage` at all. Naming the cause in the
            // result is what makes the two distinguishable from the outside.
            Err(failure) => ToolResult::text(format!("Sampling was not completed: {failure}")),
        }
    }

    /// `test_elicitation` — asks the peer to collect input from its user.
    #[protocol_droid::tool]
    async fn test_elicitation(ctx: RequestContext, message: String) -> ToolResult {
        yield_to_runtime().await;
        let Some(request) = elicitation_request(&message) else {
            return ToolResult::text("The elicitation request could not be built.");
        };
        match ctx.elicit(&request).await {
            Ok(_) => ToolResult::text("Elicitation completed."),
            Err(failure) => ToolResult::text(format!("Elicitation was not completed: {failure}")),
        }
    }

    /// `test_error_handling`, which the suite's `tools-call-error` scenario
    /// calls to check the error envelope.
    ///
    /// This one is not a naming gap. Every other failure this fixture had was
    /// "the suite asked for a name the server had never registered", which
    /// reads as a missing entry. This scenario found the entry and then failed
    /// on `Tool did not return isError: true` — so it is a real behaviour
    /// question: a tool that fails is still a *successful* JSON-RPC response
    /// carrying `isError: true` and content explaining what went wrong, and a
    /// server that answers it with a JSON-RPC error object instead is
    /// answering a different question.
    #[protocol_droid::tool]
    async fn test_error_handling() -> ToolResult {
        yield_to_runtime().await;
        ToolResult::text("This tool always returns an error.").error()
    }

    /// Several content types in one answer, which is the case that catches a
    /// result builder that only ever holds one block.
    /// Text first: a client that concatenates the blocks in order has to be
    /// able to start with one it can read, and an image-first answer forces
    /// every one of them to handle binary before it has seen any text.
    #[protocol_droid::tool]
    async fn test_multiple_content_types() -> ToolResult {
        yield_to_runtime().await;
        // The embedded block carries `mime_type` for the same reason the
        // single-block tool does: `tools-call-mixed-content` asserts a
        // `resource` block with a `mimeType`, and reports a block missing one
        // as `Missing resource content`.
        let mut embedded = EmbeddedResource::text(
            "test://embedded-resource",
            "This is an embedded resource's text.",
        );
        embedded.mime_type = Some("text/plain".to_owned());
        ToolResult::new(vec![
            Content::Text(protocol_droid::domain::ir::TextContent::new(
                "This is a multi-type response.",
            )),
            Content::Image(ImageContent::new(PNG_1X1, "image/png")),
            Content::EmbeddedResource(embedded),
        ])
    }

    /// `test_prompt_with_embedded_resource`, which
    /// `prompts-get-embedded-resource` calls with `resourceUri`.
    ///
    /// The message embeds the URI it was given rather than a fixed one, so a
    /// getter that ignored the argument would answer with a resource the peer
    /// never asked about — and the scenario, which looks for *any* embedded
    /// resource, would pass anyway. The value is echoed into the prompt text as
    /// well so the substitution is visible without decoding the block.
    #[protocol_droid::prompt(getter = get_test_prompt_with_embedded_resource)]
    fn test_prompt_with_embedded_resource() -> Prompt {
        Prompt {
            description: Some("A prompt whose message embeds a resource.".to_owned()),
            arguments: vec![PromptArgument {
                name: "resourceUri".to_owned(),
                description: Some("The URI of the resource to embed.".to_owned()),
                title: None,
                required: true,
            }],
            ..Prompt::new("test_prompt_with_embedded_resource")
        }
    }

    /// `test_prompt_with_image`, which `prompts-get-with-image` calls.
    ///
    /// Carries no arguments, and its message is an image block — the prompt
    /// analogue of the `tools-call-image` content shape, and the only place the
    /// suite checks that a *prompt* can return binary content.
    #[protocol_droid::prompt(getter = get_test_prompt_with_image)]
    fn test_prompt_with_image() -> Prompt {
        Prompt {
            description: Some("A prompt whose message is an image.".to_owned()),
            ..Prompt::new("test_prompt_with_image")
        }
    }

    /// A prompt that asks for a code review, with two arguments.
    ///
    /// The conformance suite's `prompts/list` and `prompts/get` scenarios
    /// require a prompt with a description and at least one argument, so
    /// a prompt without `arguments` would not exercise the schema path
    /// the suite reads. The two arguments — `diff` (required) and
    /// `focus` (optional) — are the smallest shape that covers both
    /// `required: [...]` and `arguments[i].required: true`.
    #[protocol_droid::prompt(getter = get_review)]
    fn review() -> Prompt {
        Prompt {
            description: Some(
                "Asks for a review of a diff against an optional focus area.".to_owned(),
            ),
            arguments: vec![
                PromptArgument {
                    name: "diff".to_owned(),
                    description: Some("The diff to review.".to_owned()),
                    title: None,
                    required: true,
                },
                PromptArgument {
                    name: "focus".to_owned(),
                    description: Some(
                        "What to look at (e.g. `security`, `performance`).".to_owned(),
                    ),
                    title: None,
                    required: false,
                },
            ],
            ..Prompt::new("review")
        }
    }

    /// A prompt without arguments, so `prompts/get` is exercised against
    /// at least one prompt whose `arguments` is the empty array.
    #[protocol_droid::prompt(getter = get_summarize)]
    fn summarize() -> Prompt {
        Prompt {
            description: Some("Summarizes whatever the caller hands in.".to_owned()),
            ..Prompt::new("summarize")
        }
    }

    /// `test_simple_prompt`, which the suite's `prompts-get-simple`
    /// scenario calls by name.
    ///
    /// Same lesson as the `test://static-text` resources: `prompts-list`
    /// passes for a fixture that registers `review` and `summarize`, because
    /// the list is non-empty and every entry has a name. The `prompts/get`
    /// scenario is the one that names a specific prompt, and it fails
    /// `no such prompt` against names the suite never asked for. The two
    /// fixtures coexist on purpose — the suite reads several prompts, and a
    /// server that only answered the suite's names would prove nothing about
    /// the general case.
    #[protocol_droid::prompt(getter = get_test_simple_prompt)]
    fn test_simple_prompt() -> Prompt {
        Prompt {
            description: Some("A simple prompt with no arguments.".to_owned()),
            ..Prompt::new("test_simple_prompt")
        }
    }

    /// `test_prompt_with_arguments`, which `prompts-get-with-args` calls
    /// with `arg1` and `arg2`.
    ///
    /// Both arguments are required, which is what makes this scenario worth
    /// having: it asserts the *values* come back substituted into the
    /// message body, so a getter that ignored `arguments` and returned a
    /// fixed string would pass `prompts-get-simple` and fail here.
    #[protocol_droid::prompt(getter = get_test_prompt_with_arguments)]
    fn test_prompt_with_arguments() -> Prompt {
        Prompt {
            description: Some("A prompt that substitutes two arguments.".to_owned()),
            arguments: vec![
                PromptArgument {
                    name: "arg1".to_owned(),
                    description: Some("First test argument.".to_owned()),
                    title: None,
                    required: true,
                },
                PromptArgument {
                    name: "arg2".to_owned(),
                    description: Some("Second test argument.".to_owned()),
                    title: None,
                    required: true,
                },
            ],
            ..Prompt::new("test_prompt_with_arguments")
        }
    }

    /// A static readme resource, with a `text/` MIME type the conformance
    /// suite's `resources/list` and `resources/read` scenarios both read.
    #[protocol_droid::resource(reader = read_readme)]
    fn readme() -> Resource {
        Resource {
            description: Some("The reference server's own readme.".to_owned()),
            mime_type: Some("text/markdown".to_owned()),
            ..Resource::new("file:///conformance/readme.md", "readme")
        }
    }

    /// A second resource so the suite sees a `resources/list` reply that
    /// is not a single-element array. A regression that registers the
    /// first resource twice would still pass a one-resource test.
    #[protocol_droid::resource(reader = read_changelog)]
    fn changelog() -> Resource {
        Resource {
            description: Some("The reference server's own changelog.".to_owned()),
            mime_type: Some("text/markdown".to_owned()),
            ..Resource::new("file:///conformance/CHANGELOG.md", "changelog")
        }
    }

    /// `test://static-text`, which is the URI the suite's `resources-read-text`
    /// scenario names verbatim.
    ///
    /// The URI is not this crate's choice. An earlier revision of this
    /// fixture registered `file:///conformance/readme.md` and a
    /// `file:///conformance/CHANGELOG.md`, both of which satisfy
    /// `resources/list` — the list is non-empty and every entry has a `uri`
    /// and a `name`. The scenario then failed on the *read*, with `no such
    /// resource`, because it asks for `test://static-text` by name and the
    /// server had nothing registered under that URI. A fixture whose names
    /// are its own invention passes a list check and fails the read, which
    /// is a worse failure to diagnose than having no resource at all: the
    /// passing `resources-list` leg reads like evidence the surface is
    /// right when the scenario that actually reads one says otherwise.
    #[protocol_droid::resource(reader = read_static_text)]
    fn static_text_resource() -> Resource {
        Resource {
            description: Some("A static text resource for the read scenario.".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            ..Resource::new("test://static-text", "static_text")
        }
    }

    /// `test://static-binary`, which the `resources-read-binary` scenario
    /// names verbatim. Blob rather than text, and the same 1x1 PNG the
    /// `tools-call-image` tool returns, so a decode failure in the suite
    /// cannot be a bad fixture payload mistaken for a codec bug.
    #[protocol_droid::resource(reader = read_static_binary)]
    fn static_binary_resource() -> Resource {
        Resource {
            description: Some("A static binary resource for the read scenario.".to_owned()),
            mime_type: Some("image/png".to_owned()),
            ..Resource::new("test://static-binary", "static_binary")
        }
    }

    /// `test://example-resource`, which `prompts-get-embedded-resource` embeds by
    /// passing its URI as the `resourceUri` argument. Registered as a real
    /// resource rather than only named in the prompt, so a peer that follows
    /// the link it is handed finds something.
    #[protocol_droid::resource(reader = read_example_resource)]
    fn example_resource() -> Resource {
        Resource {
            description: Some("The resource the embedded-resource prompt embeds.".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            ..Resource::new("test://example-resource", "example_resource")
        }
    }

    /// `test://watched-resource`, which the `resources-subscribe` and
    /// `resources-unsubscribe` scenarios name.
    #[protocol_droid::resource(reader = read_watched_resource)]
    fn watched_resource() -> Resource {
        Resource {
            description: Some("A resource the suite subscribes to.".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            ..Resource::new("test://watched-resource", "watched_resource")
        }
    }

    /// `test://template/123/data`, which `resources-templates-read` reads
    /// directly by URI.
    ///
    /// Registered as a plain resource rather than a `ResourceTemplate`. The
    /// spec's distinction is that a template advertises a URI *pattern* to be
    /// filled in, and the suite's scenario reads a concrete URI without
    /// filling one in — the value is already complete. A template would also
    /// be the more instructive shape for a real server, but registering one
    /// changes what `resources/templates/list` reports and this scenario does
    /// not read that.
    #[protocol_droid::resource(reader = read_template_data)]
    fn template_data() -> Resource {
        Resource {
            description: Some("The resource a template URI resolves to.".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            ..Resource::new("test://template/123/data", "template_data")
        }
    }

    /// Serves the readme. Takes the URI the peer named rather than closing
    /// over the listing's, so a read is answered for the resource that was
    /// actually asked for.
    fn read_readme(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        vec![static_text(
            uri,
            "# Reference server\n\nA fixture for the conformance suite.\n",
        )]
    }

    /// Serves the changelog, which is a second body so the two reads do not
    /// return identical bytes.
    fn read_changelog(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        vec![static_text(
            uri,
            "# Changelog\n\n## 0.1.0\n\n- First release.\n",
        )]
    }

    /// Serves `test://static-text`. `text/plain` rather than the
    /// `text/markdown` the two file-backed resources use, because the
    /// scenario asserts a `mimeType` is present and the spec treats the
    /// two as distinct types.
    fn read_static_text(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        vec![static_text(
            uri,
            "This is the content of the static text resource.",
        )]
    }

    /// Serves `test://static-binary` as a `blob`, which is the member a
    /// binary resource carries and a text resource does not. Emitting
    /// `text` here would satisfy "contents is not empty" and fail the
    /// scenario's read of a binary body, which is the distinction the
    /// scenario exists to check.
    fn read_static_binary(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        vec![ResourceContents::blob(uri, PNG_1X1).with_mime_type("image/png")]
    }

    /// Serves `test://example-resource`.
    fn read_example_resource(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        vec![static_text(uri, "This is the example resource's text.")]
    }

    /// Serves `test://watched-resource`.
    ///
    /// The body says it is watched, which it is not — nothing in this fixture
    /// emits `notifications/resources/updated`. The scenario that exercises a
    /// subscription checks that subscribing is *accepted*; the fixture has no
    /// change to report, so a body that claims otherwise would be a lie the
    /// suite could not catch but the next reader would.
    fn read_watched_resource(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        vec![static_text(uri, "This resource is watched by the client.")]
    }

    /// Serves `test://template/123/data`.
    fn read_template_data(_context: RequestContext, uri: &str) -> Vec<ResourceContents> {
        // The id is lifted out of the URI and echoed into the body, because the
        // scenario checks that a template's variable reached the content: a
        // server that returns a fixed string passes a read and fails the
        // substitution it was actually testing for. The body is JSON for the
        // same reason — the scenario documents this resource as
        // `application/json`, and a `text/plain` answer to it is a different
        // resource than the one it asked for.
        let id = uri
            .split('/')
            .nth(3)
            .filter(|segment| !segment.is_empty())
            .unwrap_or_default();
        let body = format!(r#"{{"id":"{id}","templateTest":true,"data":"Data for ID: {id}"}}"#);
        vec![ResourceContents::text(uri, &body).with_mime_type("application/json")]
    }

    /// Builds `test_prompt_with_embedded_resource`'s messages, embedding the
    /// URI the caller named.
    fn get_test_prompt_with_embedded_resource(
        _context: RequestContext,
        arguments: &JsonObject,
    ) -> Vec<PromptMessage> {
        let uri = string_argument(arguments, "resourceUri")
            .unwrap_or_else(|| "test://example-resource".to_owned());
        vec![PromptMessage::new(
            Role::User,
            Content::EmbeddedResource(EmbeddedResource::text(
                uri,
                "This is an embedded resource's text.",
            )),
        )]
    }

    /// Builds `test_prompt_with_image`'s messages: an image block, which is
    /// the prompt-side counterpart of `tools-call-image`.
    fn get_test_prompt_with_image(
        _context: RequestContext,
        _arguments: &JsonObject,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new(
            Role::User,
            Content::Image(ImageContent::new(PNG_1X1, "image/png")),
        )]
    }

    /// Builds the review prompt's messages.
    ///
    /// The `diff` argument is required by the prompt's schema above, so its
    /// absence here is a peer's error rather than something to paper over:
    /// the message says which argument is missing and names it.
    fn get_review(_context: RequestContext, arguments: &JsonObject) -> Vec<PromptMessage> {
        let focus = string_argument(arguments, "focus");
        vec![PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new(format!(
                "Review this diff{}.",
                focus.map_or_else(String::new, |focus| format!(", focusing on {focus}"))
            ))),
        )]
    }

    /// Builds the summarize prompt's messages, which take no arguments.
    fn get_summarize(_context: RequestContext, _arguments: &JsonObject) -> Vec<PromptMessage> {
        vec![PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new("Summarize the following.")),
        )]
    }

    /// Builds `test_simple_prompt`'s messages, which take no arguments.
    fn get_test_simple_prompt(
        _context: RequestContext,
        _arguments: &JsonObject,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new("This is a simple prompt for testing.")),
        )]
    }

    /// Builds `test_prompt_with_arguments`'s messages.
    ///
    /// Both values are interpolated rather than echoed into a fixed string,
    /// because the scenario asserts that the arguments it sent appear in the
    /// message body. A getter that ignored `arguments` would satisfy the
    /// "messages is a non-empty array of role+content" check and fail the
    /// substitution check, which is the only part of this scenario a real
    /// implementation can get wrong.
    fn get_test_prompt_with_arguments(
        _context: RequestContext,
        arguments: &JsonObject,
    ) -> Vec<PromptMessage> {
        let arg1 = string_argument(arguments, "arg1").unwrap_or_else(|| "<missing>".to_owned());
        let arg2 = string_argument(arguments, "arg2").unwrap_or_else(|| "<missing>".to_owned());
        vec![PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new(format!(
                "Prompt with arguments: arg1='{arg1}', arg2='{arg2}'"
            ))),
        )]
    }
}

/// Builds a `sampling/createMessage` request carrying one user message.
///
/// `None` when the prompt will not serialise, which a `String` from a JSON
/// peer can only do if the peer sent something that was not text. Answering
/// with a refusal is better than sending a half-built request.
fn sampling_request(prompt: &str) -> Option<JsonObject> {
    serde_json::to_string(prompt)
        .ok()
        .and_then(|text| {
            JsonObject::new(format!(
                r#"{{"messages":[{{"role":"user","content":{{"type":"text","text":{text}}}}}],"maxTokens":16}}"#
            ))
            .ok()
        })
}

/// Builds an `elicitation/create` request carrying one message.
fn elicitation_request(message: &str) -> Option<JsonObject> {
    // `requestedSchema` is required by the schema, not optional with a
    // default, and a client validating against the schema rejects the request
    // without it — which reads at the suite as "the server never elicited",
    // because the failure happens before it can record that it was asked.
    //
    // One top-level property, no nesting: the schema allows only the
    // `PrimitiveSchemaDefinition` subset, and a nested object here is refused
    // by the client for the same reason.
    serde_json::to_string(message).ok().and_then(|text| {
        JsonObject::new(format!(
            r#"{{"message":{text},"requestedSchema":{{"type":"object","properties":{{"answer":{{"type":"string","title":"Answer"}}}},"required":["answer"]}}}}"#
        ))
        .ok()
    })
}

/// Emits one `notifications/message` record at `info`.
///
/// `ctx.log` is gated on the session having negotiated logging, and returns a
/// typed error rather than writing nothing, so a fixture that ignored it would
/// silently emit zero records on a session that never negotiated the feature —
/// and `tools-call-with-logging` would report `No log notifications received`
/// with nothing to distinguish "the server declined" from "the server sent
/// none". The result is deliberately discarded here for the opposite reason:
/// the scenario's own server is the one that decides whether to advertise
/// logging, and it does.
async fn log_message(ctx: &RequestContext, message: &str) {
    let _outcome = ctx.log(LogLevel::Info, message).await;
}

/// Emits one `notifications/progress` increment against the caller's token.
async fn progress_message(ctx: &RequestContext, progress: i64, total: i64) {
    let Ok(params) = JsonObject::new(format!(r#"{{"progress":{progress},"total":{total}}}"#))
    else {
        // The arguments are two `i64`s interpolated into a literal, so this
        // branch is unreachable; returning quietly is better than unwrapping a
        // value this crate built itself.
        return;
    };
    let _outcome = ctx.report_progress(&params).await;
}

/// A `text/markdown` resource body.
fn static_text(uri: &str, body: &str) -> ResourceContents {
    ResourceContents::text(uri, body).with_mime_type("text/markdown")
}

/// One string member of an arguments object.
fn string_argument(arguments: &JsonObject, key: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(arguments.as_str()).ok()?;
    parsed
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Parses `--addr <ipv4:port>` from argv, falling back to loopback on
/// port 8080. A single flag because the conformance wrapper is the one
/// that picks the address and forwards it; a fixture that took
/// eleven flags would not be a fixture.
///
/// Returns a `Result` rather than panicking on a malformed
/// `--addr <value>`, so the error path is the same chain the
/// `axum::Error` / `Bind` errors report.
fn parse_addr() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--addr"
            && let Some(value) = args.next()
        {
            return Ok(value.parse()?);
        }
    }
    Ok("127.0.0.1:8080".parse()?)
}

/// Builds the server, then serves it on the chosen address.
///
/// # Errors
///
/// A `Box<dyn Error>` is the smallest type the surface needs: a
/// `JsonObjectError` for a tool whose schema would not parse, an
/// `io::Error` for a bind failure, an `axum::Error` for an accept
/// failure. All are reported on the way out with their own chain.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve())
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let addr = parse_addr()?;
    // `Server::serve_http` is the one-call facade, but it does not let the
    // caller pick the address — the fixture is started by a wrapper that
    // does, and an address baked into the fixture would make that wrapper's
    // job harder. The `HttpBuilder` is the explicit form that takes an
    // address, and the only piece of the suite of choices the fixture
    // actually needs to make.
    let server: Server = ConformanceFixture.into_server()?;
    // The suite exercises elicitation and sampling, which a tool can only
    // reach by putting a question on a stream and waiting for the answer. A
    // server answering with one JSON document has no stream to put it on, so
    // this is the one configuration choice the fixture has to make — and it
    // is made here, out loud, rather than left to a default it has to break.
    //
    // Without it those scenarios fail with the tool reporting that it cannot
    // ask the peer, which is the correct behaviour for the default and the
    // wrong one for this fixture.
    let config = protocol_droid::adapters::primary::transport::http::ServerConfig::new()?
        .with_streaming(protocol_droid::adapters::primary::transport::http::Streaming::EventStream);
    let builder = protocol_droid::adapters::primary::transport::http::HttpBuilder::new(server)
        .addr(addr)
        .config(config);
    let bound = builder.serve().await?;
    eprintln!("{}: listening on {}", SERVER_NAME, bound.addr);
    Ok(())
}
