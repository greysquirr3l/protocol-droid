//! A `#[prompt]`, the third mark and the other half of a registry's halves.
//!
//! Same shape as a resource — a plain `fn` returning the value, called once —
//! but the value carries its own arguments, which is where a prompt differs
//! from anything else the server lists.

/// A server that offers one prompt.
struct Reviewer;

#[protocol_droid::server(name = "reviewer")]
impl Reviewer {
    /// Asks for a review of a diff.
    #[protocol_droid::prompt]
    fn review() -> protocol_droid::domain::registry::Prompt {
        let mut prompt = protocol_droid::domain::registry::Prompt::new("review");
        prompt
            .arguments
            .push(protocol_droid::domain::registry::PromptArgument {
                name: "diff".to_string(),
                description: Some("The text to review.".to_string()),
                title: None,
                required: true,
            });
        prompt
    }
}

fn main() {
    let _ = Reviewer.into_server();
}
