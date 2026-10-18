//! The client half of the `2026-07-28` stateless lifecycle.
//!
//! SEP-2575 removed the handshake and the session. Every request now carries
//! its revision and the client's capabilities in `params._meta`, and on
//! Streamable HTTP the transport mirrors part of the body into headers so an
//! intermediary can route without parsing it:
//!
//! - `MCP-Protocol-Version`, equal to `_meta["io.modelcontextprotocol/protocolVersion"]`;
//! - `Mcp-Method`, equal to the JSON-RPC `method`;
//! - `Mcp-Name`, equal to `params.name` (`tools/call`, `prompts/get`) or
//!   `params.uri` (`resources/read`);
//! - `Mcp-Param-{Name}`, for each tool argument the tool's schema annotates
//!   with `x-mcp-header` (SEP-2243).
//!
//! All of it is a property of one body, so it is built per request here
//! rather than configured once on the connection. Source:
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http#request-metadata>
//!
//! Learning the `x-mcp-header` annotations needs the tool's schema, so this
//! client keeps the schemas from the last `tools/list` it relayed. The
//! specification also has a client drop a tool whose annotations are invalid
//! from that listing, which happens on the same pass.

use core::future::Future;
use core::pin::Pin;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};

use super::client::{HttpClient, HttpClientConfig};
use super::error::HttpClientError;
use crate::application::client::bearer::BearerAuth;
use crate::application::client::{ClientTransport, TransportError};
use crate::application::jsonrpc::document::{
    RequestError, notification_document, reply_result, request_document,
};
use crate::application::mcp::param_headers;
use crate::application::mcp::request_meta::{
    CLIENT_CAPABILITIES_KEY, CLIENT_INFO_KEY, PROTOCOL_VERSION_KEY,
};
use crate::domain::ProtocolVersion;
use crate::domain::ir::{JsonObject, MethodName};

/// A Streamable HTTP client that speaks the stateless lifecycle.
#[derive(Debug)]
pub struct StatelessHttpClient {
    inner: HttpClient,
    version: ProtocolVersion,
    capabilities: Value,
    client_info: Value,
    /// Each tool's `inputSchema`, from the last `tools/list` relayed.
    schemas: Mutex<HashMap<String, Value>>,
}

impl StatelessHttpClient {
    /// A client speaking `version`, declaring `capabilities` on every request.
    ///
    /// `capabilities` is a `ClientCapabilities` object. It is sent on every
    /// request because the server may not remember it: "Servers MUST NOT infer
    /// capabilities from prior requests".
    ///
    /// # Errors
    ///
    /// [`HttpClientError`] if the HTTP client cannot be built.
    pub fn new(
        config: HttpClientConfig,
        version: ProtocolVersion,
        capabilities: Value,
    ) -> Result<Self, HttpClientError> {
        Ok(Self {
            inner: HttpClient::new(config)?,
            version,
            capabilities,
            client_info: serde_json::json!({
                "name": "protocol-droid",
                "version": env!("CARGO_PKG_VERSION"),
            }),
            schemas: Mutex::new(HashMap::new()),
        })
    }

    /// This client, authenticating through `auth`.
    #[must_use]
    pub fn authorizing(mut self, auth: Arc<dyn BearerAuth>) -> Self {
        self.inner = self.inner.authorizing(auth);
        self
    }

    /// The revision every request is sent at.
    #[must_use]
    pub const fn version(&self) -> ProtocolVersion {
        self.version
    }

    /// `params` with this client's `_meta` keys added, keeping any the caller
    /// set itself (a progress token, a log level).
    fn with_meta(&self, params: &JsonObject) -> Value {
        let mut object: Map<String, Value> = match serde_json::from_str(params.as_str()) {
            Ok(Value::Object(object)) => object,
            _ => Map::new(),
        };
        let meta = object
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(
                PROTOCOL_VERSION_KEY.to_owned(),
                Value::from(self.version.as_wire().into_owned()),
            );
            meta.entry(CLIENT_CAPABILITIES_KEY)
                .or_insert_with(|| self.capabilities.clone());
            meta.entry(CLIENT_INFO_KEY)
                .or_insert_with(|| self.client_info.clone());
        }
        Value::Object(object)
    }

    /// The request-metadata headers for one body.
    fn headers(&self, method: &str, params: &Value) -> Vec<(String, String)> {
        let mut headers = vec![
            (
                "MCP-Protocol-Version".to_owned(),
                self.version.as_wire().into_owned(),
            ),
            ("Mcp-Method".to_owned(), method.to_owned()),
        ];
        let named = match method {
            "tools/call" | "prompts/get" => params.get("name"),
            "resources/read" => params.get("uri"),
            _ => None,
        };
        if let Some(name) = named.and_then(Value::as_str) {
            headers.push(("Mcp-Name".to_owned(), param_headers::encode(name)));
        }
        if method == "tools/call"
            && let Some(tool) = params.get("name").and_then(Value::as_str)
        {
            let schema = self
                .schemas
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(tool)
                .cloned();
            if let Some(schema) = schema {
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                headers.extend(param_headers::headers_for(&schema, &arguments));
            }
        }
        headers
    }

    /// Remembers each listed tool's schema and drops the tools whose
    /// `x-mcp-header` annotations are invalid.
    fn learn_tools(&self, result: JsonObject) -> JsonObject {
        let Ok(Value::Object(mut members)) = serde_json::from_str::<Value>(result.as_str()) else {
            return result;
        };
        let Some(Value::Array(tools)) = members.get_mut("tools") else {
            return result;
        };
        let mut schemas = self.schemas.lock().unwrap_or_else(PoisonError::into_inner);
        tools.retain(|tool| {
            let schema = tool.get("inputSchema").cloned().unwrap_or(Value::Null);
            if param_headers::has_invalid_annotation(&schema) {
                return false;
            }
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                schemas.insert(name.to_owned(), schema);
            }
            true
        });
        drop(schemas);
        JsonObject::from_value(Value::Object(members)).unwrap_or(result)
    }
}

fn unavailable(error: impl core::fmt::Display) -> TransportError {
    TransportError::Unavailable(error.to_string())
}

/// Reads an HTTP refusal whose body is a JSON-RPC error as the error it is.
///
/// A `2026-07-28` server answers `-32022`, `-32021`, `-32020` and `-32601`
/// with a `4xx` status and the error in the body. The HTTP client reports that
/// as a refused `POST`; a caller wants the code and the `data`.
fn refusal(error: HttpClientError) -> TransportError {
    if let HttpClientError::Refused { preview, .. } = &error
        && let Ok(Value::Object(body)) = serde_json::from_str::<Value>(preview)
        && let Some(Value::Object(failure)) = body.get("error")
        && let Some(code) = failure.get("code").and_then(Value::as_i64)
    {
        return TransportError::Refused {
            code,
            message: failure
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            data: failure.get("data").map(Value::to_string),
        };
    }
    unavailable(error)
}

impl ClientTransport for StatelessHttpClient {
    /// POSTs one self-describing request and returns its `result`.
    ///
    /// An `InputRequiredResult` is a result like any other and is returned as
    /// one: answering the input requests it carries is the caller's decision —
    /// it may need a person — and a transport that answered on the caller's
    /// behalf would be inventing consent.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] for a frame that cannot be assembled, a
    /// refused `POST`, or a reply carrying an `error`.
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>> {
        let prepared = (|| {
            let id = self.inner.next_request_id();
            let name = MethodName::new(method).map_err(unavailable)?;
            let params = self.with_meta(params);
            let headers = self.headers(method, &params);
            let params = JsonObject::from_value(params).map_err(unavailable)?;
            let document = request_document(&id, &name, &params).map_err(unavailable)?;
            let envelope = document
                .as_object()
                .cloned()
                .ok_or_else(|| unavailable("the request is not a JSON object"))?;
            Ok::<_, TransportError>((id, envelope, headers))
        })();
        let listing = method == "tools/list";

        Box::pin(async move {
            let (id, envelope, headers) = prepared?;
            let reply = self
                .inner
                .post_envelope_with(&envelope, &headers)
                .await
                .map_err(refusal)?
                .ok_or_else(|| unavailable("a request was answered with no body at all"))?;
            let text = serde_json::to_string(&Value::Object(reply)).map_err(unavailable)?;
            let result = reply_result(&text, &id).map_err(|error| match error {
                RequestError::PeerError {
                    code,
                    message,
                    data,
                } => TransportError::Refused {
                    code: code.code(),
                    message,
                    data,
                },
                other => unavailable(other),
            })?;
            Ok(if listing { self.learn_tools(result) } else { result })
        })
    }

    /// POSTs a notification. The core protocol defines none a client sends
    /// over this transport at this revision, but an extension may.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] if the frame cannot be assembled or the
    /// `POST` is refused.
    fn notify(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let prepared = (|| {
            let name = MethodName::new(method).map_err(unavailable)?;
            let params = self.with_meta(params);
            let headers = self.headers(method, &params);
            let params = JsonObject::from_value(params).map_err(unavailable)?;
            let document = notification_document(&name, &params).map_err(unavailable)?;
            let envelope = document
                .as_object()
                .cloned()
                .ok_or_else(|| unavailable("the notification is not a JSON object"))?;
            Ok::<_, TransportError>((envelope, headers))
        })();
        Box::pin(async move {
            let (envelope, headers) = prepared?;
            self.inner
                .post_envelope_with(&envelope, &headers)
                .await
                .map(|_| ())
                .map_err(unavailable)
        })
    }
}

#[cfg(test)]
mod tests;
