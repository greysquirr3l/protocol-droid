//! Turning a handler's parameters into a schema and a call.
//!
//! The awkward part of every function attribute is the same awkward part: a
//! Rust function takes positional parameters and a JSON object takes named
//! ones, so something has to invent a struct in between. Doing that once means
//! a fix to the invention lands in one place rather than in four macros.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, Ident, PatType, Type};

/// What a macro learned from a handler signature.
pub(crate) struct Handler {
    /// The handler's own name, which is the tool's name unless overridden.
    pub(crate) fn_name: Ident,
    /// The tool name, from `name = "..."` or the function name.
    pub(crate) tool_name: String,
    /// The description, from `description = "..."` or the doc comment.
    pub(crate) description: Option<String>,
    /// Whether the signature took a context parameter, and it was skipped.
    pub(crate) takes_context: bool,
    /// The positional parameters, in order.
    pub(crate) positional: Vec<PatType>,
    /// Whether the signature returns a `Result`.
    pub(crate) returns_result: bool,
    /// Whether the signature's answer is a `ToolResult`, bare or in a `Result`.
    ///
    /// Separate from `returns_result` because the two answer different
    /// questions: this one decides *where the result document comes from*. A
    /// `ToolResult` is already the document the wire wants, so it goes through
    /// the codec; anything else is a plain answer the crate has to shape.
    pub(crate) returns_tool_result: bool,
    /// The type's own name, which prefixes every generated item.
    pub(crate) prefix: String,
}

impl Handler {
    /// The generated argument struct's name, unique per type and handler.
    ///
    /// A method rather than a field, because the type's name is not known when
    /// the handler is read: `read_handler` sees the signature alone, and
    /// `expand_server` only afterwards knows which `impl` block it came from.
    /// A field would therefore have been frozen at the unprefixed name, and
    /// two `#[server]` types with a tool method of the same name in one module
    /// — ordinary Rust, and the thing the prefix on every *other* generated
    /// item exists to survive — would both declare `__add_Args` at module
    /// scope and fail with a duplicate definition naming a struct the author
    /// never wrote.
    pub(crate) fn args_ident(&self) -> Ident {
        format_ident!("__{}_{}_Args", self.prefix, self.fn_name)
    }
}

/// The last segment of a type path, if it is one.
///
/// A handler may write `RequestContext`, `crate::RequestContext` or
/// `protocol_droid::application::server::RequestContext`, and all three mean
/// the context. Matching the last segment is what lets the shorthand work
/// without a `use` at every call site.
pub(crate) fn last_segment(ty: &Type) -> Option<&Ident> {
    match ty {
        Type::Path(path) => path.path.segments.last().map(|segment| &segment.ident),
        _ => None,
    }
}

/// Whether `arg` is the context.
fn is_context(arg: &FnArg) -> bool {
    let FnArg::Typed(typed) = arg else {
        return false;
    };
    last_segment(&typed.ty).is_some_and(|ident| {
        let name = ident.to_string();
        name == "RequestContext" || name == "PeerSession"
    })
}

/// Reads a signature into the pieces every expansion needs.
///
/// # Errors
///
/// A `fn` that is not `async` — the generated factory awaits what the handler
/// returns, so a plain value has nothing to await and the failure would
/// otherwise arrive as a wall of type errors about the `Future` bound rather
/// than as the one edit that fixes it; a `self` parameter, because a tool is
/// called by name and a receiver would have nothing to receive from; and a
/// second context parameter, which is almost certainly a typo and which would
/// otherwise generate two fields named the same thing.
///
/// A context that is not the first parameter is also refused. The generated
/// handler is handed the context and then the decoded arguments, in that order,
/// so a `RequestContext` written after a plain argument has nowhere to go. Left
/// to the type checker this arrives as a wall of `expected fn(RequestContext,
/// String), found fn(String, RequestContext)` notes about a bound in a handler
/// factory the author never wrote, which reads as a bug in the macro rather
/// than as the parameter order — so it is caught here, where the message can
/// say which way round the two go.
pub(crate) fn read_handler(
    attrs: &[syn::Attribute],
    attribute: proc_macro2::TokenStream,
    sig: &syn::Signature,
) -> syn::Result<Handler> {
    if sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            sig,
            "a #[tool] handler is awaited when a peer calls it, so it has to be an `async fn`: \
             the generated factory calls it and awaits the future it returns, and a plain `fn` \
             answers with a value that has nothing to await. Mark it `async`, or if it really \
             does no waiting, keep the `async` — an idle future is cheaper than an unbuildable \
             server.",
        ));
    }

    let fields = literal_fields(attribute)?;
    let tool_name = field(&fields, "name").unwrap_or_else(|| sig.ident.to_string());
    let description = field(&fields, "description").or_else(|| description_from(attrs));

    let mut takes_context = false;
    let mut positional = Vec::new();
    // Whether a plain argument has been read, which is how a context that is
    // not first is recognised. The generated handler is handed the context and
    // then the decoded arguments, in that order, so the context is written
    // first; a context after an argument has nowhere to go.
    let mut seen_argument = false;

    for arg in &sig.inputs {
        match arg {
            FnArg::Receiver(receiver) => {
                return Err(syn::Error::new_spanned(
                    receiver,
                    "a #[tool] handler takes no `self`: a tool is called by name, so a \
                     receiver would have nothing to receive from. Share state through \
                     `Server::builder().with_state(value)` and read it from the context: \
                     `ctx.state::<MyType>()`.",
                ));
            }
            FnArg::Typed(typed) if is_context(arg) => {
                if takes_context {
                    return Err(syn::Error::new_spanned(
                        typed,
                        "a #[tool] handler takes at most one context parameter, and this \
                         is the second",
                    ));
                }
                if seen_argument {
                    return Err(syn::Error::new_spanned(
                        typed,
                        "a #[tool] handler takes its context first, before the arguments: \
                         the generated handler is given the context and then the decoded \
                         arguments, in that order, so a context written after an argument \
                         has nowhere to go. Move it to the front of the parameter list.",
                    ));
                }
                takes_context = true;
            }
            FnArg::Typed(typed) => {
                seen_argument = true;
                positional.push(typed.clone());
            }
        }
    }

    Ok(Handler {
        fn_name: sig.ident.clone(),
        tool_name,
        description,
        takes_context,
        positional,
        returns_result: returns_result(sig),
        returns_tool_result: returns_tool_result(sig),
        prefix: String::new(),
    })
}

/// The argument struct for a handler.
///
/// Empty for a handler that takes no parameters. A unit struct rather than
/// `{}`: a braced struct with no fields is an error in Rust, and a unit struct
/// deserialises from `{}` and from `null`, which is what a peer sends for a
/// tool it has no arguments for. `additionalProperties` is left off, so a
/// peer that sends extra members is not rejected — the arguments are this
/// tool's, not the peer's to get right.
pub(crate) fn args_struct(handler: &Handler) -> TokenStream {
    let args_ident = handler.args_ident();

    if handler.positional.is_empty() {
        // A braced struct with no fields, not a unit struct. A unit struct
        // deserialises from `null` and nothing else, so a peer sending the `{}`
        // the specification says a no-argument tool gets would be refused — and
        // it would produce a `null` schema, which tells a model the arguments
        // are the absence of a value rather than an object with no members.
        //
        // No `deny_unknown_fields`: a no-argument tool has nothing to check a
        // member against, and rejecting a call for carrying a member the tool
        // does not use would be a refusal with nothing behind it.
        return quote! {
            // No `serde(default)`: on a struct with no fields it degenerates
            // into "default the whole struct", which asks for a `Default` that
            // deserialises and is the never type. A braced struct with no
            // members already accepts `{}` and ignores anything else, which is
            // what a no-argument tool wants.
            // `serde(crate = ..)` points the derived impl's own `serde::` paths
            // at the re-export, so a user of `#[server]` needs only this crate.
            //
            // There is deliberately no matching `schemars` attribute: one exists,
            // but it redirects only the derive's helper paths and not the
            // `schemars::` references in the impl it emits, so it would buy a
            // dependency on `schemars` in the user's `Cargo.toml` and give the
            // impression it had not. `schemars` is a direct dependency of every
            // crate that uses `#[server]`, and the macro docs say so.
            #[derive(
                ::protocol_droid::schemars::JsonSchema,
                ::protocol_droid::serde::Deserialize,
                ::core::default::Default,
            )]
            #[serde(crate = "::protocol_droid::serde")]
            #[allow(
                dead_code,
                missing_docs,
                non_camel_case_types,
                reason = "generated by #[server], and never named by a user",
            )]
            struct #args_ident {}
        };
    }

    let members = handler.positional.iter().map(|field| {
        let PatType { pat, ty, .. } = field;
        quote! { #pat: #ty }
    });

    quote! {
        #[derive(
            ::protocol_droid::schemars::JsonSchema,
            ::protocol_droid::serde::Deserialize,
        )]
        #[serde(crate = "::protocol_droid::serde")]
        #[allow(
            dead_code,
            missing_docs,
            non_camel_case_types,
            reason = "generated by #[server], and never named by a user",
        )]
        struct #args_ident {
            #(#members,)*
        }
    }
}

/// The name of a tool's factory function.
pub(crate) fn tool_fn_ident(handler: &Handler) -> proc_macro2::Ident {
    format_ident!("__{}_{}_TOOL", handler.prefix, handler.fn_name)
}

/// The name of a tool's handler factory.
pub(crate) fn handler_fn_ident(handler: &Handler) -> proc_macro2::Ident {
    format_ident!("__{}_{}_HANDLER", handler.prefix, handler.fn_name)
}

/// The tool factory for one tool: its name, its description, its schema.
///
/// A function rather than a `const`, because `schemars` derives a schema by
/// trait at run time. A `const` would have meant panicking on a schema that
/// will not serialise, and a panic in generated code is a confusing way to
/// learn that an argument type has no schema.
pub(crate) fn tool_fn(handler: &Handler) -> TokenStream {
    let tool_fn = format_ident!("__{}_{}_TOOL", handler.prefix, handler.fn_name);
    let args_ident = handler.args_ident();
    let tool_name = &handler.tool_name;
    let described = handler.description.as_ref().map_or_else(
        proc_macro2::TokenStream::new,
        |text| quote! { .described(#text) },
    );

    quote! {
        #[allow(
            dead_code,
            missing_docs,
            non_snake_case,
            reason = "generated by #[server], and never named by a user",
        )]
        fn #tool_fn() -> ::core::result::Result<
            ::protocol_droid::domain::registry::Tool,
            ::protocol_droid::domain::ir::JsonObjectError,
        > {
            let schema = ::protocol_droid::schemars::schema_for!(#args_ident);
            let text = ::protocol_droid::serde_json::to_string(&schema)
                .map_err(|_| ::protocol_droid::domain::ir::JsonObjectError::NotAnObject)?;
            Ok(
                ::protocol_droid::domain::registry::Tool::new(#tool_name, &text)?
                    #described,
            )
        }
    }
}

/// The handler factory for one tool.
///
/// `f` is the user's own function, passed in rather than named here: `#[tool]`
/// expands inside an `impl` block, where it cannot see the type it is a method
/// of, so it has no way to write `Self::add`. `#[server]` does see the type and
/// passes `Self::add` in — so the one macro that knows writes the path and the
/// one that does not stays ignorant of it.
///
/// The future is returned as a named type parameter because a `Box<dyn
/// Future>` here would allocate per call on the dispatch path, which is the
/// same cost `ToolHandler` already pays once and no more.
/// The call and its two shapes: a handler returning a value, and one returning
/// a `Result`.
///
/// Split out because it is the only part of the factory that varies by return
/// type, and a body that decides the parameter list, the bounds, the decode and
/// the outcome at once is four jobs wearing one function's name.
fn outcome(handler: &Handler, call_args: &TokenStream) -> TokenStream {
    // A `ToolResult` is already the document the wire wants, so it is lowered
    // through the negotiated revision's codec rather than shaped here. This is
    // the only way a generated handler can emit a `content` array holding an
    // image, an audio block or an embedded resource: `success` builds its own
    // single text block, so every other content type was unreachable from a
    // `#[tool]` before this branch existed.
    //
    // The version comes from the session rather than being guessed, which is
    // the whole reason the codec is reached for instead of a hand-rolled
    // serialiser — the crate already knows how each revision spells a content
    // block, and a second answer here is a second opinion that is wrong in
    // exactly the cases nobody tests.
    if handler.returns_tool_result {
        // No type annotation here: `bounds` has already named the future's
        // output as `ToolResult` in place of `__Answer`, so the call's type is
        // known without the body's help. An annotation would be redundant, and
        // would also have been wrong — `__Answer` is a free parameter, so
        // annotating its value does not constrain it, it just fails to typecheck.
        if is_result(handler) {
            return quote! {
                match function(#call_args).await {
                    Ok(answer) => {
                        ::protocol_droid::adapters::secondary::codec::result::verbatim(
                            context.peer().negotiated(),
                            &answer,
                        )
                    }
                    Err(error) => {
                        Ok(::protocol_droid::application::server::result::failure(error))
                    }
                }
            };
        }
        return quote! {
            ::protocol_droid::adapters::secondary::codec::result::verbatim(
                context.peer().negotiated(),
                &function(#call_args).await,
            )
        };
    }
    if is_result(handler) {
        quote! {
            match function(#call_args).await {
                Ok(answer) => ::protocol_droid::application::server::result::success(&answer),
                Err(error) => {
                    Ok(::protocol_droid::application::server::result::failure(error))
                }
            }
        }
    } else {
        // One expression, not two statements. The caller splices this into
        // `match (#outcome) { .. }`, so anything but a single expression here
        // lands a `let` in expression position and the expansion does not
        // parse — which is what this did before, and it meant every handler
        // that did not return a `Result` failed with `expected expression,
        // found 'let' statement` pointing at the `#[server]` attribute rather
        // than at the signature that caused it.
        //
        // The binding is not needed to keep the answer alive: a temporary
        // borrowed as a call argument lives to the end of the enclosing
        // statement, and `success` returns an owned document, so nothing
        // borrows the answer past this expression.
        quote! {
            ::protocol_droid::application::server::result::success(
                &function(#call_args).await
            )
        }
    }
}

/// The generic parameters a handler factory is declared with.
///
/// Varies by return type, and has to: a type parameter that no bound and no
/// body position mentions is one the caller cannot infer, so the tool factory's
/// call to the handler factory fails with `cannot infer type of the type
/// parameter __Error` before it reaches the handler at all. That is exactly
/// what a plain `async fn f() -> T` hit — `__Error` was declared for every
/// handler but only constrained for the `Result` ones, and the parse error the
/// plain path used to also raise stood in front of this one, so neither had
/// ever been seen.
///
/// Its own function for the same reason `bounds` has one: the declaration and
/// the `where` clause have to agree about which parameters exist, and the two
/// are read side by side far more often than either is read alone.
fn generics(
    handler: &Handler,
    f_ty: &Ident,
    fut_ty: &Ident,
    error_ty: &Ident,
    answer_ty: &Ident,
) -> TokenStream {
    match (is_result(handler), handler.returns_tool_result) {
        (true, false) => quote! { <#f_ty, #fut_ty, #error_ty, #answer_ty> },
        (true, true) => quote! { <#f_ty, #fut_ty, #error_ty> },
        (false, false) => quote! { <#f_ty, #fut_ty, #answer_ty> },
        (false, true) => quote! { <#f_ty, #fut_ty> },
    }
}

/// The `where` clause a handler factory carries.
///
/// Its own function because every clause is load-bearing and the reasons are
/// the point: get one of the bounds wrong and the code still compiles, fails to
/// satisfy `ToolHandler`, or — worst — compiles and is callable once.
fn bounds(handler: &Handler, fut_ty: &Ident, error_ty: &Ident, answer_ty: &Ident) -> TokenStream {
    // A `ToolResult` is not `Serialize`, on purpose: the IR deliberately has no
    // serde impls, because the wire spelling of a content block is a
    // per-revision fact and a derived one would answer for all six revisions at
    // once. So on that path the answer is named concretely — `__Answer` is not
    // declared at all (see `generics`) — and nothing is bound on it, because
    // `verbatim` takes a `&ToolResult` and lowers it through the codec, which
    // needs no `Serialize` anywhere.
    //
    // The comma *leads* the bound rather than trailing it. `handler_factory`
    // splices this in as `#bounds_clause,`, so a trailing comma here would put
    // two commas in the expansion for every non-`Result` tool — and a `where`
    // clause with a doubled comma is a parse error pointing at the `#[server]`
    // attribute rather than at the signature that caused it.
    let concrete = quote! { ::protocol_droid::domain::ir::ToolResult };
    let answer_ty = if handler.returns_tool_result {
        concrete
    } else {
        quote! { #answer_ty }
    };
    let answer_bound = if handler.returns_tool_result {
        quote! {}
    } else {
        quote! { , #answer_ty: ::protocol_droid::serde::Serialize }
    };
    if is_result(handler) {
        quote! {
            #fut_ty: ::core::future::Future<Output = ::core::result::Result<#answer_ty, #error_ty>>
                + ::core::marker::Send,
            #error_ty: ::core::fmt::Display
            #answer_bound
        }
    } else {
        quote! {
            #fut_ty: ::core::future::Future<Output = #answer_ty> + ::core::marker::Send
            #answer_bound
        }
    }
}

pub(crate) fn handler_factory(handler: &Handler) -> TokenStream {
    let args_ident = handler.args_ident();
    let f_ty = format_ident!("__F");
    let fut_ty = format_ident!("__Fut");
    let error_ty = format_ident!("__Error");
    let answer_ty = format_ident!("__Answer");

    // The parameter list the user's function has, minus any context.
    let plain: Vec<&syn::PatType> = handler.positional.iter().collect();
    let field_names: Vec<&syn::Ident> = plain
        .iter()
        .filter_map(|field| match &*field.pat {
            syn::Pat::Ident(ident) => Some(&ident.ident),
            _ => None,
        })
        .collect();
    let field_types: Vec<&syn::Type> = plain.iter().map(|field| field.ty.as_ref()).collect();

    // `Fn(A, B) -> Fut` or `Fn(ctx, A, B) -> Fut`, matching what the user wrote.
    //
    // The two forms differ in one more way than the parameter list: a handler
    // that takes no context is given one by the `ToolHandler` and does not use
    // it, so the binding has to be read for the closure to be honest about
    // what it ignores. That read is a statement *before* the call, not a
    // fragment inside it — an expression has no room for a `let`.
    // A call's argument list is the names, comma-separated — but a list of no
    // names is `f()`, not `f(())`. Interpolating `(#{...},*)` over an empty
    // sequence yields `()`, which is a one-element tuple and a type error at
    // the call site, so the empty case is spelled out.
    let call_args = if handler.takes_context {
        quote! { context.clone(), #(#field_names),* }
    } else {
        quote! { #(#field_names),* }
    };

    // A unit struct destructures as a bare path — `let S = ..` — and only a
    // struct with fields takes `{ a, b }`. Spelling the empty case as `{}` is a
    // pattern no Rust type can match, and it is the shape every argument-less
    // tool would get.
    let binding = if field_names.is_empty() {
        quote! { #args_ident }
    } else {
        quote! { #args_ident { #(#field_names),* } }
    };

    let (fn_inputs, use_context) = if handler.takes_context {
        let mut inputs = vec![quote! { ::protocol_droid::application::server::RequestContext }];
        inputs.extend(field_types.iter().map(|ty| quote! { #ty }));
        (inputs, quote! {})
    } else {
        let inputs = field_types
            .iter()
            .map(|ty| quote! { #ty })
            .collect::<Vec<_>>();
        (inputs, quote! { let _ = &context; })
    };

    let handler_fn = format_ident!("__{}_{}_HANDLER", handler.prefix, handler.fn_name);

    let generics_clause = generics(handler, &f_ty, &fut_ty, &error_ty, &answer_ty);
    let bounds_clause = bounds(handler, &fut_ty, &error_ty, &answer_ty);
    let outcome = outcome(handler, &call_args);

    quote! {
        #[allow(
            dead_code,
            missing_docs,
            non_snake_case,
            reason = "generated by #[server], and never named by a user",
        )]
        fn #handler_fn #generics_clause (f: #f_ty) -> ::protocol_droid::application::server::ToolHandler
        where
            // `ToolHandler` is `Arc<dyn Fn .. + Send + Sync>`, so the closure
            // is `Fn` and the function it captures has to be too: an `FnOnce`
            // would be callable once, and a tool has to work twice. It also
            // has to be `Send + Sync`, because the closure is and one captured
            // value makes the closure what it is.
            //
            // `'static` for the same reason the returned future carries no
            // lifetime: `BoxFuture` is a bare `dyn Future`, which defaults to
            // `'static`, so the async block cannot borrow the function out of
            // the closure and has to own it.
            #f_ty: ::core::ops::Fn(#(#fn_inputs),*) -> #fut_ty
                + ::core::marker::Send
                + ::core::marker::Sync
                + 'static,
            #bounds_clause,
        {
            // The function behind an `Arc`, so the async block can own it and
            // the closure stays `Fn`. One atomic increment per call, against a
            // heap allocation per call if the block could not borrow instead —
            // which it cannot, because `BoxFuture` is `'static`.
            let function = ::std::sync::Arc::new(f);
            ::std::sync::Arc::new(move |context, arguments| {
                let function = ::std::sync::Arc::clone(&function);
                Box::pin(async move {
                    // Parsed, not validated. serde's message names the field and
                    // the reason, which is the part a caller can act on; a
                    // second shorter vocabulary would throw it away.
                    let #binding: #args_ident = match ::protocol_droid::serde_json::from_str(
                        arguments.as_str()
                    ) {
                        Ok(decoded) => decoded,
                        Err(error) => {
                            return
                                ::protocol_droid::application::server::result::failure(
                                    ::protocol_droid::application::server::result::OutcomeError::Arguments(
                                        error.to_string(),
                                    ),
                                );
                        }
                    };
                    #use_context

                    match (#outcome) {
                        Ok(document) => document,
                        // The answer will not serialise, which is the handler's
                        // own types at fault rather than the peer's request.
                        // Still an error *result* and not a transport failure:
                        // the call arrived and the tool could not answer.
                        Err(error) => {
                            ::protocol_droid::application::server::result::failure(error)
                        }
                    }
                })
            })
        }
    }
}

/// Whether this handler's signature returns a `Result`.
///
/// By shape, not by resolving the type: the expansion only needs to know
/// whether the call yields a value or a value and an error, and a type alias
/// spelling `Result` under another name fails to compile at the call site,
/// which is a better place to find out than here.
pub(crate) const fn is_result(handler: &Handler) -> bool {
    handler.returns_result
}

/// Whether a signature returns a `Result`.
///
/// By shape, not by resolving the type: the expansion only needs to know
/// whether the call yields a value or a value and an error, and a type alias
/// spelling `Result` under another name still fails to compile at the call
/// site, which is a better place to find out than here.
pub(crate) fn returns_result(sig: &syn::Signature) -> bool {
    match &sig.output {
        syn::ReturnType::Default => false,
        syn::ReturnType::Type(_, ty) => last_segment(ty).is_some_and(|ident| ident == "Result"),
    }
}

/// Whether a signature's answer is a `ToolResult`.
///
/// Matches by shape, the same way [`returns_result`] does, and for the same
/// reason: the expansion decides which of two *shape* of answer it is holding,
/// and a type alias spelling `ToolResult` under another name fails to compile
/// at the call site, which is the better place to find out.
///
/// Both spellings count, because a handler that can fail and one that cannot
/// are the same decision with an error arm wrapped round it. Reading only the
/// bare form would make `-> Result<ToolResult, String>` fall back to the
/// `Serialize` path and produce a `structuredContent` wrapping the whole
/// result — which compiles, serialises, and is wrong on the wire.
pub(crate) fn returns_tool_result(sig: &syn::Signature) -> bool {
    let syn::ReturnType::Type(_, ty) = &sig.output else {
        return false;
    };
    // `ReturnType::Type` hands back a `Box<Type>`. A function argument would
    // coerce that to `&Type` on its own, but a *pattern* is not a coercion
    // site, so the deref has to happen before the match rather than at it.
    let ty = ty.as_ref();
    if last_segment(ty).is_some_and(|ident| ident == "ToolResult") {
        return true;
    }
    let Type::Path(path) = ty else {
        return false;
    };
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    if segment.ident != "Result" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return false;
    };
    arguments.args.iter().any(|argument| match argument {
        // `GenericArgument::Type` holds a `Type` directly, so the binding
        // under a `&GenericArgument` is already the `&Type` `last_segment`
        // wants. No coercion, unlike the `Box` the return type is wrapped in.
        syn::GenericArgument::Type(inner) => {
            last_segment(inner).is_some_and(|ident| ident == "ToolResult")
        }
        _ => false,
    })
}

/// The `name = "..."`, `description = "..."` pairs on an attribute.
///
/// Parsed from the attribute's own token stream rather than from an
/// `Attribute` value the caller rebuilt. Round-tripping through a synthetic
/// `#[tool(..)]` to read it back would mean the two spellings of the same
/// argument — the one the user wrote and the one the macro invented — could
/// parse differently, and a difference there is invisible.
pub(crate) fn literal_fields(
    attribute: proc_macro2::TokenStream,
) -> syn::Result<Vec<(String, syn::LitStr)>> {
    if attribute.is_empty() {
        return Ok(Vec::new());
    }
    let nested = syn::parse::Parser::parse2(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        attribute,
    )?;

    let mut found = Vec::new();
    for entry in nested {
        let syn::Meta::NameValue(pair) = entry else {
            return Err(syn::Error::new_spanned(
                &entry,
                "expected a named value, for example name = \"my-tool\"",
            ));
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) = &pair.value
        else {
            return Err(syn::Error::new_spanned(
                &pair.value,
                "expected a string literal, for example name = \"my-tool\"",
            ));
        };
        found.push((
            pair.path
                .get_ident()
                .map_or_else(String::new, ToString::to_string),
            text.clone(),
        ));
    }
    Ok(found)
}

/// One of the parsed `name = "..."` pairs.
pub(crate) fn field(fields: &[(String, syn::LitStr)], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.value())
}

/// The description a doc comment gives, as one line.
///
/// The first paragraph only, and its lines joined with spaces. A tool's
/// description goes into a schema a model reads, and three paragraphs of
/// rustdoc pasted there cost context without informing anything. The join is
/// the other half: a one-line-per-paragraph description reads as a list.
pub(crate) fn description_from(attrs: &[syn::Attribute]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let syn::Meta::NameValue(pair) = &attr.meta else {
            continue;
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) = &pair.value
        else {
            continue;
        };
        let line = text.value();
        if line.trim().is_empty() && !lines.is_empty() {
            break;
        }
        if !line.trim().is_empty() {
            lines.push(line.trim().to_string());
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join(" "))
    }
}
