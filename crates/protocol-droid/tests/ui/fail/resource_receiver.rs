//! A `#[resource]` method that takes `&self`.
//!
//! The generated registration is `Self::readme()`, with no receiver, so
//! there is nothing for `&self` to borrow from. A method written as a method
//! rather than as a constructor is the easy mistake to make, and the failure
//! without a check is a `ServerBuilder::resource` call handed a future or a
//! method value.

/// A server whose resource is written as a method.
struct Docs {
    /// Some state the method reaches for.
    root: String,
}

#[protocol_droid::server(name = "docs")]
impl Docs {
    /// The project's readme.
    #[protocol_droid::resource]
    fn readme(&self) -> protocol_droid::domain::registry::Resource {
        protocol_droid::domain::registry::Resource::new(&self.root, "readme")
    }
}

fn main() {
    let _ = Docs { root: "file:///".to_string() };
}
