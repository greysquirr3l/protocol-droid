//! `Auth` — how a call template says to authenticate.
//!
//! # Where the shapes come from
//!
//! The UTCP specification defines three authentication types, each an object
//! with an `auth_type` discriminator: `api_key` (a key, the header or
//! parameter it travels in, and where that is), `basic` (a username and a
//! password), and `oauth2` (a token URL, a client id, a client secret, and an
//! optional scope). Every one of them carries its secret as a *variable
//! reference* — `"${API_KEY}"` — rather than as the value, which is what
//! lets a manual be published without publishing the credential.
//!
//! # Why an `Other` variant
//!
//! A fourth type is a type this build has never heard of, and the manual it
//! came from still has to be readable. Dropping the block would lose the fact
//! that the tool needed authentication at all, which is the one thing a
//! caller has to know before it tries. So an unrecognised `auth_type` is kept
//! verbatim in [`Auth::Other`] and the caller is told which type it was.

use serde_json::{Map, Value};

use crate::domain::ir::JsonObject;
use crate::domain::utcp::error::ManualError;
use crate::domain::utcp::parse;

/// An authentication block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// A key sent in a header, a query parameter, or a cookie.
    ApiKey {
        /// The key, usually a `${VAR}` reference rather than the value.
        api_key: String,
        /// The name of the header, parameter, or cookie it travels in.
        var_name: String,
        /// Where it travels.
        location: ApiKeyLocation,
    },
    /// A username and a password.
    Basic {
        /// The username, usually a `${VAR}` reference.
        username: String,
        /// The password, usually a `${VAR}` reference.
        password: String,
    },
    /// An `OAuth2` client-credentials exchange.
    OAuth2 {
        /// Where the token is fetched from.
        token_url: String,
        /// The client identifier, usually a `${VAR}` reference.
        client_id: String,
        /// The client secret, usually a `${VAR}` reference.
        client_secret: String,
        /// The scope to ask for, when the server wants one.
        scope: Option<String>,
    },
    /// An authentication type this build does not model, kept as it arrived.
    Other {
        /// The `auth_type` discriminator.
        auth_type: String,
        /// The whole block, verbatim.
        raw: JsonObject,
    },
}

impl Auth {
    /// Reads an auth block out of a parsed document.
    ///
    /// # Errors
    ///
    /// [`ManualError::MissingField`] when a field the type requires is
    /// absent, and [`ManualError::InvalidField`] when one is present but is
    /// not the right shape.
    pub fn from_value(value: &Value) -> Result<Self, ManualError> {
        let object = parse::object(value, "authentication block")?;
        let auth_type = parse::required_str(object, "authentication block", "auth_type")?;
        match auth_type.as_str() {
            "api_key" => Ok(Self::ApiKey {
                api_key: parse::required_str(object, "authentication block", "api_key")?,
                var_name: parse::required_str(object, "authentication block", "var_name")?,
                location: ApiKeyLocation::from_str(
                    &parse::required_str(object, "authentication block", "location")?,
                )?,
            }),
            "basic" => Ok(Self::Basic {
                username: parse::required_str(object, "authentication block", "username")?,
                password: parse::required_str(object, "authentication block", "password")?,
            }),
            "oauth2" => Ok(Self::OAuth2 {
                token_url: parse::required_str(object, "authentication block", "token_url")?,
                client_id: parse::required_str(object, "authentication block", "client_id")?,
                client_secret: parse::required_str(object, "authentication block", "client_secret")?,
                scope: parse::optional_str(object, "scope"),
            }),
            _ => Ok(Self::Other {
                auth_type,
                raw: JsonObject::from_value(value.clone()).map_err(|error| {
                    ManualError::InvalidField {
                        what: "authentication block",
                        field: "auth_type",
                        detail: error.to_string(),
                    }
                })?,
            }),
        }
    }

    /// The `auth_type` this block carries.
    #[must_use]
    pub fn auth_type(&self) -> &str {
        match self {
            Self::ApiKey { .. } => "api_key",
            Self::Basic { .. } => "basic",
            Self::OAuth2 { .. } => "oauth2",
            Self::Other { auth_type, .. } => auth_type,
        }
    }
}

/// Where an API key travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyLocation {
    /// In a request header.
    Header,
    /// In a query parameter.
    Query,
    /// In a cookie.
    Cookie,
}

impl ApiKeyLocation {
    /// The spelling the specification uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Query => "query",
            Self::Cookie => "cookie",
        }
    }

    /// Reads a location out of the document.
    ///
    /// # Errors
    ///
    /// [`ManualError::InvalidField`] when the spelling is not one the
    /// specification defines.
    fn from_str(raw: &str) -> Result<Self, ManualError> {
        match raw {
            "header" => Ok(Self::Header),
            "query" => Ok(Self::Query),
            "cookie" => Ok(Self::Cookie),
            _ => Err(ManualError::InvalidField {
                what: "authentication block",
                field: "location",
                detail: format!("`{raw}` is not header, query, or cookie"),
            }),
        }
    }
}

/// The `auth` block of a call template, as the document carries it.
pub fn auth_from_map(
    object: &Map<String, Value>,
) -> Result<Option<Auth>, ManualError> {
    match object.get("auth") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Auth::from_value(value).map(Some),
    }
}

/// The `auth` block as the document carries it, for writing a manual back out.
pub fn auth_to_value(auth: &Option<Auth>) -> Option<Value> {
    let auth = auth.as_ref()?;
    let mut object = Map::new();
    match auth {
        Auth::ApiKey {
            api_key,
            var_name,
            location,
        } => {
            object.insert("auth_type".to_owned(), Value::String("api_key".to_owned()));
            object.insert("api_key".to_owned(), Value::String(api_key.clone()));
            object.insert("var_name".to_owned(), Value::String(var_name.clone()));
            object.insert(
                "location".to_owned(),
                Value::String(location.as_str().to_owned()),
            );
        }
        Auth::Basic {
            username,
            password,
        } => {
            object.insert("auth_type".to_owned(), Value::String("basic".to_owned()));
            object.insert("username".to_owned(), Value::String(username.clone()));
            object.insert("password".to_owned(), Value::String(password.clone()));
        }
        Auth::OAuth2 {
            token_url,
            client_id,
            client_secret,
            scope,
        } => {
            object.insert("auth_type".to_owned(), Value::String("oauth2".to_owned()));
            object.insert("token_url".to_owned(), Value::String(token_url.clone()));
            object.insert("client_id".to_owned(), Value::String(client_id.clone()));
            object.insert(
                "client_secret".to_owned(),
                Value::String(client_secret.clone()),
            );
            if let Some(scope) = scope {
                object.insert("scope".to_owned(), Value::String(scope.clone()));
            }
        }
        Auth::Other { raw, .. } => {
            return serde_json::from_str(raw.as_str()).ok();
        }
    }
    Some(Value::Object(object))
}
