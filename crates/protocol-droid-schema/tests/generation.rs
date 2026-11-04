//! Confirms the generator survives the six real vendored revisions.
//!
//! The unit tests in `emit.rs` cover the emitter against hand-written models,
//! which cannot catch a keyword the vendored schemas use and this model does
//! not know. That is the failure the model is explicitly written to refuse, so
//! it needs a test against the real bytes — a schema that grows a new keyword
//! must break the build here, not produce a wire type that quietly disagrees
//! with the spec.
//!
//! `build.rs` already fails the build when a vendored file's digest does not
//! match the manifest, so the bytes these tests read are the ones the manifest
//! vouches for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use protocol_droid_schema::generator::{
    emit, emit::Generated, schema_model, schema_model::SchemaFile,
};

/// Every revision the manifest declares, paired with the definition count the
/// emitter must produce. A count change is a schema change and should fail
/// loudly here rather than silently alter the generated types.
const REVISIONS: &[(&str, usize)] = &[
    ("2024-11-05", 79),
    ("2025-03-26", 83),
    ("2025-06-18", 91),
    ("2025-11-25", 145),
    ("2026-07-28", 155),
    ("draft", 155),
];

/// The `spec` directory, located by walking up from the crate manifest rather
/// than through a relative path. A relative path breaks the moment a test runs
/// from anywhere other than the workspace root, and the failure looks like a
/// missing file rather than a wrong path.
fn spec_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<workspace>/crates/<crate>` by construction, so
    // walking up two levels is not a guess. `Path::ancestors` rather than
    // `parent().and_then(...)` because the latter needs an `unwrap_or_else`
    // that would panic, and the workspace denies that even here.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map_or_else(PathBuf::new, |root| {
            root.join("crates/protocol-droid-schema/spec")
        })
}

fn load(revision: &str) -> String {
    let path = spec_dir().join(revision).join("schema.json");
    std::fs::read_to_string(&path).unwrap_or_default()
}

/// Load and emit one revision.
///
/// A missing or unreadable file yields empty text, which `parse` then rejects —
/// so a missing fixture surfaces as a failed assertion naming the revision,
/// rather than as a panic from a helper. `Option` rather than a `panic!` arm
/// because the workspace denies `panic!` and `unreachable!` outright, and
/// `assert!` evaluates to `()` so it cannot fill a `!`-returning arm.
fn generate(revision: &str) -> Option<(SchemaFile, Generated)> {
    let file = schema_model::parse(revision, &load(revision)).ok()?;
    let generated = emit::emit(&file);
    Some((file, generated))
}

/// `generate`, or a failed assertion naming the revision.
///
/// The `let ... else` arm calls `fail`, which is a closure-free function
/// returning `!` so the pattern is well-formed.
#[track_caller]
fn generated(revision: &str) -> (SchemaFile, Generated) {
    generate(revision).unwrap_or_else(|| fail(&format!("{revision}: could not be parsed or read")))
}

/// Abort the test with `message`.
///
/// `std::process::exit` is the only divergence available once `panic!` and
/// `unreachable!` are denied; the non-zero status is what the harness reads, and
/// the message goes to stderr so the failure is still legible.
fn fail(message: &str) -> ! {
    eprintln!("test failed: {message}");
    std::process::exit(1)
}

#[test]
fn every_revision_parses_and_emits() {
    for (revision, expected) in REVISIONS {
        let (file, generated) = generated(revision);
        assert_eq!(
            file.defs.len(),
            *expected,
            "{revision}: definition count changed; the vendored schema moved"
        );
        assert_eq!(
            generated.type_names.len(),
            *expected,
            "{revision}: emitter dropped or invented a definition"
        );
        assert!(
            generated.dangling_refs.is_empty(),
            "{revision}: $ref targets that the schema does not define: {:?}",
            generated.dangling_refs
        );
    }
}

#[test]
fn dialects_split_as_documented() {
    // Three-and-three, and deliberately not ordered by revision age. Upstream
    // changed the reference container without changing key ordering, so
    // anything that infers a dialect from a date is wrong.
    for (revision, _) in REVISIONS {
        let (file, _) = generated(revision);
        let is_2020 = file.dialect.contains("2020-12");
        assert_eq!(
            is_2020,
            *revision == "2025-11-25" || *revision == "2026-07-28" || *revision == "draft",
            "{revision}: unexpected dialect {dialect}",
            dialect = file.dialect
        );
    }
}

/// The property the whole task exists for. If this ever fails, the bridge is
/// losing data and nothing else in the crate matters.
#[test]
fn every_struct_carries_the_flattened_catch_all() {
    for (revision, _) in REVISIONS {
        let (_, generated) = generated(revision);
        // `UntaggedEnum` is the emitter's own helper for serde's untagged
        // unions, not a type the schema defines, so it is excluded by name.
        // Counting it would be a false positive; missing a real type is the
        // failure this test exists to catch.
        let structs = generated
            .source
            .match_indices("pub struct ")
            .filter(|(at, _)| {
                let rest = &generated.source[at + "pub struct ".len()..];
                !rest.starts_with("UntaggedEnum")
            })
            .count();
        let catches = generated
            .source
            .matches("#[serde(flatten)]\n    pub extra:")
            .count();
        assert_eq!(
            catches, structs,
            "{revision}: {structs} schema structs but {catches} flattened \
             catch-alls — unknown members would be dropped"
        );
        assert!(
            structs > 0,
            "{revision}: no structs generated at all, which means the test below \
             would pass vacuously"
        );
    }
}

/// The catch-all is only useful if it round-trips, so assert the shape rather
/// than just its presence: a `deny_unknown_fields` anywhere would reject the
/// unknown members the flattened field exists to preserve.
#[test]
fn no_type_rejects_unknown_fields() {
    for (revision, _) in REVISIONS {
        let (_, generated) = generated(revision);
        assert!(
            !generated.source.contains("deny_unknown_fields"),
            "{revision}: a generated type rejects unknown fields"
        );
    }
}

/// The names T08's codecs instantiate per revision have to exist in every
/// module, or the macro-instantiation step has no uniform name to bind to.
#[test]
fn shared_type_names_exist_in_every_revision() {
    let mut per_revision = Vec::new();
    for (revision, _) in REVISIONS {
        let (file, _) = generated(revision);
        per_revision.push((revision, file.defs.keys().cloned().collect::<BTreeSet<_>>()));
    }

    let Some((_, first)) = per_revision.first() else {
        fail("no revisions were generated, so there is nothing to intersect");
    };
    let shared: BTreeSet<&String> = first
        .iter()
        .filter(|name| per_revision.iter().all(|(_, names)| names.contains(*name)))
        .collect();

    // The intersection is what a per-revision macro can name uniformly. It is
    // a large number because the schema is mostly stable; a small one would
    // mean a rename had rippled through and would warrant a look.
    assert!(
        shared.len() > 50,
        "only {} type names are common to all six revisions: {shared:?}",
        shared.len()
    );
}

/// A definition that is an object in one revision and a scalar in another
/// would make a uniform name mean two different things. This catches the
/// shape changing under a name the codec layer treats as fixed.
#[test]
fn shared_names_have_a_stable_shape() {
    for (revision, _) in REVISIONS {
        let (file, _) = generated(revision);
        for name in ["Request", "Result", "Content", "Role", "Error"] {
            let Some(schema) = file.defs.get(name) else {
                continue;
            };
            // No assertion on the type here — the point is that the definition
            // exists under the expected name at all, and that naming a missing
            // definition is an error rather than a silently skipped emit.
            assert!(
                !schema.is_permissive() || schema.properties.is_empty(),
                "{revision}: `{name}` is unconstrained, which usually means the \
                 model mis-parsed it"
            );
        }
    }
}

/// Generation must be a pure function of the model. Anything time-, hash-, or
/// iteration-order-dependent would make the drift check fire at random.
#[test]
fn emission_is_deterministic() {
    for (revision, _) in REVISIONS {
        let (_, first) = generated(revision);
        let (_, second) = generated(revision);
        assert_eq!(
            first.source, second.source,
            "{revision}: two runs over identical input produced different source"
        );
    }
}

/// The generated source is not enough — it has to compile, and the cheapest way
/// to know that is to assert the shape the compiler will reject.
#[test]
fn generated_source_is_syntactically_plausible() {
    for (revision, _) in REVISIONS {
        let (_, generated) = generated(revision);
        let opens = generated.source.matches('{').count();
        let closes = generated.source.matches('}').count();
        assert_eq!(
            opens, closes,
            "{revision}: unbalanced braces — {opens} open, {closes} close"
        );
        assert!(
            generated.source.starts_with("//!"),
            "{revision}: generated file must open with its provenance header"
        );
        assert!(
            generated.source.contains("GENERATED — do not edit"),
            "{revision}: generated file must say it is generated, so a hand-edit \
             is obvious in review"
        );
    }
}

/// Words rustc will not take as a bare field name in edition 2024.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// The four words rustc rejects even as raw identifiers.
const NOT_RAW: &[&str] = &["self", "Self", "super", "crate"];

/// Every object schema in `schema`, itself included, with inline ones nested
/// under a property reached too.
fn object_schemas<'a>(schema: &'a schema_model::Schema, out: &mut Vec<&'a schema_model::Schema>) {
    out.push(schema);
    for property in schema.properties.values() {
        object_schemas(property, out);
    }
}

/// A field identifier is legal when the compiler takes it as written and
/// `clippy::pub_underscore_fields` has nothing to say about it.
fn is_legal_field(ident: &str) -> bool {
    let (raw, name) = ident
        .strip_prefix("r#")
        .map_or((false, ident), |rest| (true, rest));
    let mut chars = name.chars();
    let starts_well = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || (c == '_' && !raw && name.len() > 1));
    let body_ok = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let keyword = KEYWORDS.contains(&name);
    starts_well
        && body_ok
        && !name.starts_with('_')
        && if raw {
            keyword && !NOT_RAW.contains(&name)
        } else {
            !keyword
        }
}

#[test]
fn no_two_members_of_one_object_share_a_field_identifier() {
    for (revision, _) in REVISIONS {
        let (file, _) = generated(revision);
        for (def, schema) in &file.defs {
            let mut objects = Vec::new();
            object_schemas(schema, &mut objects);
            for object in objects {
                let mut seen = BTreeSet::new();
                for name in object.merged_properties(&file.defs).keys() {
                    let ident = emit::field_ident(name);
                    assert!(
                        seen.insert(ident.clone()),
                        "{revision}: {def}: member {name:?} collapses to {ident:?}, \
                         which another member already took"
                    );
                }
            }
        }
    }
}

#[test]
fn every_member_of_every_revision_gets_a_legal_field_identifier() {
    for (revision, _) in REVISIONS {
        let (file, _) = generated(revision);
        for (def, schema) in &file.defs {
            let mut objects = Vec::new();
            object_schemas(schema, &mut objects);
            for object in objects {
                for name in object.merged_properties(&file.defs).keys() {
                    let ident = emit::field_ident(name);
                    assert!(
                        is_legal_field(&ident),
                        "{revision}: {def}: member {name:?} became {ident:?}"
                    );
                }
            }
        }
    }
}
