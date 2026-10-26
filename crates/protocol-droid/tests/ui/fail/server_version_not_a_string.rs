//! A `version` that is a number rather than a string.
//!
//! The field takes an expression, and a number is an expression, so the macro
//! cannot refuse it by looking at what was written — it has to be the type
//! that refuses. The question this case pins is whether that refusal is
//! *readable*: a reader who wrote `version = 1` needs to be told which of the
//! two spellings was meant.

/// A server whose version is a number.
struct Docs;

#[protocol_droid::server(name = "docs", version = 1)]
impl Docs {
    /// The readme, listed.
    #[protocol_droid::resource(reader = read_readme)]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
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
    let _ = Docs;
}
