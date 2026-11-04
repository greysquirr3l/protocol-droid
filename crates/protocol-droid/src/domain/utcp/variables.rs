//! Substituting the variables a manual refers to.
//!
//! # Why the specification writes `${API_KEY}`
//!
//! A manual is a document that gets published, copied, and cached. If it
//! carried the credential itself, publishing it would be publishing the
//! secret. So the specification writes a credential as a *reference* —
//! `${API_KEY}` — and the client that makes the call resolves it against
//! whatever it holds: the environment, a secrets manager, a file the
//! operator named.
//!
//! # Why resolution fails rather than substitutes nothing
//!
//! A reference that resolves to nothing must not become an empty header or
//! an empty password. A server that receives `Authorization: ` behaves
//! differently from one that receives no header at all, and the difference
//! is a silent authentication failure at the far end. So a missing
//! variable is an error, and the call does not go out.
//!
//! The substitution itself is a pure function over a map, which keeps it
//! out of the environment and makes it testable without one.

use std::collections::BTreeMap;

/// A reference to a variable that could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VariableError {
    /// The document referred to a variable that is not set.
    #[error("`${{{name}}}` is not set")]
    Missing {
        /// The name of the variable.
        name: String,
    },
}

/// Replaces every `${name}` in `text` with the value of that variable.
///
/// A name is the text between `${` and the next `}`. A `${` with no closing
/// brace, or with an empty name, is left alone: it is not a reference, and
/// rewriting it would corrupt whatever it was.
///
/// # Errors
///
/// [`VariableError::Missing`] when a reference names a variable the map does
/// not hold.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use protocol_droid::domain::utcp::variables::substitute;
///
/// let mut variables = BTreeMap::new();
/// variables.insert("API_KEY".to_owned(), "s3cret".to_owned());
/// let resolved = substitute("Bearer ${API_KEY}", &variables)?;
/// assert_eq!(resolved, "Bearer s3cret");
/// # Ok::<(), protocol_droid::domain::utcp::variables::VariableError>(())
/// ```
pub fn substitute(
    text: &str,
    variables: &BTreeMap<String, String>,
) -> Result<String, VariableError> {
    let mut resolved = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        resolved.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // No closing brace: this is not a reference. Everything from
            // here on is literal.
            resolved.push_str(&rest[start..]);
            return Ok(resolved);
        };
        let name = &after[..end];
        if name.is_empty() {
            resolved.push_str("${");
            rest = after;
            continue;
        }
        let Some(value) = variables.get(name) else {
            return Err(VariableError::Missing {
                name: name.to_owned(),
            });
        };
        resolved.push_str(value);
        rest = &after[end + 1..];
    }
    resolved.push_str(rest);
    Ok(resolved)
}

/// Whether the text refers to any variable at all.
///
/// A caller that wants to know whether resolving is worth attempting asks
/// this rather than resolving and catching the error.
#[must_use]
pub fn refers_to_variables(text: &str) -> bool {
    text.contains("${")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map with two variables in it.
    fn variables() -> BTreeMap<String, String> {
        let mut variables = BTreeMap::new();
        variables.insert("API_KEY".to_owned(), "s3cret".to_owned());
        variables.insert("HOST".to_owned(), "api.example.com".to_owned());
        variables
    }

    #[test]
    fn a_reference_is_replaced_with_its_value() {
        let resolved = substitute("${HOST}/v1", &variables());
        assert_eq!(resolved, Ok("api.example.com/v1".to_owned()));
    }

    #[test]
    fn several_references_in_one_string_are_all_replaced() {
        let resolved = substitute("${HOST}:${API_KEY}", &variables());
        assert_eq!(resolved, Ok("api.example.com:s3cret".to_owned()));
    }

    #[test]
    fn a_string_with_no_reference_is_left_alone() {
        let resolved = substitute("https://api.example.com/users", &variables());
        assert_eq!(resolved, Ok("https://api.example.com/users".to_owned()));
    }

    #[test]
    fn a_missing_variable_is_refused_rather_than_left_empty() {
        let resolved = substitute("${NOT_SET}", &variables());
        assert_eq!(
            resolved,
            Err(VariableError::Missing {
                name: "NOT_SET".to_owned()
            })
        );
    }

    #[test]
    fn a_brace_that_never_closes_is_not_a_reference() {
        let resolved = substitute("${unclosed", &variables());
        assert_eq!(resolved, Ok("${unclosed".to_owned()));
    }

    #[test]
    fn an_empty_reference_is_not_a_reference() {
        let resolved = substitute("${}", &variables());
        assert_eq!(resolved, Ok("${}".to_owned()));
    }

    #[test]
    fn the_text_says_whether_it_refers_to_a_variable() {
        assert!(refers_to_variables("Bearer ${TOKEN}"));
        assert!(!refers_to_variables("Bearer token"));
    }
}
