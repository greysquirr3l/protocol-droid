//! The messages a `prompts/get` answers with.
//!
//! # Why messages are not part of a `Prompt`
//!
//! `prompts/list` describes what a prompt *is* — its name, its arguments, its
//! description — and `prompts/get` produces what the prompt *says* for one set
//! of arguments. Those are separate methods with separate answers, and the
//! listing has no field to carry messages: the vendored `Prompt` definition for
//! `2025-06-18` has `name`, `title`, `description`, `arguments`, `icons` and
//! `_meta`, and nothing else.
//!
//! So the two live in two types. A `Prompt` value that could also hold
//! messages would be a shape the wire cannot express, and the registry would
//! be storing something no peer could read back.

use crate::domain::ir::content::{Content, Role};

/// One message of a `prompts/get` result.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptMessage {
    /// Who the message is meant for.
    pub role: Role,
    /// What it says.
    ///
    /// A `Content` rather than a `String`, because the schema types this field
    /// as `ContentBlock` — the same union a tool result carries — and the
    /// suite's image-bearing prompt scenario depends on that. A prompt cannot
    /// embed an image if its message is a string.
    pub content: Content,
}

impl PromptMessage {
    /// A message carrying one content block.
    #[must_use]
    pub const fn new(role: Role, content: Content) -> Self {
        Self { role, content }
    }
}

/// What `prompts/get` answers with.
///
/// Built from the messages rather than carrying them, so the description has
/// one obvious source: the prompt's own, already registered for `prompts/list`.
/// Reading it from here instead would leave two places that can disagree.
#[derive(Debug, Clone, PartialEq)]
pub struct GetPromptResult {
    /// The prompt's own description, echoed back.
    pub description: Option<String>,
    /// The messages, in order.
    pub messages: Vec<PromptMessage>,
}

impl GetPromptResult {
    /// The messages, with no description.
    #[must_use]
    pub const fn new(messages: Vec<PromptMessage>) -> Self {
        Self {
            description: None,
            messages,
        }
    }

    /// The same result, with a description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

}

#[cfg(test)]
mod tests {
    use super::PromptMessage;
    use crate::domain::ir::content::{Content, TextContent};
    use crate::domain::ir::{GetPromptResult, Role};

    #[test]
    fn a_result_carries_its_messages_and_no_description_by_default() -> Result<(), String> {
        let result = GetPromptResult::new(vec![PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new("hello")),
        )]);

        match result.description {
            None => {}
            other => return Err(format!("a result with no description carried {other:?}")),
        }
        if result.messages.len() != 1 {
            return Err(format!("expected 1 message, got {}", result.messages.len()));
        }
        Ok(())
    }

    #[test]
    fn a_description_is_kept_when_there_is_one() -> Result<(), String> {
        let result = GetPromptResult::new(vec![PromptMessage::new(
            Role::Assistant,
            Content::Text(TextContent::new("hi")),
        )])
        .with_description("greets");

        match result.description.as_deref() {
            Some("greets") => Ok(()),
            other => Err(format!("the description was {other:?}")),
        }
    }

    #[test]
    fn a_message_keeps_the_block_it_was_given() -> Result<(), String> {
        let message = PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new("an image, not a string")),
        );

        match (&message.role, &message.content) {
            (Role::User, Content::Text(text)) if text.text == "an image, not a string" => Ok(()),
            other => Err(format!("the message became {other:?}")),
        }
    }
}
