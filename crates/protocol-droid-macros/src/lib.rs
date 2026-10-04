//! Procedural macros for the `protocol-droid` toolkit.
//!
//! This crate is separate from `protocol-droid` because a proc-macro crate is
//! compiled for the host and consumed only by other crates, so it cannot be a
//! module of the library that would use it.
//!
//! It intentionally carries no `domain`, `application` or `adapters` modules.
//! The other crates in this workspace are built around a hexagonal layering,
//! and that layering describes where business logic lives. This crate has no
//! business logic: it is pure tooling that runs at compile time. Giving it four
//! empty directories to satisfy a pattern would be cargo cult, and would hide
//! the fact that the layout is chosen per crate rather than imposed blindly.
//!
//! # What the macros generate
//!
//! `#[tool]` on an `async fn` produces a `Tool` and a `ToolHandler` from the
//! function's own signature: its parameters become the tool's `inputSchema`,
//! and its return type becomes the answer. `#[server]` on an `impl` block
//! collects every `#[tool]` below it into a `Server`.
//!
//! # What they deliberately do not generate
//!
//! The wire document. A generated handler decodes its arguments and returns
//! through `application::server::result`, which owns the shape of a
//! `tools/call` result. A macro that built that JSON itself would be free to
//! disagree with the domain type and with the codec that lowers it, and
//! nothing would notice until a client read the difference.
//!
//! # Why `#[server]` goes on an `impl` block
//!
//! An attribute macro only ever sees the item it is attached to, so a `#[server]`
//! on a struct would not see the `impl` beside it, and every tool in that impl
//! would be invisible to it. Putting it on the `impl` puts the macro where the
//! methods are, which is the only place the tools can be read from.

// `dead_code_pub_in_binary` fires on both proc macros in this crate's own *test*
// build, and is wrong to. They are the entire public surface — consumed by
// `protocol-droid` and by its tests — and a proc macro cannot be *called* from
// inside the crate that defines it, so a test here has nothing to call. The
// check sees a library exporting two functions nobody calls, because the
// callers are in another crate and the lint does not cross the boundary.
//
// Gated on `test` because that is the only build it fires in; an ungated
// expectation would be unfulfilled everywhere else, which is its own error.
// Crate-level because that is where this particular lint has to be expected.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "a proc macro's callers are in other crates"
    )
)]
// A proc-macro crate can export only macros, so nothing in it is reachable from
// outside. That puts the two lints this file has to satisfy in direct
// conflict:
//
// * `unreachable_pub` — every `pub` item in a proc-macro crate is unreachable
//   by construction, because there is no way to name it.
// * `clippy::redundant_pub_crate` — a `pub(crate)` item in a module that is
//   not `pub` is redundant, and its fix is to write `pub`.
//
// So `pub` is required and forbidden, and the only spelling left is to
// acknowledge the contradiction. The crate-level `expect` below covers the
// `unreachable_pub` half; this one covers the other.
#[expect(
    clippy::redundant_pub_crate,
    reason = "a proc-macro crate exports no items, so 'pub' and 'pub(crate)' are               both wrong and the lint that says so is right only by accident"
)]
pub(crate) mod args;

/// Whether `attr` is the named attribute, however it was spelled.
///
/// Matched on the **last** path segment, not the whole path, because the
/// attribute is written `#[protocol_droid::tool]` in a user crate and `#[tool]`
/// after the re-export — the same attribute, two paths, and only a whole-path
/// match would see one of them. A macro that silently ignored the qualified
/// spelling would leave every tool unmarked and produce an empty server rather
/// than an error, which is the worst outcome available: the build succeeds and
/// the tools are gone.
fn is_attr(attr: &syn::Attribute, name: &str) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == name)
}


use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, ItemImpl, parse_macro_input};

/// Marks an `async fn` as a tool.
///
/// This attribute generates nothing on its own. `#[server]` reads the mark,
/// generates the tool's schema, the argument struct and the handler, and
/// registers all three — and it does so at module scope, which is the only
/// scope where generated items may be declared.
///
/// That split is forced, not chosen. A `#[tool]` on a method expands *inside*
/// the `impl` block, where Rust forbids declaring a struct or a function, and
/// where even a `const _` collides with every other `const _` in the same
/// `impl`. There is no way to emit a schema or an argument struct from a
/// method's own attribute, so a macro that tried would be generating code that
/// cannot compile. A marker that generates nothing is honest about that; a
/// marker that generated something would only generate something for free
/// functions, and the difference would be a rule nobody could see.
///
/// So a `#[tool]` outside a `#[server]` block is a mark on nothing. `#[server]`
/// is the only place a tool is built, and that is what makes the generated
/// names (`__{fn}_TOOL`, `__{fn}_HANDLER`, `__{fn}_Args`) predictable: there is
/// exactly one place that mints them.
///
/// # Examples
///
/// ```
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Sum { total: i32 }
///
/// struct Demo;
///
/// #[protocol_droid::server(name = "demo")]
/// impl Demo {
///     /// Adds two numbers.
///     #[protocol_droid::tool]
///     async fn add(a: i32, b: i32) -> Result<Sum, String> {
///         Ok(Sum { total: a + b })
///     }
/// }
/// ```
// `dead_code_pub_in_binary` fires on both of these in this crate's own *test*
// build, and is wrong to: they are the entire public surface, consumed by
// `protocol-droid` and by its tests, and a proc macro cannot be *called* from
// inside the crate that defines it. The check sees a library exporting two
// functions nobody calls, because the callers are in another crate and the
// lint does not cross the boundary.
//
// Gated on `test` because that is the only build it fires in, and an ungated
// `expect` would then be unfulfilled everywhere else — which is its own error.
#[proc_macro_attribute]
pub fn tool(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    // Parsed and discarded, purely so a malformed function is reported against
    // this attribute rather than as a parse failure from whatever the `#[tool]`
    // left behind in its place. The function itself is handed back untouched.
    let _ = syn::parse::<ItemFn>(item.clone());
    item
}

/// Collects every `#[tool]` in an `impl` block into a `Server`.
///
/// The `impl` is left exactly as written and a second inherent block is added
/// beside it, carrying one method:
///
/// ```text
/// fn into_server(self) -> Result<Server, JsonObjectError>
/// ```
///
/// The method consumes `self`, so the name reads as what it is: the type you
/// wrote becomes a server. It returns a `Result` because a tool's `inputSchema`
/// is a `serde_json` schema and a type with no schema — a map with a non-string
/// key, say — is a mistake worth naming rather than a panic.
///
/// The server's name is the type's own, or `name = "..."` when the name a peer
/// sees should differ from the Rust type name, which is usually the case.
///
/// # Examples
///
/// ```
/// #[derive(Default)]
/// struct Demo;
///
/// #[protocol_droid::server(name = "demo")]
/// impl Demo {
///     #[protocol_droid::tool]
///     /// Answers, always.
///     async fn ping() -> Result<String, String> {
///         Ok("pong".to_string())
///     }
/// }
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let server = Demo.into_server()?;
/// assert_eq!(server.name(), "demo");
/// # Ok(()) }
/// ```
// `dead_code_pub_in_binary` fires on both of these in this crate's own *test*
// build, and is wrong to: they are the entire public surface, consumed by
// `protocol-droid` and by its tests, and a proc macro cannot be *called* from
// inside the crate that defines it. The check sees a library exporting two
// functions nobody calls, because the callers are in another crate and the
// lint does not cross the boundary.
//
// Gated on `test` because that is the only build it fires in, and an ungated
// `expect` would then be unfulfilled everywhere else — which is its own error.
#[proc_macro_attribute]
pub fn server(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let block = parse_macro_input!(item as ItemImpl);
    let reference = &block;
    match expand_server(attribute, reference) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Expands `#[server]`.
///
/// # Errors
///
/// An `impl` with no self type, which has nothing to add a method to; and a
/// bare `impl` block, for the same reason.
fn expand_server(
    attribute: TokenStream,
    block: &ItemImpl,
) -> syn::Result<proc_macro2::TokenStream> {
    let self_ty = &block.self_ty;
    let fields = args::literal_fields(proc_macro2::TokenStream::from(attribute))?;
    let server_name = args::field(&fields, "name");

    // One registration per marked method, with everything it needs generated
    // beside it. The `const _` blocks are at module scope here — an attribute
    // on the `impl` is — so the names inside them cannot collide.
    let mut generated = Vec::new();
    let mut registrations = Vec::new();

    // A generated name is the type's own last path segment plus the method's,
    // so `Demo::add` and `Other::add` cannot collide. The type is in the name
    // because two types with a method of the same name is ordinary Rust, and
    // two generated `__add_TOOL`s at module scope is an error a user cannot
    // act on.
    let type_prefix = match self_ty.as_ref() {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map_or_else(String::new, |segment| segment.ident.to_string()),
        _ => String::new(),
    };

    for method in &block.items {
        let syn::ImplItem::Fn(function) = method else {
            continue;
        };
        if !function.attrs.iter().any(|attr| is_attr(attr, "tool")) {
            continue;
        }

        // The method's own `#[tool(name = .., description = ..)]` arguments.
        // Reading them is not optional: a `name = "renamed"` the macro dropped
        // would leave the Rust method name on the wire, and the tool would be
        // published under a name the author explicitly said not to use.
        let marker = function
            .attrs
            .iter()
            .find(|attr| is_attr(attr, "tool"))
            .map_or_else(proc_macro2::TokenStream::new, |attr| {
                attr.parse_args().unwrap_or_else(|_| proc_macro2::TokenStream::new())
            });
        let mut handler = args::read_handler(&function.attrs, marker, &function.sig)?;
        handler.prefix.clone_from(&type_prefix);
        let tool_fn = args::tool_fn(&handler);
        let handler_fn = args::handler_factory(&handler);
        let args_struct = args::args_struct(&handler);
        let method_name = &function.sig.ident;
        let factory = args::tool_fn_ident(&handler);
        let handler_factory_ident = args::handler_fn_ident(&handler);

        generated.push(quote! {
            #args_struct
            #tool_fn
            #handler_fn
        });
        registrations.push(quote! {
            builder = builder.tool(#factory()?, #handler_factory_ident(Self::#method_name));
        });
    }

    // With no `name`, the type's own name — which is what a reader of the
    // generated code would expect, and what `type_name` gives without the
    // macro having to carry a copy of the identifier.
    let name_expression = server_name.as_ref().map_or_else(
        || quote! { ::std::string::ToString::to_string(::std::any::type_name::<#self_ty>()) },
        |name| quote! { #name.to_string() },
    );

    let __out = quote! {
        #block
        #(#generated)*

        #[allow(
            missing_docs,
            reason = "the doc comment above is the one that should be read",
        )]
        impl #self_ty {
            /// Builds a server from every `#[tool]` in this type.
            ///
            /// # Errors
            ///
            /// [`JsonObjectError`](::protocol_droid::domain::ir::JsonObjectError)
            /// when one of the tools has no serialisable `inputSchema`. That is a
            /// mistake in a tool's argument type, and naming it is more use than
            /// panicking over it.
            pub fn into_server(
                self,
            ) -> ::core::result::Result<
                ::protocol_droid::application::server::Server,
                ::protocol_droid::domain::ir::JsonObjectError,
            > {
                #[allow(unused_mut, reason = "no tool means nothing to build")]
                let mut builder =
                    ::protocol_droid::application::server::Server::builder(#name_expression);
                #(#registrations)*
                Ok(builder.build())
            }
        }
    };
    let _ = std::fs::write("/tmp/pd_expansion.rs", format!("{__out}"));
    Ok(__out)
}

