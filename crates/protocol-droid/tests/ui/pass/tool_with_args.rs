//! A `#[tool]` with arguments, which become the tool's `inputSchema`.
//!
//! The answer is a struct of the caller's own rather than a bare type, since
//! that is what a real tool returns and the derive is what the expansion
//! requires. `protocol_droid::serde` is used rather than `serde` because this
//! case is compiled as its own crate, depending only on `protocol-droid`.

/// What `add` answers with.
#[derive(protocol_droid::serde::Serialize)]
struct Sum {
    /// The total, which a peer reads out of `structuredContent`.
    total: i32,
}

/// A server with one tool.
struct Calculator;

#[protocol_droid::server(name = "calculator")]
impl Calculator {
    /// Adds two numbers.
    #[protocol_droid::tool]
    async fn add(a: i32, b: i32) -> Result<Sum, String> {
        Ok(Sum { total: a + b })
    }
}

fn main() {
    // Building the server is the whole check: both marked methods have been
    // turned into a registration, and a schema and a handler that typecheck.
    let _ = Calculator.into_server();
}
