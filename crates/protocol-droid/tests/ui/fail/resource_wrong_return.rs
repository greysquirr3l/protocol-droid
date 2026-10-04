//! A `#[resource]` that returns the wrong type.
//!
//! The mark and the type are a pair, and it is easy to swap them: a method
//! returning a `Prompt` reads exactly like a resource method, and only the
//! attribute says which it is. Here it is neither, which is the case a user
//! hits when the return type is a helper of their own that has not been
/// written yet.

/// A server whose resource returns a string.
struct Docs;

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The readme's path, which is not a `Resource`.
    #[protocol_droid::resource]
    fn readme() -> String {
        "file:///README.md".to_string()
    }
}

fn main() {
    let _ = Docs;
}
