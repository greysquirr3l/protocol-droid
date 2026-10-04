//! A `#[prompt]` carrying a `#[resource]`'s return type.
//!
//! The cross, rather than an unrelated type: the method is written correctly
//! apart from the pair, and the expansion would otherwise register a `Prompt`
//! method that produces a `Resource` — a builder method handed a type it does
//! not take, with the real mistake two attributes away.

/// A server whose prompt returns a resource.
struct Reviewer;

#[protocol_droid::server(name = "reviewer")]
impl Reviewer {
    /// Marked as a prompt, but produces a `Resource`.
    #[protocol_droid::prompt]
    fn review() -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new("file:///diff", "review")
    }
}

fn main() {
    let _ = Reviewer;
}
