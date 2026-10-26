//! The `#[server]` macro, exercised from a crate that does not depend on
//! `schemars`.
//!
//! # Why this file lives in the CLI crate
//!
//! The `#[server]` macro generates a `JsonSchema` derive for a tool's
//! argument struct. The derive's own expansion names `schemars::…`
//! unqualified, so it resolves against the *consuming* crate's root — which
//! means the consumer needs `schemars` in scope as a direct dependency, or the
//! expansion fails to compile.
//!
//! Every other test of the macro lives in the `protocol-droid` crate, and
//! every one of those is blind to the problem: `protocol-droid` depends on
//! `schemars` itself, so `schemars` is in the extern prelude of its own
//! integration tests, doctests and trybuild harness alike. A test there
//! compiles whether or not the expansion is self-contained.
//!
//! This crate is the workspace's one consumer of `protocol-droid` that does
//! *not* depend on `schemars`, so it is the only place in the tree where the
//! question "does the expansion need the consumer to name `schemars`?" has a
//! real answer. That makes this file a genuine black-box test rather than a
//! restatement of the macro's own intent.
//!
//! The fix is the `schemars(crate = "::protocol_droid::schemars")` attribute
//! on the generated struct, which makes the derive emit
//! `use ::protocol_droid::schemars as schemars;` and shadow the unresolved
//! name for every path it expands to.
//!
//! # What is asserted
//!
//! Not only that the expansion compiles — a macro test that only compiles
//! proves very little, because the expansion can typecheck and still register
//! a tool with a malformed schema. This file also reads the generated
//! `inputSchema` back out of the registry and checks it describes the
//! handler's parameters, so a redirect that pointed the derive at the wrong
//! crate would fail here rather than passing a compile check.

// The `#[tool]` handler is `async` because the macro requires it: the
// generated factory calls it and awaits the future it returns, so a plain
// `fn` would answer with a value that has nothing to await. The lint's own
// remedy — deleting the `async` — is the bug.
#![expect(
    clippy::unused_async,
    reason = "the generated handler awaits the call, and a plain value has nothing to await"
)]
#![expect(
    clippy::unused_async_trait_impl,
    reason = "the generated handler awaits the call, and a plain value has nothing to await"
)]

use protocol_droid::domain::registry::PrimitiveKind;
use protocol_droid::domain::registry::primitive::Primitive;

/// What a test here can fail with.
///
/// One enum with a `From` for each error type in play, so a `?` on a
/// `JsonObjectError` or a `serde_json::Error` is a compile error until the
/// conversion is written.
#[derive(Debug)]
enum Failure {
    /// The server did not build.
    NotBuilt(String),
    /// The schema could not be read.
    Unreadable(String),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotBuilt(text) => write!(f, "the server did not build: {text}"),
            Self::Unreadable(text) => write!(f, "the schema was not readable: {text}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<protocol_droid::domain::ir::JsonObjectError> for Failure {
    fn from(error: protocol_droid::domain::ir::JsonObjectError) -> Self {
        Self::NotBuilt(error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::Unreadable(error.to_string())
    }
}

/// A server whose tools take a named argument and none.
///
/// The argument is the point: a tool with no parameters would generate a
/// struct with no fields, and the schema of that struct is a much weaker
/// signal than the schema of one with a field in it. The redirect under test
/// changes what the derive emits for *every* field, so the field is what
/// proves the redirect reached the field-level paths too.
///
/// Both shapes are here because the macro emits them from two separate
/// `quote!` blocks — a braced struct with no members for the no-argument case
/// and a struct with fields otherwise — and each carries its own copy of the
/// redirect. A test that covered only one would pass while the other branch
/// had lost the attribute.
struct Greeter;

#[protocol_droid::server(name = "greeter")]
impl Greeter {
    /// Greets someone by name.
    #[protocol_droid::tool]
    async fn greet(name: String) -> Result<String, String> {
        Ok(format!("hello, {name}"))
    }

    /// Says hello to nobody in particular.
    #[protocol_droid::tool]
    async fn greet_everyone() -> Result<String, String> {
        Ok("hello, everyone".to_owned())
    }
}

/// A named tool's `inputSchema`, as JSON.
///
/// The registry is read directly rather than through a dispatcher: the
/// question here is what the macro *registered*, and a session would add a
/// negotiation and a transport to the path without adding a fact about the
/// macro.
fn tool_schema(name: &str) -> Result<serde_json::Value, Failure> {
    let server = Greeter.into_server()?;
    // The snapshot is an `Arc`, so it has to be bound to outlive the borrow
    // of the entry taken out of it.
    let snapshot = server.registry().snapshot();
    let listed = snapshot
        .list(PrimitiveKind::Tool)
        .iter()
        .find(|registered| registered.id().as_str() == name)
        .ok_or_else(|| Failure::Unreadable(format!("the registry holds no tool named `{name}`")))?;

    let Primitive::Tool(tool) = listed.primitive() else {
        return Err(Failure::Unreadable(format!(
            "the registry entry named `{name}` is not a tool"
        )));
    };
    // `input_schema` is kept as the original text, validated once on the way
    // in. Reading it back as a `Value` is what lets the assertion below ask
    // whether it *describes* the handler rather than merely parsing.
    Ok(serde_json::from_str(tool.input_schema.as_str())?)
}

#[test]
fn the_server_macro_needs_no_schemars_dependency() -> Result<(), Failure> {
    // The compile is the assertion. If the expansion named `schemars`
    // unqualified, this file would not compile at all — and the error would
    // point at the `#[server]` attribute, which is exactly the confusion the
    // issue reported.
    Greeter
        .into_server()
        .map_err(|error| Failure::NotBuilt(error.to_string()))?;
    Ok(())
}

#[test]
fn the_generated_schema_describes_the_handler_parameters() -> Result<(), Failure> {
    let schema = tool_schema("greet")?;

    // The schema is a JSON Schema object with a `properties` map, and the
    // handler's `name` parameter is in it. A schema that came back empty, or
    // without the field, would mean the derive ran against something other
    // than the argument struct.
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Err(Failure::Unreadable(format!(
            "the schema has no `properties` object: {schema}"
        )));
    };
    if !properties.contains_key("name") {
        return Err(Failure::Unreadable(format!(
            "the schema has no `name` property: {properties:?}"
        )));
    }
    Ok(())
}

#[test]
fn the_no_argument_tools_schema_is_an_object_and_not_null() -> Result<(), Failure> {
    // The no-argument branch generates a *braced* struct with no members, and
    // the specification says such a tool's arguments are `{}` — an object with
    // no members, not the absence of a value. A unit struct would deserialise
    // from `null` and produce a `null` schema, which tells a model the
    // arguments are nothing rather than an empty object. So the shape of the
    // schema is the assertion: `type` is `object`.
    //
    // There is no `properties` key to check here, and its absence is not a
    // defect: `schemars` omits `properties` for a struct with no members, and
    // a JSON Schema with `type: "object"` and no `properties` accepts exactly
    // `{}` — which is what the specification asks for. The assertion is on
    // `type`, because that is the field that separates "an object with no
    // members" from "null", and the failure this guards against is the unit
    // struct that would have produced the latter.
    let schema = tool_schema("greet_everyone")?;

    if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        return Err(Failure::Unreadable(format!(
            "the schema is not an object: {schema}"
        )));
    }
    // And the object must have no members to validate, or the peer would be
    // told the tool takes arguments it does not.
    if let Some(properties) = schema.get("properties") {
        let empty = properties
            .as_object()
            .is_some_and(serde_json::Map::is_empty);
        if !empty {
            return Err(Failure::Unreadable(format!(
                "the schema has properties, and a no-argument tool has none: {properties}"
            )));
        }
    }
    Ok(())
}
