//! A `#[resource]` written `async`.
//!
//! A resource is registered once, while the server is being built, so there
//! is nothing to await: `into_server` calls the method and wants a `Resource`,
//! and an `async fn` hands it a future instead. The natural mistake is to
//! carry `async` over from a `#[tool]`, which genuinely does wait.

/// A server whose resource is mistakenly asynchronous.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The project's readme.
    #[protocol_droid::resource]
    async fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }
}

fn main() {
    // Nothing here runs: the error is emitted while the `impl` is expanded and
    // the whole block is replaced. The type is still named so that it is not
    // reported as dead, which would bury the one message that matters.
    let _ = Docs;
}
