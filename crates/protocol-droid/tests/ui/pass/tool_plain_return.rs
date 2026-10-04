//! A `#[tool]` whose handler returns the answer rather than a `Result`.
//!
//! The other half of the return-type split, and the half that had no
//! compile-level check until this case: the factory splices the two shapes
//! into the same `match`, so a handler returning a bare value and one
//! returning a `Result` differ only in that one expression. When the plain
//! shape emitted a statement where the splice wanted an expression, the
//! expansion stopped parsing — and every handler written this way failed
//! with a parse error pointing at the `#[server]` attribute rather than at
//! the signature that caused it.
//!
//! Both a bare type and one of the author's own appear, so the `Serialize`
//! bound on the plain path is satisfied by a type the user had to derive it
//! for, not only by a type the prelude already provides.

/// What `summarise` answers with.
#[derive(protocol_droid::serde::Serialize)]
struct Summary {
    /// How many lines were counted.
    lines: usize,
}

/// A server whose tools answer directly.
struct Counter;

#[protocol_droid::server(name = "counter")]
impl Counter {
    /// Answers with a type the prelude already provides.
    #[protocol_droid::tool]
    async fn shout(text: String) -> String {
        text.to_uppercase()
    }

    /// Answers with a type of the author's own.
    #[protocol_droid::tool]
    async fn summarise(text: String) -> Summary {
        let lines = text.lines().count();
        Summary { lines }
    }
}

fn main() {
    // Building the server is the whole check: both marked methods have been
    // turned into a registration whose handler compiles on the path that
    // takes the answer directly.
    let _ = Counter.into_server();
}
