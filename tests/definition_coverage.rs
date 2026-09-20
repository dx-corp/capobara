//! Guards against a crate file silently falling outside the self-projection
//! (M3: "a later `src/bin/`, `benches/`, `rust-toolchain.toml`, or a data
//! file `build.rs` reads would be omitted from the definition, omitted from
//! the test's scratch copy, and omitted from the lock script ... and no
//! check exists that could have said anything"). These tests are the check.
//! Gated on `mono-fixtures` because they read files that only exist inside
//! `dx-corp/mono` (the crate's own git history, `config/projections/`).

mod support;

/// Paths under `rust/tools/capobara/` that are git-tracked but deliberately
/// NOT part of the public self-projection, with the reason each is
/// excluded. Every other tracked path must be matched by
/// `config/projections/capobara.json`'s include/exclude globs.
const UNPROJECTED: &[(&str, &str)] = &[
    (
        "scripts/standalone-lock.sh",
        "Mono-only maintenance script; it regenerates the standalone Cargo.lock \
     from rust/Cargo.lock, the shared workspace lock, which does not exist \
     in the standalone public repository.",
    ),
    (
        "scripts/equivalence.sh",
        "Mono-only equivalence harness; it builds Capobara at Mono revisions and \
     diffs against the Node projector, which the public repository lacks.",
    ),
    (
        "EQUIVALENCE.md",
        "Record of the Mono-side equivalence runs (Mono SHAs, destination clones); \
     internal evidence, not part of the published crate.",
    ),
];

#[test]
#[cfg_attr(not(feature = "mono-fixtures"), ignore)]
fn every_tracked_crate_file_is_projected_or_explicitly_unprojected() {
    let crate_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let definition = support::capobara_definition();
    let mapping = support::crate_mapping(&definition);
    let tracked = support::git_ls_files(&crate_dir);
    assert!(
        !tracked.is_empty(),
        "git ls-files returned nothing under {}; is this a git checkout?",
        crate_dir.display()
    );

    // "Handled by the definition" means some include pattern names the path
    // at all -- whether it ends up projected (included and not excluded) or
    // deliberately excluded (included, then excluded; e.g.
    // tests/fixtures/definitions/**, which the definition excludes on
    // purpose). Only a path NO include pattern names at all falls outside
    // what the definition's machinery can even decide about, and that's the
    // gap UNPROJECTED exists to name explicitly.
    let allowlisted: Vec<&str> = UNPROJECTED.iter().map(|(path, _)| *path).collect();
    let unmatched: Vec<String> = tracked
        .iter()
        .filter(|path| {
            !support::is_named_by_include(mapping, path) && !allowlisted.contains(&path.as_str())
        })
        .cloned()
        .collect();
    assert!(
        unmatched.is_empty(),
        "these git-tracked files under rust/tools/capobara/ are named by \
         none of config/projections/capobara.json's include patterns and \
         are not in tests/definition_coverage.rs's UNPROJECTED allowlist \
         -- add each one to whichever is correct: {unmatched:?}"
    );

    for (path, _reason) in UNPROJECTED {
        assert!(
            tracked.iter().any(|tracked_path| tracked_path == path),
            "UNPROJECTED names {path}, which is no longer a tracked file; remove the stale entry"
        );
        // The allowlist's whole invariant is "no include pattern names this
        // path" -- a path an include pattern DOES name (whether ultimately
        // projected, or deliberately excluded, like
        // tests/fixtures/definitions/**) is already handled by the
        // definition itself and needs no allowlist entry. `is_projected`
        // alone would miss the excluded-but-named case (it's `false` for
        // both reasons), so this checks `is_named_by_include`, not
        // `is_projected`.
        assert!(
            !support::is_named_by_include(mapping, path),
            "{path} is in UNPROJECTED but an include pattern in \
             config/projections/capobara.json already names it (whether or \
             not an exclude pattern then removes it) -- the definition \
             already handles this path; remove the stale allowlist entry"
        );
    }
}

#[test]
#[cfg_attr(not(feature = "mono-fixtures"), ignore)]
fn standalone_lock_script_copies_exactly_the_projected_source_set() {
    // scripts/standalone-lock.sh keeps its own literal copy list (a root
    // `for f in ...` loop plus `cp -R` directory lines) instead of deriving
    // it dynamically -- see the fix-round-2 report for why. This test is
    // the mechanical check that list owes in exchange: its declared
    // top-level copy set, normalized ("dir" for a `cp -R ... dir` line,
    // "dir/**" is then compared against the definition's own directory
    // patterns), must equal the definition's top-level include set minus
    // `Cargo.lock` -- the one entry the script deliberately does not copy
    // from the crate, because regenerating it *is* the script's job.
    let crate_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(crate_dir.join("scripts/standalone-lock.sh")).unwrap();

    let mut script_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let for_line = script
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with("for f in "))
        .expect("scripts/standalone-lock.sh has no `for f in ...; do` root-file copy loop");
    let names = for_line
        .trim_start_matches("for f in ")
        .split(';')
        .next()
        .expect("`for f in ...; do` line has no `;`");
    for name in names.split_whitespace() {
        script_files.insert(name.to_owned());
    }
    for line in script.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("cp -R \"$crate_dir/") {
            let dir = rest
                .split('"')
                .next()
                .expect("`cp -R \"$crate_dir/...\"` line has no closing quote");
            script_files.insert(format!("{dir}/**"));
        }
    }
    assert!(
        script.contains("$scratch/Cargo.lock"),
        "scripts/standalone-lock.sh no longer seeds Cargo.lock from the workspace lock; \
         update this test's Cargo.lock special-casing if that's intentional"
    );

    let definition = support::capobara_definition();
    let mapping = support::crate_mapping(&definition);
    let mut projected_top_level: std::collections::BTreeSet<String> =
        mapping.include.iter().cloned().collect();
    projected_top_level.remove("Cargo.lock");

    assert_eq!(
        script_files, projected_top_level,
        "scripts/standalone-lock.sh's copy list has drifted from \
         config/projections/capobara.json's include list (Cargo.lock is \
         correctly excluded from both sides -- the script regenerates it \
         instead of copying it)"
    );

    // The script also deletes what the definition excludes
    // (`rm -rf "$scratch/tests/fixtures/definitions"`, matching
    // `exclude: ["tests/fixtures/definitions/**"]`) so its scratch copy is
    // the true projected set, not a superset -- cross-check that too, the
    // same way: parse the script's `rm -rf "$scratch/..."` lines and
    // compare against `mapping.exclude` with each pattern's trailing
    // `/**` stripped.
    let mut script_deleted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in script.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("rm -rf \"$scratch/") {
            let dir = rest
                .split('"')
                .next()
                .expect("`rm -rf \"$scratch/...\"` line has no closing quote");
            script_deleted.insert(dir.to_owned());
        }
    }
    let excluded_top_level: std::collections::BTreeSet<String> = mapping
        .exclude
        .iter()
        .map(|pattern| pattern.strip_suffix("/**").unwrap_or(pattern).to_owned())
        .collect();
    assert_eq!(
        script_deleted, excluded_top_level,
        "scripts/standalone-lock.sh's `rm -rf` deletions have drifted from \
         config/projections/capobara.json's exclude list"
    );
}
