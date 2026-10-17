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
    #[protocol_droid::prompt(getter = get_review)]
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

    /// Serves the prompt's messages, by the arguments the peer sent.
    fn get_review(
        _context: protocol_droid::application::server::context::RequestContext,
        _arguments: &protocol_droid::domain::ir::JsonObject,
    ) -> Vec<protocol_droid::domain::ir::PromptMessage> {
        Vec::new()
    }
}

fn main() {
    let _ = Reviewer.into_server();
}
