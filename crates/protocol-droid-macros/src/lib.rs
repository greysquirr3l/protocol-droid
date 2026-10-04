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
//! and its return type becomes the answer. `#[resource]` and `#[prompt]` mark
//! a plain `fn` whose return value is the primitive itself, and need no
//! generated code beyond the registration that calls it.
//!
//! `#[server]` on an `impl` block collects every marked method below it into
//! a `Server`.
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
#[expect(
    clippy::redundant_pub_crate,
    reason = "a proc-macro crate exports no items, so 'pub' and 'pub(crate)' are both wrong and the lint that says so is right only by accident"
)]
pub(crate) mod primitives;

/// Whether `attr` is the named attribute, however it was spelled.
///
/// Matched on the **last** path segment, not the whole path, because the
/// attribute is written `#[protocol_droid::tool]` in a user crate and `#[tool]`
/// after the re-export — the same attribute, two paths, and only a whole-path
/// match would see one of them. A macro that silently ignored the qualified
/// spelling would leave every tool unmarked and produce an empty server rather
/// than an error, which is the worst outcome available: the build succeeds and
/// the tools are gone.
pub(crate) fn is_attr(attr: &syn::Attribute, name: &str) -> bool {
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
/// names (`__{Type}_{fn}_TOOL`, `__{Type}_{fn}_HANDLER`, `__{Type}_{fn}_Args`)
/// predictable: there is exactly one place that mints them, and every one of
/// them carries the type, so two servers in one module can each have an `add`.
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
// `dead_code_pub_in_binary` fires on these in this crate's own *test* build,
// and is wrong to: they are the entire public surface, consumed by
// `protocol-droid` and by its tests, and a proc macro cannot be *called* from
// inside the crate that defines it. The check sees a library exporting
// functions nobody calls, because the callers are in another crate and the
// lint does not cross the boundary.
//
// Gated on `test` because that is the only build it fires in, and an ungated
// `expect` would then be unfulfilled everywhere else — which is its own error.
#[proc_macro_attribute]
pub fn tool(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    // Checked so a malformed function is reported against this attribute. The
    // error is emitted rather than dropped: when the method sits under a
    // `#[server]`, the `#[server]` expansion has already replaced the whole
    // `impl` with a `compile_error!` and this never runs, so the author sees
    // one error rather than two. When it does not — a mark on an orphan method
    // — this is the only pass that will ever look at the function, and a
    // silently dropped parse error would leave `syn`'s "expected `fn`" with
    // nothing pointing at the author.
    parse_or_report(item)
}

/// Marks a plain `fn` returning a [`Resource`](protocol_droid::domain::registry::Resource)
/// as a resource the server lists.
///
/// This attribute generates nothing on its own, for the same reason
/// [`tool`] generates nothing: it expands inside the `impl` block, where
/// Rust forbids declaring a struct or a function. [`server`] reads the mark
/// and emits the registration.
///
/// # The shape
///
/// ```text
/// fn name() -> Resource
/// ```
///
/// A plain `fn`, not an `async fn`. A resource is registered once, while the
/// server is being built, so there is nothing to await — and an `async fn`
/// would hand `into_server` a future where a `Resource` belongs. The return
/// type is read by its last path segment, so `Resource` and
/// `crate::domain::registry::Resource` are equally good.
///
/// # What the method body is for
///
/// The whole `Resource`, including its uri, description, mime type and size.
/// That is deliberate: the body is ordinary Rust, so anything computable
/// there — a config value, a constant, a `match` — is available.
///
/// # What is *not* checked, and why
///
/// **Two methods in one `#[server]` block producing the same uri is not
/// caught at expansion time.** The uri lives in the method body, which is
/// arbitrary code, so `syn` cannot read it — a check that saw the body would
/// have to be a check that ran it. Two claims for one key are therefore
/// decided by the registry at build time, not by the macro. The detection
/// would need the uri as a `#[resource(uri = "...")]` literal, and that would
/// put the key in two places at once: the attribute and the body, with
/// nothing to say they agree. A duplicate-uri check that covers only the
/// attribute-literal spelling and reads as a general one is worse than none,
/// because it is confidently green over the case it cannot see.
///
/// A `#[resource]` outside a `#[server]` block is also unchecked, and cannot
/// be: an attribute on a method is expanded without any knowledge of the
/// `impl` around it, so it has no way to ask whether anything is reading it.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::registry::Resource;
///
/// struct Docs;
///
/// #[protocol_droid::server(name = "docs")]
/// impl Docs {
///     /// The project's readme.
///     #[protocol_droid::resource]
///     fn readme() -> Resource {
///         Resource {
///             description: Some("The project's readme.".to_string()),
///             mime_type: Some("text/markdown".to_string()),
///             ..Resource::new("file:///README.md", "readme")
///         }
///     }
/// }
/// # fn main() -> Result<(), Box<dyn std::error::Error>> { let _ = Docs.into_server()?; Ok(()) }
/// ```
#[proc_macro_attribute]
pub fn resource(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    parse_or_report(item)
}

/// Marks a plain `fn` returning a [`Prompt`](protocol_droid::domain::registry::Prompt)
/// as a prompt the server offers.
///
/// This attribute generates nothing on its own, for the same reason
/// [`tool`] generates nothing: it expands inside the `impl` block, where
/// Rust forbids declaring a struct or a function. [`server`] reads the mark
/// and emits the registration.
///
/// # The shape
///
/// ```text
/// fn name() -> Prompt
/// ```
///
/// A plain `fn`, for the same reason a [`resource`] is one: a prompt is
/// registered once, while the server is being built, so there is nothing to
/// await.
///
/// # Arguments
///
/// Declared in the body through
/// [`Prompt::with_argument`](protocol_droid::domain::registry::Prompt::with_argument),
/// not through the attribute. A prompt's arguments are an ordered list of
/// named, required-or-not entries, and a comma-separated attribute list
/// cannot express "required" without a second parallel spelling of every
/// argument's name.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::registry::Prompt;
///
/// struct Reviewer;
///
/// #[protocol_droid::server(name = "reviewer")]
/// impl Reviewer {
///     /// Asks for a review of a diff.
///     #[protocol_droid::prompt]
///     fn review() -> Prompt {
///         Prompt::new("review")
///             .with_argument("diff", true)
///             .with_argument("focus", false)
///     }
/// }
/// # fn main() -> Result<(), Box<dyn std::error::Error>> { let _ = Reviewer.into_server()?; Ok(()) }
/// ```
#[proc_macro_attribute]
pub fn prompt(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    parse_or_report(item)
}

/// Parses a marked function, reporting rather than discarding a failure.
///
/// The marked function is handed back untouched either way: a mark that
/// changed the item would be a second implementation of whatever it marks,
/// and the one that has to stay in step with `#[server]` is the other one.
fn parse_or_report(item: TokenStream) -> TokenStream {
    match syn::parse::<ItemFn>(item.clone()) {
        Ok(_) => item,
        Err(error) => error.to_compile_error().into(),
    }
}

/// Collects every marked method in an `impl` block into a `Server`.
///
/// Reads three marks: [`tool`], [`resource`] and [`prompt`]. Each is a
/// different amount of work, and the difference is the reason this macro
/// exists rather than three separate ones — the registration statements have
/// to run in one `into_server`, in source order, against one builder.
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
/// # What is checked, and what is left to the registry
///
/// Signatures. A `#[resource]` that is `async`, takes a receiver, or returns
/// something other than a `Resource` is a compile error naming the fix, and
/// the same for `#[prompt]` and `Prompt`.
///
/// Identities are not. Two `#[resource]` methods producing the same uri is
/// decided by the registry when `into_server` runs, because the uri is built
/// in a method body and a macro cannot read it without running the body. See
/// [`resource`] for why a partial check is worse than none here.
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
    match expand_server(attribute.into(), reference) {
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
    attribute: proc_macro2::TokenStream,
    block: &ItemImpl,
) -> syn::Result<proc_macro2::TokenStream> {
    let self_ty = &block.self_ty;
    let fields = args::literal_fields(attribute)?;
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

        // A `#[resource]` or `#[prompt]` is one registration statement and
        // nothing else, so it is handled and skipped rather than falling
        // through to the tool path below. Falling through would be the
        // interesting bug: a resource would be registered twice, and never
        // derive a schema.
        if let Some(mark) = primitives::Mark::of(function) {
            mark.check(function)?;
            registrations.push(mark.registration(function));
            continue;
        }

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
                attr.parse_args()
                    .unwrap_or_else(|_| proc_macro2::TokenStream::new())
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
            /// Builds a server from every marked method in this type.
            ///
            /// A `#[tool]` contributes its schema and its handler; a
            /// `#[resource]` and a `#[prompt]` contribute the value their
            /// method returns. All three are registered here, in the order
            /// they were written.
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
                #[allow(unused_mut, reason = "nothing marked means nothing to build")]
                let mut builder =
                    ::protocol_droid::application::server::Server::builder(#name_expression);
                #(#registrations)*
                Ok(builder.build())
            }
        }
    };
    Ok(__out)
}

#[cfg(test)]
mod scratch {
    use super::expand_server;
    use quote::quote;
    use syn::ItemImpl;

    #[test]
    fn debug() {
        let source = r"
            impl Docs {
                /// The project's readme.
                #[resource]
                fn readme() -> Resource { todo!() }
            }
        ";
        let Ok(block) = syn::parse_str::<ItemImpl>(source) else {
            eprintln!("PARSE ERR: {:?}", syn::parse_str::<ItemImpl>(source).err());
            return;
        };
        match expand_server(quote! { name = "docs" }, &block) {
            Ok(tokens) => eprintln!("OK: {tokens}"),
            Err(error) => eprintln!("ERR: {error}"),
        }
    }
}
