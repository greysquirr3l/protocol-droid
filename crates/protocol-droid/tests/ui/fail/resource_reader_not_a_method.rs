//! A resource mark naming its reader with a string rather than a path.
//!
//! The macro has to write `Self::<name>()`, so a string is not a name it can
//! use — and a reader declared in another module is not one it can reach
//! either. Either way the error points at the value.

/// A server whose resource names its reader with a string.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The readme, listed.
    #[protocol_droid::resource(reader = "read_readme")]
    fn readme() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///README.md", "readme")
    }
}

fn main() {
    let _ = Docs;
}
