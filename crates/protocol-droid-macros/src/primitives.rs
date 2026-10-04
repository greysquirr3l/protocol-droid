//! The `#[resource]` and `#[prompt]` marks.
//!
//! Both are marks for the reason `#[tool]` is one: a method's attribute
//! expands *inside* the `impl` block, where Rust forbids declaring a struct or
//! a function and where even a `const _` collides with every other `const _`
//! in the same `impl`. `#[server]` reads the mark and emits the registration.
//!
//! # Why these are much smaller than `#[tool]`
//!
//! A tool is a schema plus a handler. Its parameters become a JSON Schema
//! derived through `schemars`, and its body is awaited on a `BoxFuture` that
//! owns the function behind an `Arc` because `BoxFuture` is `'static`. All of
//! that is work, and none of it is work a resource or a prompt needs.
//!
//! `ServerBuilder::resource` and `ServerBuilder::prompt` each take one value
//! and nothing else. So a method marked with either returns that value and is
//! called once, synchronously, while `into_server` is still building the
//! server. Which is also why the shape is a plain `fn`: a primitive is
//! registered before the server exists, so there is nothing to await, and an
//! `async fn` would hand `into_server` a future where a `Resource` belongs.
//!
//! # What is generated
//!
//! One statement per marked method: `builder = builder.resource(Self::name());`.
//! Nothing is minted at module scope, so unlike `#[tool]` there is no
//! generated name for `Demo::a` and `Other::a` to collide over — there is no
//! generated name at all.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItemFn};

use crate::args::last_segment;

/// A method attribute that produces a registry entry rather than a handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    /// `#[resource]`, on a method returning a `Resource`.
    Resource,
    /// `#[prompt]`, on a method returning a `Prompt`.
    Prompt,
}

impl Mark {
    /// The attribute's own name, as the author spells it.
    pub(crate) const fn attribute(self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::Prompt => "prompt",
        }
    }

    /// The type the marked method has to return.
    pub(crate) const fn returns(self) -> &'static str {
        match self {
            Self::Resource => "Resource",
            Self::Prompt => "Prompt",
        }
    }

    /// The `ServerBuilder` method that registers one.
    pub(crate) const fn builder_method(self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::Prompt => "prompt",
        }
    }

    /// The mark `method` carries, if it carries one.
    ///
    /// Matched on the last path segment rather than the whole path, so
    /// `#[protocol_droid::resource]` and the re-exported `#[resource]` are one
    /// attribute. A macro that saw only the unqualified spelling would ignore
    /// every qualified one, and the symptom would be a server quietly missing
    /// a resource.
    pub(crate) fn of(method: &ImplItemFn) -> Option<Self> {
        method.attrs.iter().find_map(|attr| {
            if crate::is_attr(attr, "resource") {
                Some(Self::Resource)
            } else if crate::is_attr(attr, "prompt") {
                Some(Self::Prompt)
            } else {
                None
            }
        })
    }

    /// Rejects a method this mark cannot register.
    ///
    /// # Errors
    ///
    /// An `async` method, one taking a receiver, and one returning anything
    /// other than the primitive this mark names. All three compile far enough
    /// to fail confusingly further on — a future registered as a resource, a
    /// `Self::name()` call with no receiver to supply one from, a
    /// `ServerBuilder` method handed a type it does not take — so each is
    /// caught here, where the message can say which half of the signature to
    /// change.
    pub(crate) fn check(self, method: &ImplItemFn) -> syn::Result<()> {
        let attribute = self.attribute();
        let expected = self.returns();
        let name = method.sig.ident.clone();

        if let Some(asyncness) = &method.sig.asyncness {
            return Err(syn::Error::new_spanned(
                asyncness,
                format!(
                    "a #[{attribute}] method is registered once, while the server is being \
                     built, so there is nothing to await: write `fn {name}() -> {expected}` \
                     without `async`. A function that has to wait is a #[tool], and that one is \
                     called per request rather than per server."
                ),
            ));
        }

        if let Some(FnArg::Receiver(receiver)) = method.sig.inputs.first() {
            return Err(syn::Error::new_spanned(
                receiver,
                format!(
                    "a #[{attribute}] method is registered as `Self::{name}()`, and that call \
                     has no receiver to supply one from: take no `self` argument. A primitive \
                     that needs state to describe itself belongs in `into_server` itself, or is \
                     added through `ServerBuilder::{}` where the value can be passed directly.",
                    self.builder_method()
                ),
            ));
        }

        let syn::ReturnType::Type(_, ty) = &method.sig.output else {
            return Err(syn::Error::new_spanned(
                &method.sig,
                format!(
                    "a #[{attribute}] method must return `{expected}`: it is how the server \
                     describes the {} to a peer, and a method returning nothing describes none.",
                    lower_first(expected)
                ),
            ));
        };

        match last_segment(ty) {
            Some(ident) if ident == expected => Ok(()),
            Some(ident) => Err(syn::Error::new_spanned(
                ty,
                format!(
                    "a #[{attribute}] method must return `{expected}`, not `{ident}`: \
                     `ServerBuilder::{}` registers a `{expected}`, and the method is called \
                     once to produce it. A function that runs per request is a #[tool].",
                    self.builder_method()
                ),
            )),
            None => Err(syn::Error::new_spanned(
                ty,
                format!(
                    "a #[{attribute}] method must return `{expected}`, and this return type is \
                     not a path the macro can read a name from: write `{expected}` or a path \
                     ending in `{expected}`."
                ),
            )),
        }
    }

    /// The statement `into_server` runs for one marked method.
    ///
    /// Takes the method rather than its name so the generated `Self::name()`
    /// is written where the type is in scope. `#[resource]` cannot see it — it
    /// expands inside the `impl` — which is the same blindness `#[tool]`
    /// works around by having `#[server]` pass the path in.
    pub(crate) fn registration(self, method: &ImplItemFn) -> TokenStream {
        // `builder_method` is a `&'static str` because the same name goes
        // into `format!` error messages, where a string is what is wanted.
        // Interpolating it into `quote!` is not: quote renders a `&str` as a
        // *string literal*, so `builder.#registers(..)` expands to
        // `builder."resource"(..)` and rustc rejects it at the `#[server]`
        // span. The name has to become an `Ident` first.
        let registers = format_ident!("{}", self.builder_method());
        let name = &method.sig.ident;
        quote! {
            builder = builder.#registers(Self::#name());
        }
    }
}

/// Lowercases a type name's first letter, for prose.
///
/// `Resource` reads mid-sentence and `resource` reads at the start of one. A
/// message saying "describes the Resource to a peer" is correct and reads as a
/// typo, which is a small thing to be wrong about in a message whose only job
/// is to be read once.
fn lower_first(name: &str) -> String {
    let mut characters = name.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(characters).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::{Mark, lower_first};
    use crate::args::last_segment;
    use syn::{ImplItemFn, ItemImpl, Type};

    /// What a test here can fail with.
    ///
    /// One enum with a `From` for the one error type in play, so a `?` on a
    /// `syn::Error` is a compile error until the conversion is written. A test
    /// that swallows a broken fixture and then asserts on the result is worse
    /// than one that does not compile.
    #[derive(Debug)]
    enum Failure {
        /// The fixture was not the Rust this test meant to write.
        Fixture(syn::Error),
        /// The macro accepted something this test meant to see refused.
        Accepted(&'static str),
        /// The macro refused something this test meant to see accepted.
        Refused(syn::Error),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Fixture(error) => write!(f, "bad fixture: {error}"),
                Self::Accepted(what) => write!(f, "the macro accepted {what}"),
                Self::Refused(error) => write!(f, "the macro refused: {error}"),
            }
        }
    }

    impl From<syn::Error> for Failure {
        fn from(error: syn::Error) -> Self {
            Self::Fixture(error)
        }
    }

    /// The methods of an `impl`, in order.
    fn methods(source: &str) -> syn::Result<Vec<ImplItemFn>> {
        let block: ItemImpl = syn::parse_str(source)?;
        Ok(block
            .items
            .into_iter()
            .filter_map(|item| match item {
                syn::ImplItem::Fn(function) => Some(function),
                _ => None,
            })
            .collect())
    }

    /// The methods of an `impl` that is known to hold exactly one.
    fn one_method(source: &str) -> Result<ImplItemFn, Failure> {
        let mut found = methods(source)?;
        match (found.len(), found.pop()) {
            (1, Some(function)) => Ok(function),
            (count, _) => Err(Failure::Fixture(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("the fixture should hold one method, and holds {count}"),
            ))),
        }
    }

    #[test]
    fn either_spelling_of_the_attribute_is_the_same_mark() -> Result<(), Failure> {
        let qualified =
            one_method("impl Demo { #[protocol_droid::resource] fn a() -> Resource { todo() } }")?;
        let bare = one_method("impl Demo { #[resource] fn a() -> Resource { todo() } }")?;

        assert_eq!(Mark::of(&qualified), Mark::of(&bare));
        Ok(())
    }

    #[test]
    fn an_unmarked_method_carries_no_mark() -> Result<(), Failure> {
        let plain = one_method("impl Demo { fn a() -> crate::Resource { todo() } }")?;

        assert_eq!(Mark::of(&plain), None);
        Ok(())
    }

    #[test]
    fn a_tool_is_not_a_primitive_mark() -> Result<(), Failure> {
        // `#[tool]` and `#[resource]` are read by different halves of
        // `expand_server`. A `Mark` for a `#[tool]` would register it twice and
        // never derive its schema.
        let tool = one_method("impl Demo { #[tool] async fn a() -> String { todo() } }")?;

        assert_eq!(Mark::of(&tool), None);
        Ok(())
    }

    #[test]
    fn each_mark_names_the_type_it_registers() {
        assert_eq!(Mark::Resource.returns(), "Resource");
        assert_eq!(Mark::Prompt.returns(), "Prompt");
        assert_eq!(Mark::Resource.builder_method(), "resource");
        assert_eq!(Mark::Prompt.builder_method(), "prompt");
        assert_eq!(Mark::Resource.attribute(), "resource");
        assert_eq!(Mark::Prompt.attribute(), "prompt");
    }

    #[test]
    fn a_well_formed_method_is_accepted() -> Result<(), Failure> {
        for source in [
            "impl Demo { #[resource] fn readme() -> Resource { todo() } }",
            "impl Demo { #[prompt] fn summarise() -> Prompt { todo() } }",
            // A path is read by its last segment, so a user does not have to
            // import the type to have it checked.
            "impl Demo { #[resource] fn a() -> crate::registry::Resource { todo() } }",
        ] {
            let method = one_method(source)?;
            let mark = Mark::of(&method).ok_or(Failure::Accepted("a marked method"))?;
            if let Err(error) = mark.check(&method) {
                return Err(Failure::Refused(error));
            }
        }
        Ok(())
    }

    #[test]
    fn the_rejection_names_the_mark_the_type_written_and_the_type_wanted() -> Result<(), Failure> {
        let method = one_method("impl Demo { #[prompt] fn a() -> String { todo() } }")?;
        let Some(error) = Mark::Prompt.check(&method).err() else {
            return Err(Failure::Accepted("a prompt returning String"));
        };

        // The four things a reader needs: which mark, which type was wanted,
        // what was written, and what to use instead.
        let message = error.to_string();
        for expected in ["#[prompt]", "Prompt", "String", "#[tool]"] {
            assert!(message.contains(expected), "message was: {message}");
        }
        Ok(())
    }

    #[test]
    fn a_type_the_macro_cannot_read_a_name_from_is_refused() -> Result<(), Failure> {
        // `-> impl Trait` has no last segment. Accepting it would mean
        // registering whatever it happens to resolve to.
        let method = one_method("impl Demo { fn a() -> impl Sized {} }")?;

        assert!(Mark::Resource.check(&method).is_err());
        Ok(())
    }

    #[test]
    fn a_method_returning_nothing_is_refused() -> Result<(), Failure> {
        let method = one_method("impl Demo { #[resource] fn a() {} }")?;

        assert!(Mark::Resource.check(&method).is_err());
        Ok(())
    }

    #[test]
    fn a_receiver_is_refused_and_the_message_points_at_the_builder() -> Result<(), Failure> {
        let method = one_method("impl Demo { #[resource] fn a(&self) -> Resource { todo() } }")?;
        let Some(error) = Mark::Resource.check(&method).err() else {
            return Err(Failure::Accepted("a resource method taking &self"));
        };

        assert!(error.to_string().contains("ServerBuilder::resource"));
        Ok(())
    }

    #[test]
    fn the_return_type_checked_is_the_one_in_the_signature() -> Result<(), Failure> {
        // The body never mentions its return type, and reading the body would
        // mean running it, so the check has to come from the signature.
        let method = one_method("impl Demo { #[resource] fn a() -> Resource { todo() } }")?;
        let syn::ReturnType::Type(_, ty) = &method.sig.output else {
            return Err(Failure::Fixture(syn::Error::new(
                proc_macro2::Span::call_site(),
                "the fixture declares no return type",
            )));
        };

        assert!(matches!(&**ty, Type::Path(_)));
        assert_eq!(
            last_segment(ty).map(ToString::to_string).as_deref(),
            Some("Resource")
        );
        Ok(())
    }

    #[test]
    fn a_type_name_lowercases_only_its_first_letter() {
        for (name, expected) in [
            ("Resource", "resource"),
            ("Prompt", "prompt"),
            ("ResourceIcon", "resourceIcon"),
            ("", ""),
        ] {
            assert_eq!(lower_first(name), expected, "for {name}");
        }
    }
}
