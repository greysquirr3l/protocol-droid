//! A resource mark naming a reader the `impl` does not declare.
//!
//! The expansion calls `Self::read_it()`, so a name with no method beside it
//! fails inside generated code — reported against the `#[server]` attribute
//! and naming neither the reader nor the call. This is why the macro checks
//! the name against the block it is expanding.

/// A server whose resource names a reader that does not exist.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The readme, listed.
    #[protocol_droid::resource(reader = read_it)]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }
}

fn main() {
    let _ = Docs;
}
