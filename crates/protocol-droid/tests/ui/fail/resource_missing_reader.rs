//! A `#[resource]` with no reader named.
//!
//! Registering the listing alone produces a server that advertises a resource
//! and then refuses every read of it, which is a promise broken on the wire
//! rather than at the call site. The mark therefore requires the reader's name,
//! and this is the error for leaving it off.

/// A server whose resource cannot be read.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The readme, listed but never served.
    #[protocol_droid::resource]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }
}

fn main() {
    let _ = Docs;
}
