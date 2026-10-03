//! Comment-preserving edits to `spec/MANIFEST.toml`.
//!
//! The manifest is heavily commented, and those comments are load-bearing: they
//! record why 2024-11-05 has no upstream changelog, which tags were excluded
//! and why, and that draft's changelog is a placeholder rather than a defect.
//!
//! Round-tripping the file through `toml::to_string` would silently destroy
//! every one of them. So digests are rewritten by walking the original text and
//! replacing only the hex value on the matching line. Anything that is not a
//! digest line passes through byte-for-byte.
//!
//! The alternative — regenerating the manifest from the parsed model — was
//! rejected for the same reason. This file has to be treated as source, with
//! comments as part of it, not as a serialisation target.

use std::collections::BTreeMap;

/// A digest line: `"filename" = "<64 hex>"`, possibly with a trailing comment.
///
/// Slices are stored *without* their surrounding quotes, borrowed from the
/// original line so a rewrite never has to re-parse what it already split.
#[derive(Debug, PartialEq, Eq)]
struct DigestLine<'a> {
    /// Leading whitespace, preserved verbatim.
    pub indent: &'a str,
    /// File name, unquoted.
    pub key: &'a str,
    /// Whitespace between the key's closing quote and the `=`, preserved verbatim.
    pub gap: &'a str,
    /// Whitespace between the `=` and the opening quote of the value, preserved
    /// verbatim.
    pub post_gap: &'a str,
    /// The 64-character lowercase hex digest, unquoted.
    pub digest: &'a str,
    /// Anything after the closing quote, including a trailing `#` comment.
    pub trailing: &'a str,
}

impl DigestLine<'_> {
    /// File name.
    #[must_use]
    const fn file_name(&self) -> &str {
        self.key
    }

    /// Rebuild the line with a new digest, preserving every other byte.
    #[must_use]
    pub(super) fn with_digest(&self, digest: &str) -> String {
        format!(
            "{indent}\"{key}\"{gap}={post_gap}\"{digest}\"{trailing}",
            indent = self.indent,
            key = self.key,
            gap = self.gap,
            post_gap = self.post_gap,
            digest = digest,
            trailing = self.trailing,
        )
    }
}

/// Parse a line as a digest entry, or `None` if it is not one.
///
/// Deliberately hand-rolled rather than a regex: this runs over a file a human
/// edits by hand, so it must reject anything it does not positively recognise
/// as `"name" = "<64 hex>"` rather than guessing.
#[must_use]
fn parse_digest_line(line: &str) -> Option<DigestLine<'_>> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let rest = rest.trim_start();

    // Key: a quoted string, which must not contain an escaped quote.
    let rest = rest.strip_prefix('"')?;
    let key_len = rest.find('"')?;
    let (key, rest) = rest.split_at(key_len);
    // Step over the key's closing quote, but do NOT trim yet: the whitespace
    // after it is the alignment this file's tables are built on, and dropping it
    // would reflow every line on the first rewrite.
    let rest = &rest[1..];

    // Separator. The gap is measured BEFORE trimming, otherwise the alignment
    // that gives the manifest its shape is silently dropped on rewrite.
    let gap_len = rest.len() - rest.trim_start().len();
    let (gap, rest) = rest.split_at(gap_len);
    if !rest.starts_with('=') {
        return None;
    }
    let rest = &rest[1..];

    // Value: exactly 64 lowercase hex inside quotes. The whitespace after the
    // `=` is kept so a hand-aligned table stays aligned, which means measuring
    // it before trimming, for the same reason as `gap` above.
    let post_len = rest.len() - rest.trim_start().len();
    let (post_gap, rest) = rest.split_at(post_len);
    let rest = rest.strip_prefix('"')?;
    if rest.len() < 65 {
        return None;
    }
    let (digest, rest) = rest.split_at(64);
    if !digest
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    if !rest.starts_with('"') {
        return None;
    }
    let trailing = &rest[1..];

    Some(DigestLine {
        indent,
        key,
        gap,
        post_gap,
        digest,
        trailing,
    })
}

/// Apply new digests to a manifest's text, preserving comments and layout.
///
/// `updates` maps a revision name to the new digest of each of its files. Only
/// those `(revision, file)` pairs are touched; every other line is returned
/// unchanged.
///
/// # Errors
///
/// Returns a human-readable error if a listed file has no corresponding digest
/// line in the manifest, which would mean the file is recorded somewhere other
/// than the `[revisions.<name>.files]` table and a blind rewrite would silently
/// do nothing.
pub fn apply_digest_updates(
    text: &str,
    updates: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<String, String> {
    // Flattened to (revision, file) -> new digest, for O(1) lookup per line.
    // Keys are owned so nothing borrows from `revision` while it is reassigned.
    let wanted: BTreeMap<(String, String), String> = updates
        .iter()
        .flat_map(|(rev, files)| {
            files
                .iter()
                .map(move |(file, digest)| ((rev.clone(), file.clone()), digest.clone()))
        })
        .collect();
    let mut applied: BTreeMap<(String, String), String> = BTreeMap::new();

    let mut out = String::with_capacity(text.len());
    let mut revision: Option<String> = None;
    let mut in_files_table = false;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[') {
            // A new table header ends whichever table we were in.
            in_files_table = false;
            if let Some(name) = rest.strip_suffix(']')
                && let Some(rev) = name.strip_prefix("revisions.")
            {
                match rev.strip_suffix(".files") {
                    Some(rev) => {
                        in_files_table = true;
                        revision = Some(rev.to_owned());
                    }
                    None => revision = Some(rev.to_owned()),
                }
            }
        }

        // `current` is an owned copy, so `revision` stays free to be reassigned
        // on the next iteration while this one is being processed.
        let current = if in_files_table {
            revision.clone()
        } else {
            None
        };
        let parsed = parse_digest_line(line);

        let replacement = match (&current, &parsed) {
            (Some(rev), Some(parsed)) => {
                let key = (rev.clone(), parsed.file_name().to_owned());
                wanted.get(&key).map(|digest| {
                    applied.insert(key, digest.clone());
                    parsed.with_digest(digest)
                })
            }
            _ => None,
        };

        match replacement {
            Some(new_line) => {
                out.push_str(&new_line);
                if !new_line.ends_with('\n') && line.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(line),
        }
    }

    let missing: Vec<String> = wanted
        .keys()
        .filter(|key| !applied.contains_key(*key))
        .map(|(rev, file)| format!("{rev}/{file}"))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "no digest line found in MANIFEST.toml for: {}",
            missing.join(", ")
        ));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls; propagating errors would replace a clear failure message with noise"
    )]

    use std::collections::BTreeMap;

    use super::{apply_digest_updates, parse_digest_line};

    const NEW: &str = "aaaa";

    fn one() -> BTreeMap<String, BTreeMap<String, String>> {
        let mut files = BTreeMap::new();
        files.insert("schema.json".to_owned(), NEW.to_owned());
        BTreeMap::from([("2025-06-18".to_owned(), files)])
    }

    #[test]
    fn a_simple_digest_line_parses() {
        let line = "\"schema.json\" = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"";
        let parsed = parse_digest_line(line).expect("must parse");
        assert_eq!(parsed.file_name(), "schema.json");
        assert_eq!(parsed.digest.len(), 64);
        assert_eq!(parsed.trailing, "");
    }

    #[test]
    fn alignment_and_trailing_comments_are_preserved() {
        let original = "\"schema.ts\"   = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"  # upstream";
        let parsed = parse_digest_line(original).expect("must parse");
        let rewritten = parsed.with_digest(NEW);
        assert_eq!(
            rewritten, "\"schema.ts\"   = \"aaaa\"  # upstream",
            "alignment and the trailing comment must survive"
        );
    }

    #[test]
    fn indented_lines_keep_their_indentation() {
        let parsed =
            parse_digest_line("  \"schema.json\" = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"")
                .expect("must parse");
        assert_eq!(parsed.indent, "  ");
        assert_eq!(parsed.with_digest(NEW), "  \"schema.json\" = \"aaaa\"");
    }

    #[test]
    fn non_digest_lines_are_not_parsed() {
        for line in [
            "schema_prefix = \"schema/2025-06-18\"",
            "tag = \"2025-06-18\"",
            "[revisions.2025-06-18.files]",
            "# \"schema.json\" = \"short\"",
            "floating = true",
            "",
        ] {
            assert!(
                parse_digest_line(line).is_none(),
                "should not have parsed: {line:?}"
            );
        }
    }

    #[test]
    fn an_uppercase_digest_is_not_recognised() {
        // The manifest requires lowercase hex; a line that is not one is left
        // alone rather than half-rewritten.
        let line = "\"schema.json\" = \"0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef\"";
        assert!(parse_digest_line(line).is_none());
    }

    #[test]
    fn comments_and_layout_survive_a_rewrite() {
        let text = "\
# leading comment
[revisions.2025-06-18]
tag = \"2025-06-18\"

# why this revision is vendored
[revisions.2025-06-18.files]
\"schema.json\" = \"1111111111111111111111111111111111111111111111111111111111111111\"
\"schema.ts\"   = \"2222222222222222222222222222222222222222222222222222222222222222\"  # kept
";
        let out = apply_digest_updates(text, &one()).expect("rewrite must succeed");
        assert!(out.contains("# leading comment"), "comment lost");
        assert!(
            out.contains("# why this revision is vendored"),
            "comment lost"
        );
        assert!(
            out.contains("\"schema.json\" = \"aaaa\""),
            "digest not rewritten"
        );
        assert!(
            out.contains("\"schema.ts\"   = \"2222222222222222222222222222222222222222222222222222222222222222\"  # kept"),
            "untouched file must be byte-identical, got: {out}"
        );
    }

    #[test]
    fn a_digest_in_another_revision_is_not_touched() {
        let text = "\
[revisions.2024-11-05.files]
\"schema.json\" = \"1111111111111111111111111111111111111111111111111111111111111111\"

[revisions.2025-06-18.files]
\"schema.json\" = \"1111111111111111111111111111111111111111111111111111111111111111\"
";
        let out = apply_digest_updates(text, &one()).expect("rewrite must succeed");
        let before = text.split("\n\n").next().expect("first revision");
        assert!(
            out.contains(before),
            "the other revision's digest was modified:\n{out}"
        );
    }

    #[test]
    fn a_file_with_no_digest_line_is_an_error() {
        let text = "[revisions.2025-06-18]\ntag = \"2025-06-18\"\n";
        let mut updates = one();
        updates
            .get_mut("2025-06-18")
            .expect("entry exists")
            .insert("schema-extra.json".to_owned(), NEW.to_owned());
        let error = apply_digest_updates(text, &updates).expect_err("must fail");
        assert!(error.contains("schema-extra.json"), "error: {error}");
    }
}
