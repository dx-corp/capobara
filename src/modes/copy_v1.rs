//! The `copy-v1` mode: literal file mappings from a source tree onto a
//! managed subtree of the destination. Ports the `copy-v1` branch of
//! `buildProjection` and `mayContainIncluded` from
//! `scripts/projections/project.mjs`.

use std::path::Path;

use crate::definition::Definition;
use crate::tree::{Entries, Matcher, contained_path, files_under, read_entry};
use crate::{Result, contract};

/// True when `path` names a file or directory that could contain (or itself
/// be) something matched by one of `patterns`. Used to prune `files_under`'s
/// walk of a mapping's source directory: a directory that fails this check
/// cannot hold any included file, so it is never descended into.
///
/// A pattern matches directly when it matches `path` as a glob, or when the
/// pattern names something nested under `path` (`pattern` starts with
/// `"{path}/"`, covering literal include patterns like `"README.md"` while
/// `path` is still `""`-rooted ancestry such as `"."`... in practice, a
/// shorter directory prefix of a literal file pattern).
///
/// When `pattern` contains a wildcard, `prefix` is the fixed text before the
/// first wildcard character with its trailing partial path segment removed
/// (the slash before that segment is kept): `"src/**"` yields `"src/"`,
/// `"*.md"` yields `""`. `path` may then be an ancestor of the pattern
/// (`path` starts with `prefix`) or a descendant of it (`prefix` starts with
/// `"{path}/"`).
pub fn may_contain_included(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        let single = std::slice::from_ref(pattern);
        if Matcher::new(single)
            .map(|m| m.matches(path))
            .unwrap_or(false)
        {
            return true;
        }
        if pattern.starts_with(&format!("{path}/")) {
            return true;
        }
        let Some(wildcard) = pattern.find(['*', '?', '[', ']']) else {
            return false;
        };
        let before = &pattern[..wildcard];
        let prefix = match before.rfind('/') {
            Some(slash) => &before[..=slash],
            None => "",
        };
        path.starts_with(prefix) || prefix.starts_with(&format!("{path}/"))
    })
}

/// Collects the entries and candidate deletions for a `copy-v1` definition.
/// `owned` is `Matcher::new(&definition.destination_owned)`, built once by
/// the caller (`build::build_projection`) since it is needed for both the
/// mode-specific collection here and the shared build tail.
///
/// For each mapping, candidate source paths are the mapping's own `include`
/// list verbatim when `mapping.source` is `"."` (a root mapping, restricted
/// by definition validation to exact root files), otherwise every file under
/// the mapping's source directory that is not excluded and might be
/// included (`may_contain_included`). Each candidate that is actually
/// included and not excluded is copied to `path` (or
/// `"{destination}/{path}"`); a target claimed by an earlier mapping is a
/// `Contract` error, and so is a target outside the definition's managed
/// output boundary.
///
/// Deletion candidates are every non-owned file already under `target_root`
/// that falls inside the managed boundary; `build::build_projection`
/// narrows this further (dropping paths that are about to be (re)written)
/// and checks the rest against ownership again before planning.
pub fn collect(
    definition: &Definition,
    source_root: &Path,
    target_root: &Path,
    owned: &Matcher,
) -> Result<(Entries, Vec<String>)> {
    let output_managed = definition
        .output_managed
        .as_ref()
        .expect("copy-v1 definitions always carry outputManaged");
    let managed = Matcher::new(output_managed)?;

    let mut entries: Entries = Entries::new();
    for mapping in &definition.mappings {
        let source_dir = if mapping.source == "." {
            source_root.to_path_buf()
        } else {
            contained_path(source_root, &mapping.source)?
        };
        let included = Matcher::new(&mapping.include)?;
        let excluded = Matcher::new(&mapping.exclude)?;
        let candidates: Vec<String> = if mapping.source == "." {
            mapping.include.clone()
        } else {
            let include = &mapping.include;
            files_under(&source_dir, &|path: &str| {
                excluded.matches(path) || !may_contain_included(path, include)
            })?
        };
        for path in candidates {
            if !included.matches(&path) || excluded.matches(&path) {
                continue;
            }
            let target = if mapping.destination == "." {
                path.clone()
            } else {
                format!("{}/{}", mapping.destination, path)
            };
            contract(
                !entries.contains_key(&target),
                format!("Overlapping mappings: {target}"),
            )?;
            contract(
                managed.matches(&target),
                format!("Copy output outside managed boundary: {target}"),
            )?;
            entries.insert(target, read_entry(&source_dir, &path)?);
        }
    }

    let deletions: Vec<String> = files_under(target_root, &|path: &str| owned.matches(path))?
        .into_iter()
        .filter(|path| managed.matches(path))
        .collect();

    Ok((entries, deletions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_glob_and_ancestor_matches_win_without_a_wildcard_prefix() {
        let patterns = vec!["README.md".to_string()];
        assert!(may_contain_included("README.md", &patterns));
        // "README.md" has no wildcard, so only the direct-match and
        // ancestor-of-pattern branches can fire; a sibling never matches.
        assert!(!may_contain_included("OTHER.md", &patterns));
    }

    #[test]
    fn wildcard_prefix_matches_ancestors_and_descendants_of_the_fixed_text() {
        let patterns = vec!["src/**".to_string()];
        // "src" is an ancestor directory of the pattern's fixed prefix "src/".
        assert!(may_contain_included("src", &patterns));
        // "src/sub" also qualifies (path starts with the prefix).
        assert!(may_contain_included("src/sub", &patterns));
        assert!(!may_contain_included("docs", &patterns));
    }

    #[test]
    fn wildcard_with_no_fixed_directory_prefix_matches_every_path() {
        // "*.md" gives an empty prefix, and every path starts with "", so
        // this conservatively admits any directory as a possible ancestor of
        // a matching file -- matching Node's `path.startsWith(prefix)` with
        // prefix = "".
        let patterns = vec!["*.md".to_string()];
        assert!(may_contain_included("anything/at/all", &patterns));
    }
}
