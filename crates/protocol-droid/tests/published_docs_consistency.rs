//! The documents a reader actually receives, checked against the code.
//!
//! Two documents reach people: the root `README.md`, which is what someone
//! reads before deciding whether to depend on this crate at all, and the
//! Starlight site under `site/`, which is the only documentation tree the
//! deploy script publishes. Both were wrong in ways nothing caught.
//!
//! The README's install snippets named `0.2` through two releases, because the
//! consistency test that exists (`protocol-droid-schema/tests/readme.rs`) covers
//! the generated-wire-types example and never looked at a version string. A
//! reader who followed it installed a crate without the RFC 9728 document.
//!
//! The site's security guide carried a hand-written resource-server snippet
//! with four import paths that do not exist, because nothing ever compiled it.
//! It is now spliced from `examples/oauth_resource_server.rs`, which CI does
//! build — so the remaining risk is the splice going stale, which is what these
//! tests are for.
//!
//! Note the absence of `docs/guide/`. It was an mdBook tree that no workflow
//! built, no deploy script published, and nothing linked to, and it had already
//! drifted from the site by dozens of lines. It is deleted, and
//! `site_is_the_only_document_tree` below fails if it returns — a second tree
//! is not a redundancy, it is a second thing to forget to update.

use std::path::{Path, PathBuf};

/// The repository root, resolved from this crate's manifest directory so the
/// test works whatever the working directory is.
fn repo_root() -> Result<PathBuf, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "could not resolve the repo root from {}",
                manifest.display()
            )
        })
}

fn read(root: &Path, relative: &str) -> Result<String, String> {
    let path = root.join(relative);
    std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

/// The workspace version, read from the manifest rather than from
/// `env!("CARGO_PKG_VERSION")`, which is the `protocol-droid` crate's version
/// and would agree with the manifest only by coincidence.
fn workspace_version(root: &Path) -> Result<String, String> {
    let manifest = read(root, "Cargo.toml")?;
    let version = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .ok_or_else(|| "no top-level `version =` in the workspace manifest".to_owned())?;
    Ok(version.to_owned())
}

/// The major.minor series, which is what a caret requirement names.
///
/// The README is meant to say `0.4` while the workspace is `0.4.1`: under
/// Cargo's default caret semantics `"0.4"` already resolves to every `0.4.x`,
/// and pinning the patch would make every release a README edit. Comparing the
/// series rather than the full version encodes that, so the test fails when a
/// reader would be sent to the *wrong minor* — the 0.2 case — and stays quiet
/// for a routine patch.
fn series(version: &str) -> &str {
    let Some((major, rest)) = version.split_once('.') else {
        return version;
    };
    // The second dot's index in the whole string: past the major and its dot,
    // then however far into `rest` the dot sits. A version with no patch
    // component is already the series.
    rest.find('.').map_or(version, |offset| {
        let second_dot = major.len().saturating_add(1).saturating_add(offset);
        version.get(..second_dot).unwrap_or(version)
    })
}

/// The slice arithmetic above is easy to get off by the dot, which would
/// compare `"0."` against `"0.4."` and fail on a correct README. Pin the cases
/// rather than trusting one green run to have exercised them.
#[test]
fn a_series_is_the_major_and_minor() {
    assert_eq!(series("0.4"), "0.4");
    assert_eq!(series("0.4.1"), "0.4");
    assert_eq!(series("0.4.13"), "0.4");
    assert_eq!(series("1.0.0"), "1.0");
    assert_eq!(series("10.20.3"), "10.20");
    // A bare major has no series to truncate, and is returned whole rather
    // than panicking on a missing component.
    assert_eq!(series("4"), "4");
}

/// Every install snippet in the README names the published series.
///
/// Covers both shapes the README uses: a bare dependency, and one carrying
/// `default-features`. A snippet that hardcodes a stale minor sends the reader
/// to a crate missing features the README's own prose describes.
#[test]
fn the_readme_install_snippets_name_the_published_version() -> Result<(), String> {
    let root = repo_root()?;
    let version = workspace_version(&root)?;
    let readme = read(&root, "README.md")?;

    let mut checked = 0_usize;
    for (number, line) in readme.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("protocol-droid = ") {
            continue;
        }
        checked += 1;
        // Strip the key, the optional brace, and the `version =` that appears
        // inside a table form, then take the text between the first quotes.
        let after_key = trimmed
            .trim_start_matches("protocol-droid = ")
            .trim_start_matches('{')
            .trim_start();
        let after_key = after_key
            .strip_prefix("version = ")
            .unwrap_or(after_key)
            .trim_start();
        let declared = after_key
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"'))
            .map(|(version, _)| version)
            .ok_or_else(|| {
                format!(
                    "README.md:{}: could not read a version out of `{trimmed}`",
                    number + 1
                )
            })?;
        assert_eq!(
            series(declared),
            series(&version),
            "README.md:{} says `protocol-droid = \"{declared}\"` but the workspace is {version}. \
             A reader who copies this installs a crate that predates the documented feature set.",
            number + 1
        );
    }

    assert!(
        checked >= 2,
        "expected the README to carry at least two install snippets, found {checked}. \
         Either they were removed or the scan above no longer recognises their shape."
    );
    Ok(())
}

/// The feature table in the README names every feature the manifest declares.
///
/// A feature that exists but is undocumented cannot be found, and one that is
/// documented but renamed is worse: the reader writes it and gets a build
/// error at the same place the `http` note exists to preempt.
#[test]
fn the_readme_feature_table_matches_the_manifest() -> Result<(), String> {
    let root = repo_root()?;
    let manifest = read(&root, "crates/protocol-droid/Cargo.toml")?;
    let readme = read(&root, "README.md")?;

    let mut in_features = false;
    for line in manifest.lines() {
        if line.starts_with('[') {
            in_features = line.trim() == "[features]";
            continue;
        }
        if !in_features {
            continue;
        }
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || name == "default"
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            continue;
        }
        assert!(
            readme.contains(&format!("| `{name}`")),
            "feature `{name}` is declared in the manifest but has no row in the README's feature \
             table, so nobody can discover it"
        );
    }
    Ok(())
}

/// The site's security snippet is the compiled example, not a copy of it.
///
/// The example is what CI compiles. This asserts the markdown carries the same
/// lines, so the published document cannot drift into describing an API that no
/// longer exists — the failure mode that let four bad import paths ship.
#[test]
fn the_site_security_snippet_matches_the_compiled_example() -> Result<(), String> {
    let root = repo_root()?;
    let example = read(
        &root,
        "crates/protocol-droid/examples/oauth_resource_server.rs",
    )?;
    let site = read(&root, "site/src/content/docs/guides/security.md")?;

    // The anchored region is what the guide shows, between the two marker
    // comments, exclusive.
    let mut region: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in example.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("// ANCHOR: assembly") {
            inside = true;
            continue;
        }
        if trimmed.starts_with("// ANCHOR_END: assembly") {
            inside = false;
            continue;
        }
        if inside {
            region.push(line);
        }
    }
    while region.first().is_some_and(|line| line.trim().is_empty()) {
        region.remove(0);
    }
    while region.last().is_some_and(|line| line.trim().is_empty()) {
        region.pop();
    }
    assert!(
        region.len() > 10,
        "the example's `assembly` anchor captured only {} lines; the markers probably moved",
        region.len()
    );

    // Every non-blank line of the region must appear in the published fence, in
    // order. Comparing presence rather than an exact block lets the surrounding
    // prose change freely while still failing the moment the code diverges.
    let mut after_fence = site.lines().skip_while(|line| line.trim() != "```rust");
    after_fence.next();
    let published: Vec<&str> = after_fence
        .take_while(|line| line.trim() != "```")
        .collect();

    let mut cursor = 0_usize;
    for line in &region {
        if line.trim().is_empty() {
            continue;
        }
        let remaining = published.get(cursor..).ok_or_else(|| {
            format!("the search cursor ran {cursor} lines past the end of the published fence")
        })?;
        match remaining
            .iter()
            .position(|candidate| candidate.trim() == line.trim())
        {
            Some(offset) => cursor += offset + 1,
            None => {
                return Err(format!(
                    "site/src/content/docs/guides/security.md no longer carries this line from the \
                     example, in order: `{line}`.\nRe-splice the example's `assembly` region into \
                     the guide."
                ));
            }
        }
    }
    Ok(())
}

/// The site is the only maintained document tree.
///
/// `docs/guide/` was an mdBook tree beside the site: not built by any workflow,
/// not published by the deploy script, linked from nothing, and already
/// carrying prose that disagreed with the site. It was deleted rather than
/// reconciled, because reconciling it would have meant maintaining both.
///
/// This asserts the deletion holds. A second tree is worse than a missing one:
/// it looks maintained, so nobody re-checks whether it is, and it drifts while
/// appearing authoritative. The root `book.toml` goes with it — it existed only
/// to describe that build.
#[test]
fn site_is_the_only_document_tree() -> Result<(), String> {
    let root = repo_root()?;

    for relative in ["docs/guide", "book.toml"] {
        let path = root.join(relative);
        if path.exists() {
            return Err(format!(
                "{} exists again. The site under `site/` is the only maintained document tree: \
                 no workflow builds this, the deploy script does not publish it, and it drifts \
                 from the site without failing anything. Either delete it or move its content \
                 into the site — do not leave two.",
                path.display()
            ));
        }
    }
    Ok(())
}
