//! A `#[tool]` whose answer cannot be serialised.
//!
//! The answer crosses the wire, so it has to implement `Serialize`, and the
//! expansion bounds the generated factory on it. A handler returning a type
//! of the author's own that has not been given the derive is the common
//! version of this: the signature looks right and nothing else does.

/// An answer that cannot be serialised, because nothing derives `Serialize`.
struct Opaque;

/// A server whose tool returns something unwritable.
struct Broken;

#[protocol_droid::server(name = "broken")]
impl Broken {
    /// Answers with a type no peer could read.
    #[protocol_droid::tool]
    async fn opaque() -> Opaque {
        Opaque
    }
}

fn main() {
    let _ = Broken;
}
