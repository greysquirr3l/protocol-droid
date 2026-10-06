//! Shared plumbing for the OAuth adapters' outbound HTTP calls.
//!
//! Two adapters talk to an authorization server — introspecting a token and
//! redeeming a code — and both need the same two things: a reason string that
//! cannot leak the credential the request carried, and a bounded read of the
//! response. They live here rather than in each adapter because a size cap
//! copied into two files is a cap that will eventually be fixed in one of them.

/// The most these adapters will read from one response.
///
/// An authorization server is not trusted to bound its own answer. One that is
/// compromised, or merely streaming forever, would otherwise be a way to
/// exhaust this process's memory from a single call — and the call that does it
/// is one this process believed was authenticated. 64 KiB is far more than any
/// legal response: both documents are a handful of short strings.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Read a response body, stopping if it grows past [`MAX_RESPONSE_BYTES`].
///
/// Enforced while reading rather than after, because `content_length` is a claim
/// the peer makes about itself and is absent entirely from a chunked response —
/// checking it and then calling `bytes()` would bound only the responses that
/// chose to be bounded. `chunk()` needs no `StreamExt`, which keeps this off the
/// `futures` facade for one loop.
///
/// The failure is a reason string rather than a shared error type on purpose:
/// introspection and redemption refuse in their own error enums, and this
/// function has no business knowing either of them.
pub async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(describe)? {
        // `saturating_add` because the whole point is that the running total is
        // attacker-controlled: a length near `usize::MAX` must not wrap into a
        // small number and slip past the check.
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(format!("response exceeded {MAX_RESPONSE_BYTES} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Why an HTTP call to the authorization server failed, without the credential.
///
/// `reqwest`'s `Display` for a transport error names the URL and the cause but
/// never the body, and these adapters only ever put credentials in a body — so
/// the failure text is safe to surface in a log.
///
/// Asserted by each adapter's redaction test.
pub fn describe(failure: impl core::fmt::Display) -> String {
    failure.to_string()
}
