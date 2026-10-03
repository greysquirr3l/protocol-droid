//! Generating JSON values that a given schema accepts.
//!
//! # Why generation is schema-directed rather than type-directed
//!
//! The obvious property test builds an `Arbitrary` impl per generated Rust type.
//! That is not available here: the types are produced by a build script, so an
//! `Arbitrary` impl would have to be emitted too, and a generator that can only
//! produce values for the shapes it was itself taught is weaker evidence than
//! one driven by the schema.
//!
//! Reading the vendored `schema.json` and working backwards is the stronger
//! direction, and it is what the task asks for: an arbitrary value that
//! *validates* against the specification. The generated Rust type then has to
//! accept it, which is a claim about the type rather than about this generator.
//!
//! # The keyword surface
//!
//! Deliberately not a general JSON Schema implementation. The vendored schemas
//! use `type`, `$ref`, `properties`, `required`, `const`, `enum`, `anyOf`,
//! `oneOf`, `allOf`, `items`, `additionalProperties`, `minimum`, `maximum`,
//! `minLength` and `minItems` — and nothing else. No `not`, `if`, `pattern`,
//! `patternProperties` or `uniqueItems`. A keyword this module does not model
//! falls through to `any_value()`, and the `jsonschema` check in the test is
//! what reports it, rather than this module silently narrowing coverage.
//!
//! # The recursive cases
//!
//! `JSONRPCMessage` contains `Vec<JSONRPCMessage>` and `JSONValue` contains
//! itself, so an unbounded walk never terminates. [`MAX_DEPTH`] bounds it, and
//! the `$ref` path currently being expanded is tracked so a cycle resolves
//! rather than recursing. At the bound the walk returns the shallowest value
//! the schema accepts, which is why `anyOf` prefers its first branch: the
//! recursive definitions in these schemas list a non-recursive alternative
//! first.

use std::collections::BTreeMap;
use std::rc::Rc;

use proptest::prelude::*;
use serde_json::{Map, Value, json};

/// How deep the walk descends before it starts returning shallow values.
///
/// Deep enough to reach a nested message's own fields, shallow enough that a
/// recursive definition terminates. Four covers every nesting these schemas
/// use; raising it would only make the suite slower.
const MAX_DEPTH: usize = 4;

/// The definitions container of a schema, keyed by definition name.
pub type Defs = BTreeMap<String, Value>;

/// The definition container this schema uses.
///
/// `2024-11-05` through `2025-06-18` are draft-07 and use `definitions`; the
/// later revisions are 2020-12 and use `$defs`. Guessing wrong would make every
/// `$ref` dangle, so this is read from the file rather than assumed.
pub fn defs_of(schema: &Value) -> Defs {
    schema
        .get("$defs")
        .or_else(|| schema.get("definitions"))
        .and_then(Value::as_object)
        .map(|members| {
            members
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// A strategy producing values for the definition called `name`.
///
/// `None` when the revision has no such definition, so a case naming a
/// definition the schema does not have fails with a message about the case list
/// rather than with a generator panic.
pub fn value_for_def(name: &str, defs: &Defs) -> Option<BoxedStrategy<Value>> {
    let schema = defs.get(name)?.clone();
    Some(value_at(&schema, defs, 0, &mut Vec::new()))
}

/// An unconstrained JSON value, for a schema keyword this module does not
/// model.
///
/// `serde_json::Value` has no `Arbitrary` impl, and this is the only place one
/// is wanted: a schema that constrains nothing, or one whose constraint is a
/// keyword the module ignores, still has to produce something. Without it those
/// branches would return a constant and the suite would pass on them vacuously
/// while claiming coverage.
///
/// The shape is deliberately lopsided towards small scalars. Real payloads are
/// mostly strings, numbers and booleans, and a uniformly random JSON tree
/// spends nearly all its budget on nesting that the round trip cannot
/// distinguish.
fn any_value() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        6 => prop::sample::select(vec!["", "a", "hello", "x-unknown"])
            .prop_map(|s| Value::String(s.to_owned())),
        3 => any::<i64>().prop_map(|n| json!(n)),
        3 => any::<bool>().prop_map(Value::Bool),
        1 => Just(Value::Null),
    ];
    leaf.prop_recursive(2, 8, 3, move |inner| {
        prop_oneof![
            6 => inner.clone(),
            1 => proptest::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            1 => proptest::collection::hash_map("[a-zA-Z]{1,8}", inner, 0..3)
                .prop_map(|members| Value::Object(members.into_iter().collect())),
        ]
    })
    .boxed()
}

/// The walk, carrying a depth and the `$ref` path currently being expanded.
///
/// Sub-strategies are built eagerly, before this returns, and none of them
/// captures `schema`, `defs` or the `visiting` borrow: a `BoxedStrategy` is
/// `'static`, and `prop_flat_map` takes an `Fn`, which cannot mutate through a
/// captured `&mut`. That is what lets all three be plain references here.
fn value_at(
    schema: &Value,
    defs: &Defs,
    depth: usize,
    visiting: &mut Vec<String>,
) -> BoxedStrategy<Value> {
    let Some(map) = schema.as_object() else {
        // A bare `true` schema accepts anything; a bare `false` one accepts
        // nothing and cannot be satisfied. Neither appears in these files, and
        // `any_value` is the sound choice for the first.
        return any_value();
    };

    // `const` and `enum` are the only keywords that pin an exact value, so they
    // are checked before anything tries to invent one.
    if let Some(value) = map.get("const") {
        return Just(value.clone()).boxed();
    }
    if let Some(Value::Array(values)) = map.get("enum") {
        return prop::sample::select(values.clone()).boxed();
    }

    // `anyOf`/`oneOf`: a value satisfying one branch satisfies the union, so
    // sampling a branch and generating for it is sound.
    for keyword in ["anyOf", "oneOf"] {
        let Some(Value::Array(branches)) = map.get(keyword) else {
            continue;
        };
        if branches.is_empty() {
            continue;
        }
        // At the bound the first branch is preferred: the recursive definitions
        // in these schemas list a non-recursive alternative first, so this is
        // what stops a recursive union expanding forever.
        if depth >= MAX_DEPTH {
            // The branches list is known non-empty here, so `first` is `Some`.
            // Indexing would be the obvious way to say that, and panicking is
            // denied.
            let Some(first) = branches.first() else {
                return any_value();
            };
            return value_at(first, defs, depth, visiting);
        }
        let built: Vec<BoxedStrategy<Value>> = branches
            .iter()
            .map(|branch| value_at(branch, defs, depth + 1, visiting))
            .collect();
        let count = built.len();
        // Indexed rather than sampled by value: a strategy is not `Clone` in
        // general, only clonable through a `Box`, and `select` needs the values
        // up front. The index is drawn from exactly `0..count`, so the lookup
        // cannot miss — the `None` arm is there because the compiler insists,
        // not because it can happen.
        return (0..count)
            .prop_flat_map(move |i| built.get(i).map_or_else(any_value, Clone::clone))
            .boxed();
    }

    // `allOf`: every branch must hold at once, which for the object schemas in
    // these files means one merged object satisfies all of them.
    if let Some(Value::Array(branches)) = map.get("allOf")
        && !branches.is_empty()
    {
        return fold_over(
            branches
                .iter()
                .map(|branch| value_at(branch, defs, depth + 1, visiting)),
        );
    }

    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        return resolve(reference, defs, depth, visiting);
    }

    match chosen_type(map) {
        Some("array") => array_value(schema, defs, depth, visiting),
        Some("string") => {
            let min = bound(map.get("minLength"), 64);
            map.get("format")
                .and_then(Value::as_str)
                .map_or_else(|| string_value(min), |format| formatted_string(format, min))
        }
        Some("integer") => integer_value(map),
        Some("number") => number_value(map),
        Some("boolean") => any::<bool>().prop_map(Value::Bool).boxed(),
        Some("null") => Just(Value::Null).boxed(),
        // `object` is named explicitly rather than falling into the `Some(_)`
        // arm below: every named type has an arm of its own, and a type this
        // module does not model is a schema keyword gap, not a licence to emit
        // a scalar for something the spec says is an object.
        Some("object") | None => object_value(schema, defs, depth, visiting),
        // A `type` list such as `["string", "integer"]` takes its first member
        // above. Reaching here means a named type the vendored schemas do not
        // use. Generating for it is what a schema keyword gap looks like, and
        // the `jsonschema` check is what reports it.
        Some(_) => any_value(),
    }
}

/// The single type this schema names, from `"type": "x"` or `"type": ["x", ..]`.
///
/// A list means "any one of these", so the first member is chosen. Sampling
/// from the whole list would need the remaining keywords re-applied per member,
/// for no extra coverage of the round trip under test.
fn chosen_type(map: &Map<String, Value>) -> Option<&str> {
    match map.get("type") {
        Some(Value::String(name)) => Some(name),
        Some(Value::Array(names)) => names.first().and_then(Value::as_str),
        _ => None,
    }
}

/// Follow a `$ref` into `defs`.
///
/// A reference to a definition already on the expansion path resolves to the
/// empty object. That is valid wherever the definition has no required members,
/// and is the only terminating answer otherwise; the `jsonschema` check in the
/// test reports it when it is not good enough, which is the point of validating
/// at all.
///
/// The depth is *not* charged for the hop. Depth bounds structural nesting —
/// descending through `properties` and `items` — and charging it here made a
/// four-hop reference chain exhaust the budget before reaching a leaf: `Role`
/// is a one-line `enum`, and resolving it costs nothing. Termination comes from
/// the `visiting` guard instead, which is a proper depth-first cycle check over
/// a definition graph that is finite once cycles are cut.
fn resolve(
    reference: &str,
    defs: &Defs,
    depth: usize,
    visiting: &mut Vec<String>,
) -> BoxedStrategy<Value> {
    let Some(name) = reference.rsplit('/').next() else {
        return any_value();
    };
    if visiting.iter().any(|seen| seen == name) {
        return Just(Value::Object(Map::new())).boxed();
    }
    let Some(target) = defs.get(name) else {
        return any_value();
    };
    visiting.push(name.to_owned());
    let strategy = value_at(target, defs, depth, visiting);
    visiting.pop();
    strategy
}

/// Sample every strategy in turn and merge the results into one value.
///
/// A left fold of `prop_flat_map` rather than a tuple, because the number of
/// branches is not known when the strategy is built and this handles any count.
fn fold_over<I>(parts: I) -> BoxedStrategy<Value>
where
    I: IntoIterator<Item = BoxedStrategy<Value>>,
{
    parts.into_iter().fold(
        Just(Value::Object(Map::new())).boxed(),
        |acc: BoxedStrategy<Value>, part: BoxedStrategy<Value>| {
            // One `flat_map`, not two: the accumulator arrives as a *value*
            // inside the closure and only the part is sampled there, so there
            // is no second strategy left to flatten.
            //
            // The accumulator is cloned per sample because `prop_map` takes an
            // `Fn` — the closure runs more than once, so it cannot consume a
            // capture. Ten `allOf` branches across the whole vendored set is the
            // only place this runs, so the deep clone does not matter. The `Rc`
            // is what keeps the strategy `'static`: a borrow of the parameter
            // would not outlive the fold. `Arc` would ask for `Send + Sync` that
            // `BoxedStrategy` does not promise.
            let part = Rc::new(part);
            acc.prop_flat_map(move |accumulated: Value| {
                let part = Rc::clone(&part);
                part.prop_map(move |part| merge(accumulated.clone(), part))
            })
            .boxed()
        },
    )
}

/// Deep-merge `incoming` into `base`, merging objects member-wise.
fn merge(base: Value, incoming: Value) -> Value {
    match (base, incoming) {
        (Value::Object(mut base), Value::Object(other)) => {
            for (key, value) in other {
                // A `match` rather than `map_or_else`: the `else` branch would
                // have to move `value` while the closure above it still borrows
                // it, and the borrow checker is right to refuse.
                let merged = match base.remove(&key) {
                    Some(existing) => merge(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (_, incoming) => incoming,
    }
}

/// An object value: every required member, a sampled subset of the rest, and
/// possibly one member the schema does not name.
fn object_value(
    schema: &Value,
    defs: &Defs,
    depth: usize,
    visiting: &mut Vec<String>,
) -> BoxedStrategy<Value> {
    let Some(map) = schema.as_object() else {
        return any_value();
    };
    let required: Vec<String> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    // Each member is paired with its own name so a single draw can assemble the
    // whole object. That matters: a per-member chain would multiply the tree by
    // the property count, and these definitions have up to twenty.
    let required_members: Vec<(String, BoxedStrategy<Value>)> = required
        .iter()
        .filter_map(|name| {
            let member = map.get("properties")?.get(name)?;
            Some((name.clone(), value_at(member, defs, depth + 1, visiting)))
        })
        .collect();
    let required_part = draw_all(required_members);

    // Optional members are drawn with a length of 0..=count, so the suite covers
    // a value that omits an optional member as well as one that includes it.
    // Omitting is the case that catches a field emitted without
    // `skip_serializing_if`, which would come back as an explicit `null` the
    // peer never sent.
    let optional_members: Vec<(String, BoxedStrategy<Value>)> = map
        .get("properties")
        .and_then(Value::as_object)
        .map(|all| {
            all.iter()
                .filter(|(name, _)| !required.contains(name))
                .map(|(name, member)| (name.clone(), value_at(member, defs, depth + 1, visiting)))
                .collect()
        })
        .unwrap_or_default();
    let optional_count = optional_members.len();
    let optional_part = (0..=optional_count)
        .prop_flat_map(move |keep| {
            // Every optional member is drawn, then the first `keep` are kept.
            // Drawing the surplus costs one sample per member and avoids the
            // index bookkeeping that drawing a subset directly would need.
            draw_all(optional_members.clone())
                .prop_map(move |drawn| drawn.into_iter().take(keep).collect::<Map<String, Value>>())
        })
        .boxed();

    // An object whose members are all `additionalProperties` has no
    // `properties` walk to charge depth against, so the bound is applied here
    // rather than in the caller — without it, `JSONValue` nests without limit.
    let extra_part = if depth >= MAX_DEPTH {
        Just(None).boxed()
    } else {
        unknown_member(map, defs, depth, visiting)
    };

    (required_part, optional_part, extra_part)
        .prop_map(|(mut members, optional, extra)| {
            for (name, value) in optional {
                members.insert(name, value);
            }
            if let Some((name, value)) = extra {
                members.insert(name, value);
            }
            Value::Object(members)
        })
        .boxed()
}

/// Draw one value for every `(name, strategy)` pair, keeping the names.
///
/// The names are lifted out of the strategy and no strategy is built for them:
/// `proptest` has no `Strategy` impl for `String`, so a bare name paired with a
/// value strategy is not a strategy at all. Zipping afterwards avoids that, and
/// the names are fixed anyway.
///
/// A `Vec<Strategy>` is itself a strategy drawing the whole tuple in one go.
/// `collection::vec` around it would draw the entire list `count` times and
/// produce `Vec<Vec<Value>>`, which is not the shape wanted here.
///
/// An empty pair list yields an empty map, so a schema with no properties
/// produces `{}` without a special case.
fn draw_all(members: Vec<(String, BoxedStrategy<Value>)>) -> BoxedStrategy<Map<String, Value>> {
    let names: Vec<String> = members.iter().map(|(name, _)| name.clone()).collect();
    let strategies: Vec<BoxedStrategy<Value>> = members.into_iter().map(|(_, s)| s).collect();
    strategies
        .prop_map(move |drawn| {
            names
                .iter()
                .cloned()
                .zip(drawn)
                .collect::<Map<String, Value>>()
        })
        .boxed()
}

/// A member the schema does not name, so the flattened catch-all is exercised.
///
/// Skipped entirely when `additionalProperties` is `false`: injecting a member
/// there produces a value that does not validate, and the suite would then be
/// reporting its own bug as a generator bug. When it is a schema, the member
/// conforms to it — and the caller's `depth` and `visiting` are carried into
/// it, because a schema-valued `additionalProperties` is routinely a `$ref`
/// back into a definition that reaches this function again. Starting from a
/// fresh `visiting` there recurses until the stack runs out.
fn unknown_member(
    map: &Map<String, Value>,
    defs: &Defs,
    depth: usize,
    visiting: &mut Vec<String>,
) -> BoxedStrategy<Option<(String, Value)>> {
    match map.get("additionalProperties") {
        Some(Value::Bool(false)) => Just(None).boxed(),
        Some(scheme @ Value::Object(_)) => {
            let value = value_at(scheme, defs, depth + 1, visiting);
            prop_oneof![
                3 => Just(None),
                1 => (Just("x-unknown-member".to_owned()), value)
                    .prop_map(|(name, value)| Some((name, value))),
            ]
            .boxed()
        }
        // Absent or `true`: the schema permits extra members, so a free one is
        // generated. The key comes from a fixed pool so it cannot collide with
        // a real member and overwrite one, which would make the round trip pass
        // for the wrong reason.
        _ => prop_oneof![
            3 => Just(None),
            1 => (Just("x-unknown-member".to_owned()), any_value())
                .prop_map(|(name, value)| Some((name, value))),
        ]
        .boxed(),
    }
}

/// An array value honouring `minItems` and `maxItems`.
fn array_value(
    schema: &Value,
    defs: &Defs,
    depth: usize,
    visiting: &mut Vec<String>,
) -> BoxedStrategy<Value> {
    let Some(map) = schema.as_object() else {
        return any_value();
    };
    let min = bound(map.get("minItems"), 4);
    // A cap on the sample, not on the schema: without it proptest explores
    // arrays of unbounded length.
    let max = map
        .get("maxItems")
        .map_or_else(|| min.max(2), |m| bound(Some(m), 4))
        .clamp(min, 4);
    let items = map.get("items").map_or_else(any_value, |member| {
        value_at(member, defs, depth + 1, visiting)
    });
    proptest::collection::vec(items, min..=max)
        .prop_map(Value::Array)
        .boxed()
}

/// Readable strings for a `string` value, honouring `minLength`.
///
/// Drawn from a fixed pool plus a short counter rather than generated
/// characters: these strings appear verbatim in failure output, and a random
/// glyph stream makes it unreadable. `format` is not honoured here — see
/// [`formatted_string`], which the caller prefers when the keyword is present.
fn string_value(min: usize) -> BoxedStrategy<Value> {
    const POOL: &[&str] = &[
        "",
        "a",
        "hello",
        "tool-name",
        "file:///tmp/a.txt",
        "text/plain",
        "2026-10-02T00:00:00Z",
        "client-1",
    ];
    (0..64u32)
        .prop_map(move |counter| {
            // The modulo is of the pool's own length, so the lookup cannot
            // miss; the `None` arm is there because the compiler insists.
            let base = POOL.get((counter as usize) % POOL.len()).map_or("", |b| *b);
            let mut text = format!("{base}{counter}");
            // `minLength` counts code points, and padding with ASCII keeps the
            // code-point count and the byte length in step.
            while text.len() < min {
                text.push('x');
            }
            Value::String(text)
        })
        .boxed()
}

/// Strings that satisfy the `format` keyword.
///
/// `uri` and `uri-template` are asserted by `jsonschema`, so a value that does
/// not parse as one fails validation before the round trip is even reached —
/// which would make the test report a generator fault as if it were an emitter
/// fault. `byte` is in the vendored files but is not a standard format, so the
/// crate ignores it and the base64 pool entry is only there to be realistic.
///
/// Appending a counter to a URI keeps every sample distinct while staying
/// parseable; the template braces are left alone because a `{` inside a path
/// segment is what makes it a template.
fn formatted_string(format: &str, min: usize) -> BoxedStrategy<Value> {
    let base: &'static str = match format {
        "uri" => "https://example.test/resource",
        "uri-template" => "https://example.test/resource/{id}",
        "byte" => "aGVsbG8gd29ybGQ=",
        // An unrecognised format constrains nothing the crate enforces.
        _ => return string_value(min),
    };
    (0..32u32)
        .prop_map(move |counter| {
            let mut text = format!("{base}/{counter}");
            // Padded with a path-safe character so a URI stays parseable.
            while text.len() < min {
                text.push('x');
            }
            Value::String(text)
        })
        .boxed()
}

/// An integer honouring `minimum` and `maximum`.
fn integer_value(map: &Map<String, Value>) -> BoxedStrategy<Value> {
    let min = bound_i64(map.get("minimum"), -1000).min(0);
    let max = bound_i64(map.get("maximum"), 1000).max(0);
    (min..=max.max(min)).prop_map(|n| json!(n)).boxed()
}

/// A numeric schema keyword as an `i64`, falling back to `fallback`.
///
/// Read as an integer on purpose. Every numeric bound in all six vendored
/// revisions is one — `minimum: 0`, `maximum: 1`, `maxItems: 100` — so
/// rejecting a fractional bound costs nothing here, and it is the honest answer
/// anyway: `maximum: 0.5` has no single integer reading, and rounding it up
/// would generate values the schema excludes. A bound written as `0.0` is still
/// read, because JSON has one number type and `Value::as_i64` accepts it.
fn bound_i64(keyword: Option<&Value>, fallback: i64) -> i64 {
    keyword
        .and_then(Value::as_i64)
        .or_else(|| {
            keyword
                .and_then(Value::as_u64)
                .and_then(|v| i64::try_from(v).ok())
        })
        .unwrap_or(fallback)
}

/// A non-negative numeric keyword as a `usize`, capped at `cap`.
///
/// Capping matters more than the conversion: these keywords become loop bounds
/// and vector lengths, and `maxItems: 100` in the 2026 revisions would otherwise
/// draw arrays of a hundred elements on every sample. A value past `cap` is
/// treated as `cap` rather than rejected — the property under test is the round
/// trip, not the schema's plausibility.
fn bound(keyword: Option<&Value>, cap: usize) -> usize {
    usize::try_from(bound_i64(keyword, 0).max(0)).map_or(cap, |value| value.min(cap))
}

/// A number honouring `minimum` and `maximum`.
///
/// Drawn as a bounded integer over 1000 rather than a raw `f64`. That keeps the
/// value exactly representable, so the decimal serde writes parses back to the
/// same `f64` and the round-trip comparison is not testing float formatting.
fn number_value(map: &Map<String, Value>) -> BoxedStrategy<Value> {
    let min = map.get("minimum").and_then(Value::as_f64).unwrap_or(-100.0);
    let max = map.get("maximum").and_then(Value::as_f64).unwrap_or(100.0);
    let (low, high) = (min.min(max), max.max(min));
    (-100_000i32..=100_000)
        .prop_map(move |n| json!((f64::from(n) / 1000.0).clamp(low, high)))
        .boxed()
}
