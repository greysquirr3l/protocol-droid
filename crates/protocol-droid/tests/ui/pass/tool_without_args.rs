//! A `#[tool]` with no parameters at all.
//!
//! The shape a peer discovers first, and the one whose generated argument
//! struct is empty — a braced struct rather than a unit, so the `{}` the
//! specification sends for a no-argument tool deserialises.

/// A server with one tool that takes nothing.
struct Greeter;

#[protocol_droid::server(name = "greeter")]
impl Greeter {
    /// Answers, always.
    #[protocol_droid::tool]
    async fn ping() -> Result<String, String> {
        Ok("pong".to_string())
    }
}

fn main() {
    let _ = Greeter.into_server();
}
