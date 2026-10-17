//! A `#[resource]`, which is registered once while the server is built.
//!
//! The method is a plain `fn` returning the value itself, not a future and
//! not a handler: `into_server` calls it, and there is nothing to await.

/// A server that lists one resource.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The project's readme, described by the value rather than by the doc
    /// comment above the method.
    #[protocol_droid::resource(reader = read_readme)]
    fn readme() -> protocol_droid::domain::registry::Resource {
        let mut resource =
            protocol_droid::domain::registry::Resource::new("file:///README.md", "readme");
        resource.description = Some("The project's readme.".to_string());
        resource.mime_type = Some("text/markdown".to_string());
        resource
    }

    /// Serves the readme, by the URI the peer named.
    fn read_readme(
        _context: protocol_droid::application::server::context::RequestContext,
        _uri: &str,
    ) -> Vec<protocol_droid::domain::ir::ResourceContents> {
        Vec::new()
    }
}

fn main() {
    let _ = Docs.into_server();
}
