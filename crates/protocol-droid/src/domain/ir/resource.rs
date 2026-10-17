//! The contents a `resources/read` answers with.
//!
//! # Why this is separate from `Content`
//!
//! A resource's contents and a content block are different shapes that happen
//! to share a vocabulary. A content block is a tagged union whose member is
//! chosen by its `type`; a resource's contents entry is flat, with `uri` beside
//! `text` *or* `blob` and no discriminator at all. The vendored schema puts
//! them under different definitions — `TextResourceContents` and
//! `BlobResourceContents`, joined by `anyOf` inside `ReadResourceResult` — so
//! folding them into one type would have the IR assert a distinction the
//! specification does not draw.
//!
//! The absence of a discriminator is why [`ResourceBody`] is an enum here
//! rather than a pair of `Option`s. The schema permits an entry with both
//! `text` and `blob`, or with neither; this type cannot express either, and
//! says so by being impossible to construct rather than by validating later.

/// One entry of a `resources/read` result's `contents` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceContents {
    /// The resource's URI, which the peer named in the request.
    ///
    /// Carried on every entry rather than once on the result, because the
    /// schema requires it on every entry.
    pub uri: String,
    /// The media type, if known.
    pub mime_type: Option<String>,
    /// The body, in one of the two forms the schema allows.
    pub body: ResourceBody,
}

/// A resource's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceBody {
    /// The text form, spelled `text` on the wire.
    Text(String),
    /// The base64 blob form, spelled `blob` on the wire.
    Blob(String),
}

impl ResourceContents {
    /// A textual resource.
    #[must_use]
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            mime_type: None,
            body: ResourceBody::Text(text.into()),
        }
    }

    /// A binary resource, already base64-encoded.
    ///
    /// The caller encodes. A decoder here would be a second opinion about
    /// whether a payload is binary, and the MIME type the caller already holds
    /// is the better witness.
    #[must_use]
    pub fn blob(uri: impl Into<String>, base64: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            mime_type: None,
            body: ResourceBody::Blob(base64.into()),
        }
    }

    /// The same contents, with a media type.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourceBody, ResourceContents};

    #[test]
    fn a_text_resource_carries_its_body_as_text() -> Result<(), String> {
        let contents = ResourceContents::text("test://a", "hello");

        match contents.body {
            ResourceBody::Text(ref text) if text == "hello" => {}
            other => return Err(format!("the body was {other:?}")),
        }
        if contents.uri != "test://a" {
            return Err(format!("the uri was {:?}", contents.uri));
        }
        Ok(())
    }

    #[test]
    fn a_blob_resource_carries_its_body_as_a_blob() -> Result<(), String> {
        let contents = ResourceContents::blob("test://a", "aGk=");

        match contents.body {
            ResourceBody::Blob(ref data) if data == "aGk=" => Ok(()),
            other => Err(format!("the body was {other:?}")),
        }
    }

    #[test]
    fn a_media_type_attaches_without_touching_the_body() -> Result<(), String> {
        let contents = ResourceContents::text("test://a", "hello").with_mime_type("text/markdown");

        match contents.mime_type.as_deref() {
            Some("text/markdown") => {}
            other => return Err(format!("the mime type was {other:?}")),
        }
        match &contents.body {
            ResourceBody::Text(text) if text == "hello" => Ok(()),
            other => Err(format!("the body became {other:?}")),
        }
    }
}
