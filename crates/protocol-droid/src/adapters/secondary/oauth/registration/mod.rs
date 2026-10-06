//! Client registration over HTTP: RFC 7591 dynamic registration, and fetching
//! a Client ID Metadata Document the client published.
//!
//! Two adapters for one port, because the two mechanisms are different
//! requests. Dynamic registration is a POST the client authors and the server
//! answers, and the response is a credential only the client and that server
//! know. A metadata document is a GET of something the client published and
//! the server reads, and it establishes no secret at all.
//!
//! Both implement
//! [`ClientRegistrar`](crate::application::oauth::registration::ClientRegistrar).
//! Which one a client actually uses is decided by
//! [`plan_registration`](crate::application::oauth::registration::plan_registration),
//! in `application/` — not here. An adapter that chose for itself would make
//! the revision's preference into a property of whichever adapter was wired up.

pub mod dynamic;
pub mod metadata_document;

#[cfg(test)]
mod construction {
    use super::dynamic::Rfc7591Registrar;
    use super::metadata_document::PublishedDocumentLoader;
    use crate::application::oauth::registration::{DynamicRegistrar, MetadataDocumentLoader};

    /// Both ports are RPITIT, so they are not dyn compatible — which is the
    /// deliberate trade documented on the traits themselves. Asserting them as
    /// generic bounds records that the adapters satisfy their ports without
    /// requiring a vtable the design chose not to have.
    fn is_a_dynamic_port<T: DynamicRegistrar + ?Sized>(_: &T) {}
    fn is_a_document_port<T: MetadataDocumentLoader + ?Sized>(_: &T) {}

    /// A constructor that opened a socket or read a clock would make these
    /// tests lie about what construction costs. Worth asserting, because the
    /// answer is what lets a deployment build both adapters before it knows
    /// which one a given authorization server implements.
    #[test]
    fn both_adapters_construct_without_touching_the_network() {
        let dynamic = Rfc7591Registrar::new("https://auth.example.com/register");
        assert_eq!(dynamic.endpoint(), "https://auth.example.com/register");

        let loader = PublishedDocumentLoader::default();
        // Endpoint-free by construction, so there is nothing to assert about it
        // beyond that it exists and is usable as its port.
        is_a_dynamic_port(&dynamic);
        is_a_document_port(&loader);
    }

    /// The escape hatch for a caller that needs a timeout or a proxy. Asserted
    /// because it is the only way to bound these calls, and a deployment that
    /// cannot bound them is holding a request that may never return.
    #[test]
    fn the_http_client_can_be_replaced_on_both() {
        let http = reqwest::Client::new();
        let dynamic = Rfc7591Registrar::new("https://auth.example.com/register")
            .with_http_client(http.clone());
        let loader = PublishedDocumentLoader::new().with_http_client(http);

        assert_eq!(dynamic.endpoint(), "https://auth.example.com/register");
        let _: PublishedDocumentLoader = loader;
    }
}
