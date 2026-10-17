//! A resource mark naming its reader with the wrong keyword.
//!
//! `handler` is the word `#[tool]` and `ServerBuilder::tool` use. Reaching for
//! it on a resource is an understandable slip, so the error names the keyword
//! that works rather than only rejecting the one that does not.

/// A server whose resource names its reader wrongly.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The readme, listed.
    #[protocol_droid::resource(handler = read_readme)]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }
}

fn main() {
    let _ = Docs;
}
