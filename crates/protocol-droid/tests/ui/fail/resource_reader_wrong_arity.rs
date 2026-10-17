//! A resource reader that takes only the URI.
//!
//! The context is not optional on a reader: it carries who is asking, and a
//! reader that cannot see it would serve every caller as whoever opened the
//! connection.

/// A server whose reader is not told who is asking.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The readme, listed.
    #[protocol_droid::resource(reader = read_readme)]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }

    /// Takes the URI and nothing else.
    fn read_readme(_uri: &str) -> Vec<protocol_droid::domain::ir::ResourceContents> {
        Vec::new()
    }
}

fn main() {
    let _ = Docs;
}
