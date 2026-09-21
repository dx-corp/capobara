//! Inbound vendoring: the direction Capobara did not have.
//!
//! Capobara publishes Mono subtrees *outward* — Mono is the source of truth and
//! a projection lands in a standalone repository. A vendored third-party tree
//! runs the other way, and nothing verified it, so a vendored directory was an
//! unfalsifiable snapshot: no way to answer "is this still what upstream said,
//! and which files have we changed?" without doing the import again by hand.
//!
//! This module answers exactly that question and nothing more. It **never
//! writes**. Refreshing a vendored tree stays a reviewed human operation;
//! what belongs in a tool is the check that tells you whether the tree still
//! matches its pin, and which divergences were declared.
//!
//! Comparison is on git blob object ids, not file bytes: two blobs are equal
//! exactly when their content is equal, `git ls-tree` gives them for free on
//! both sides, and nothing has to be read into memory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use crate::git::git_bytes;
use crate::tree::path::Matcher;
use crate::{Error, Result, contract, invalid};

/// A vendored upstream tree inside Mono, pinned to one upstream commit.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VendorDefinition {
    pub schema_version: u32,
    pub name: String,
    pub class: String,
    pub upstream: Upstream,
    pub destination: Destination,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Upstream {
    /// Clone URL, recorded so a reader knows what the pin refers to.
    pub repository: String,
    /// Full 40-hex commit. A branch or tag would make the check non-reproducible.
    pub commit: String,
    /// Upstream paths that were vendored. Globs: `*` within a segment, `**` across.
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Destination {
    /// Repository-relative directory the upstream subset lives in.
    pub path: String,
    /// Paths under `path`, relative to it, that Mono deliberately owns.
    /// Divergence here is reported as declared rather than as drift.
    #[serde(default)]
    pub local_paths: Vec<String>,
}

impl VendorDefinition {
    pub fn validate(&self) -> Result<()> {
        invalid(
            self.schema_version == 1,
            format!(
                "{}: unsupported schemaVersion {}",
                self.name, self.schema_version
            ),
        )?;
        invalid(
            self.class == "vendor-import",
            format!(
                "{}: class must be vendor-import, got {}",
                self.name, self.class
            ),
        )?;
        invalid(
            self.upstream.commit.len() == 40
                && self
                    .upstream
                    .commit
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            format!(
                "{}: upstream.commit must be a full lowercase 40-hex commit, got {:?}",
                self.name, self.upstream.commit
            ),
        )?;
        invalid(
            !self.upstream.include.is_empty(),
            format!("{}: upstream.include must not be empty", self.name),
        )?;
        let dest = &self.destination.path;
        invalid(
            !dest.is_empty()
                && !dest.starts_with('/')
                && !dest.split('/').any(|seg| seg == ".." || seg.is_empty()),
            format!(
                "{}: destination.path must be a clean relative path, got {dest:?}",
                self.name
            ),
        )?;
        Ok(())
    }
}

pub fn load(path: &Path) -> Result<VendorDefinition> {
    let text = std::fs::read_to_string(path).map_err(Error::Io)?;
    let definition: VendorDefinition = serde_json::from_str(&text)
        .map_err(|error| Error::Invalid(format!("{}: {error}", path.display())))?;
    definition.validate()?;
    Ok(definition)
}

/// One divergence between the vendored tree and its pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    /// Upstream has the file at this pin; the vendored tree does not.
    Missing(String),
    /// The vendored tree has the file; upstream at this pin does not.
    Extra(String),
    /// Both have it and the blobs differ.
    Modified(String),
}

impl Divergence {
    pub fn path(&self) -> &str {
        match self {
            Divergence::Missing(p) | Divergence::Extra(p) | Divergence::Modified(p) => p,
        }
    }
}

#[derive(Debug)]
pub struct VendorReport {
    pub name: String,
    pub upstream_commit: String,
    pub destination: String,
    /// Upstream paths selected by include/exclude at this pin.
    pub selected: usize,
    /// Divergences not covered by `destination.localPaths`.
    pub drift: Vec<Divergence>,
    /// Divergences that `destination.localPaths` accounts for.
    pub declared: Vec<Divergence>,
}

impl VendorReport {
    pub fn clean(&self) -> bool {
        self.drift.is_empty()
    }

    pub fn summary(&self) -> String {
        format!(
            "{}: pin {} -> {} | {} upstream paths, {} drift, {} declared",
            self.name,
            &self.upstream_commit[..12],
            self.destination,
            self.selected,
            self.drift.len(),
            self.declared.len()
        )
    }
}

/// `git ls-tree -r -z <rev> [-- <path>]` as path -> blob id.
///
/// `-z` because a vendored upstream tree is not ours and may carry paths that
/// git would otherwise quote. Non-blob entries (submodule gitlinks) are
/// rejected rather than silently skipped: a gitlink inside a vendored subset
/// means the vendor is incomplete, which is exactly the thing worth catching.
fn ls_tree(root: &Path, rev: &str, subpath: Option<&str>) -> Result<BTreeMap<String, String>> {
    let mut args: Vec<&str> = vec!["ls-tree", "-r", "-z", rev];
    if let Some(subpath) = subpath {
        args.push("--");
        args.push(subpath);
    }
    let out = git_bytes(root, &args)?;
    let text = String::from_utf8(out)
        .map_err(|_| Error::Invalid(format!("git ls-tree {rev} returned non-UTF-8 paths")))?;
    let mut entries = BTreeMap::new();
    for record in text.split('\0').filter(|r| !r.is_empty()) {
        let (meta, path) = record
            .split_once('\t')
            .ok_or_else(|| Error::Invalid(format!("unparseable ls-tree record: {record:?}")))?;
        let mut parts = meta.split_whitespace();
        let _mode = parts.next();
        let kind = parts
            .next()
            .ok_or_else(|| Error::Invalid(format!("unparseable ls-tree record: {record:?}")))?;
        let oid = parts
            .next()
            .ok_or_else(|| Error::Invalid(format!("unparseable ls-tree record: {record:?}")))?;
        contract(
            kind == "blob",
            format!("{path}: vendored trees must contain only blobs, found {kind}"),
        )?;
        entries.insert(path.to_owned(), oid.to_owned());
    }
    Ok(entries)
}

/// Compare a vendored subtree in `mono_root` against `upstream_checkout` at the
/// pinned commit. `mono_rev` is the revision of Mono to read (`HEAD` normally).
pub fn check(
    mono_root: &Path,
    mono_rev: &str,
    upstream_checkout: &Path,
    definition: &VendorDefinition,
) -> Result<VendorReport> {
    definition.validate()?;

    let include = Matcher::new(&definition.upstream.include)?;
    let exclude = Matcher::new(&definition.upstream.exclude)?;
    let local = Matcher::new(&definition.destination.local_paths)?;

    let upstream_all = ls_tree(upstream_checkout, &definition.upstream.commit, None)?;
    let upstream: BTreeMap<&str, &str> = upstream_all
        .iter()
        .filter(|(path, _)| include.matches(path) && !exclude.matches(path))
        .map(|(path, oid)| (path.as_str(), oid.as_str()))
        .collect();

    let dest_prefix = format!("{}/", definition.destination.path.trim_end_matches('/'));
    let vendored_raw = ls_tree(mono_root, mono_rev, Some(&definition.destination.path))?;
    let vendored: BTreeMap<&str, &str> = vendored_raw
        .iter()
        .filter_map(|(path, oid)| {
            path.strip_prefix(&dest_prefix)
                .map(|rel| (rel, oid.as_str()))
        })
        .collect();

    let mut drift = Vec::new();
    let mut declared = Vec::new();
    let paths: BTreeSet<&str> = upstream
        .keys()
        .copied()
        .chain(vendored.keys().copied())
        .collect();
    for path in paths {
        let divergence = match (upstream.get(path), vendored.get(path)) {
            (Some(_), None) => Divergence::Missing(path.to_owned()),
            (None, Some(_)) => Divergence::Extra(path.to_owned()),
            (Some(up), Some(here)) if up != here => Divergence::Modified(path.to_owned()),
            _ => continue,
        };
        if local.matches(divergence.path()) {
            declared.push(divergence);
        } else {
            drift.push(divergence);
        }
    }

    Ok(VendorReport {
        name: definition.name.clone(),
        upstream_commit: definition.upstream.commit.clone(),
        destination: definition.destination.path.clone(),
        selected: upstream.len(),
        drift,
        declared,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn commit_all(dir: &Path, message: &str) -> String {
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "-q", "-m", message]);
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    struct Fixture {
        upstream: TempDir,
        mono: TempDir,
        commit: String,
    }

    /// Upstream has src/a.ts, src/b.ts and a docs/ tree; Mono vendors only src/.
    fn fixture() -> Fixture {
        let upstream = TempDir::new().unwrap();
        run(upstream.path(), &["init", "-q", "-b", "main"]);
        write(upstream.path(), "src/a.ts", "export const a = 1;\n");
        write(upstream.path(), "src/b.ts", "export const b = 2;\n");
        write(upstream.path(), "docs/readme.md", "not vendored\n");
        let commit = commit_all(upstream.path(), "upstream");

        let mono = TempDir::new().unwrap();
        run(mono.path(), &["init", "-q", "-b", "main"]);
        write(
            mono.path(),
            "vendor/thing/src/a.ts",
            "export const a = 1;\n",
        );
        write(
            mono.path(),
            "vendor/thing/src/b.ts",
            "export const b = 2;\n",
        );
        commit_all(mono.path(), "vendored");

        Fixture {
            upstream,
            mono,
            commit,
        }
    }

    fn definition(commit: &str, local: &[&str]) -> VendorDefinition {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "name": "thing",
            "class": "vendor-import",
            "upstream": {
                "repository": "https://example.invalid/thing",
                "commit": commit,
                "include": ["src/**"],
                "exclude": []
            },
            "destination": { "path": "vendor/thing", "localPaths": local }
        }))
        .unwrap()
    }

    #[test]
    fn faithful_vendor_is_clean() {
        let f = fixture();
        let def = definition(&f.commit, &[]);
        let report = check(f.mono.path(), "HEAD", f.upstream.path(), &def).unwrap();
        assert!(report.clean(), "{:?}", report.drift);
        assert_eq!(report.selected, 2, "docs/ must not be selected by src/**");
    }

    #[test]
    fn an_edited_vendored_file_is_drift() {
        let f = fixture();
        write(
            f.mono.path(),
            "vendor/thing/src/a.ts",
            "export const a = 99;\n",
        );
        commit_all(f.mono.path(), "local edit");
        let report = check(
            f.mono.path(),
            "HEAD",
            f.upstream.path(),
            &definition(&f.commit, &[]),
        )
        .unwrap();
        assert!(!report.clean());
        assert_eq!(report.drift, vec![Divergence::Modified("src/a.ts".into())]);
    }

    #[test]
    fn a_declared_local_path_is_not_drift() {
        let f = fixture();
        write(
            f.mono.path(),
            "vendor/thing/src/a.ts",
            "export const a = 99;\n",
        );
        commit_all(f.mono.path(), "local edit");
        let def = definition(&f.commit, &["src/a.ts"]);
        let report = check(f.mono.path(), "HEAD", f.upstream.path(), &def).unwrap();
        assert!(report.clean(), "{:?}", report.drift);
        assert_eq!(
            report.declared,
            vec![Divergence::Modified("src/a.ts".into())]
        );
    }

    #[test]
    fn a_dropped_upstream_file_is_missing_not_silence() {
        let f = fixture();
        std::fs::remove_file(f.mono.path().join("vendor/thing/src/b.ts")).unwrap();
        commit_all(f.mono.path(), "drop b");
        let report = check(
            f.mono.path(),
            "HEAD",
            f.upstream.path(),
            &definition(&f.commit, &[]),
        )
        .unwrap();
        assert_eq!(report.drift, vec![Divergence::Missing("src/b.ts".into())]);
    }

    #[test]
    fn a_file_we_added_is_extra() {
        let f = fixture();
        write(f.mono.path(), "vendor/thing/src/local.ts", "ours\n");
        commit_all(f.mono.path(), "add local");
        let report = check(
            f.mono.path(),
            "HEAD",
            f.upstream.path(),
            &definition(&f.commit, &[]),
        )
        .unwrap();
        assert_eq!(report.drift, vec![Divergence::Extra("src/local.ts".into())]);

        let declared = definition(&f.commit, &["src/local.ts"]);
        let report = check(f.mono.path(), "HEAD", f.upstream.path(), &declared).unwrap();
        assert!(report.clean());
    }

    #[test]
    fn exclude_narrows_the_upstream_selection() {
        let f = fixture();
        std::fs::remove_file(f.mono.path().join("vendor/thing/src/b.ts")).unwrap();
        commit_all(f.mono.path(), "drop b");
        let mut def = definition(&f.commit, &[]);
        def.upstream.exclude = vec!["src/b.ts".into()];
        let report = check(f.mono.path(), "HEAD", f.upstream.path(), &def).unwrap();
        assert!(report.clean(), "{:?}", report.drift);
        assert_eq!(report.selected, 1);
    }

    #[test]
    fn a_short_or_uppercase_pin_is_rejected() {
        let f = fixture();
        let short = serde_json::from_value::<VendorDefinition>(serde_json::json!({
            "schemaVersion": 1, "name": "t", "class": "vendor-import",
            "upstream": { "repository": "r", "commit": &f.commit[..8], "include": ["src/**"] },
            "destination": { "path": "vendor/thing" }
        }))
        .unwrap();
        assert!(short.validate().is_err());

        let upper = serde_json::from_value::<VendorDefinition>(serde_json::json!({
            "schemaVersion": 1, "name": "t", "class": "vendor-import",
            "upstream": { "repository": "r", "commit": f.commit.to_uppercase(), "include": ["src/**"] },
            "destination": { "path": "vendor/thing" }
        }))
        .unwrap();
        assert!(upper.validate().is_err());
    }

    #[test]
    fn an_escaping_destination_is_rejected() {
        let f = fixture();
        let mut def = definition(&f.commit, &[]);
        def.destination.path = "vendor/../../etc".into();
        assert!(def.validate().is_err());
    }

    #[test]
    fn the_wrong_class_is_rejected() {
        let f = fixture();
        let mut def = definition(&f.commit, &[]);
        def.class = "source-tree".into();
        assert!(def.validate().is_err());
    }
}
