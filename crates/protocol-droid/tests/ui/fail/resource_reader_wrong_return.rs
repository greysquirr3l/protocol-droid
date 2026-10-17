//! A resource reader that returns the wrong collection.
//!
//! The reader is checked for its element type, not just its container: a
//! `Vec<String>` is the right shape and the wrong contents, and would
//! otherwise fail at the codec with a message about serialisation instead of
/// about the reader.

/// A server whose reader returns strings.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The readme, listed.
    #[protocol_droid::resource(reader = read_readme)]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }

    /// Returns text rather than resource contents.
    fn read_readme(
        _context: protocol_droid::application::server::context::RequestContext,
        _uri: &str,
    ) -> Vec<String> {
        Vec::new()
    }
}

fn main() {
    let _ = Docs;
}
