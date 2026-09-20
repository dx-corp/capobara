// If this test fails, the committed Cargo.lock is stale. In dx-corp/mono,
// regenerate it with `rust/tools/capobara/scripts/standalone-lock.sh` --
// do NOT run `cargo generate-lockfile` from inside `rust/tools/capobara`.
// That crate directory is a member of the `rust/` Cargo workspace and has
// no `[workspace]` table of its own, so Cargo walks up and finds
// `rust/Cargo.lock` instead; running `cargo generate-lockfile` from the
// crate directory silently rewrites the SHARED workspace lock (every other
// crate in Mono) and creates no `rust/tools/capobara/Cargo.lock` at all.
// See the script's own comment for the recipe that actually produces a
// standalone lock in step with the workspace's resolved versions. (The
// script itself is Mono-only tooling and is not projected into this
// repository; if you are reading this file in dx-corp/capobara, the crate
// here already stands alone and plain `cargo generate-lockfile` works.)
//
// This test is Mono-only, like tests/definition_coverage.rs: it reads
// `config/projections/capobara.json` (three directories above the crate)
// and runs `git ls-files` to compute which files to copy, and neither of
// those exists in dx-corp/capobara. Run it only from within dx-corp/mono,
// with `--features mono-fixtures`; run elsewhere (or without the feature)
// it stays `#[ignore]`d either way (it always requires an explicit
// `-- --ignored`, feature or not, since it compiles the crate a second
// time), but forcing it to run anyway will panic in `load_definition`
// rather than doing anything useful.

mod support;

use std::process::Command;

#[test]
#[cfg_attr(
    feature = "mono-fixtures",
    ignore = "compiles the crate a second time; run explicitly in Mono CI and before any Cargo.toml change"
)]
#[cfg_attr(
    not(feature = "mono-fixtures"),
    ignore = "requires --features mono-fixtures: reads config/projections/capobara.json and git ls-files, neither of which exists outside dx-corp/mono"
)]
fn crate_builds_outside_the_workspace_with_the_committed_lockfile() {
    let crate_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scratch = tempfile::tempdir().unwrap();

    // Copy exactly the files config/projections/capobara.json projects --
    // the same include/exclude match `modes::copy_v1::collect` uses in
    // production -- rather than a second, independently-maintained file
    // list that could silently drift from the definition (see
    // tests/definition_coverage.rs).
    let definition = support::capobara_definition();
    let mapping = support::crate_mapping(&definition);
    for path in support::git_ls_files(&crate_dir) {
        if !support::is_projected(mapping, &path) {
            continue;
        }
        let from = crate_dir.join(&path);
        let to = scratch.path().join(&path);
        std::fs::create_dir_all(to.parent().expect("projected paths are never bare roots"))
            .unwrap();
        std::fs::copy(&from, &to).unwrap();
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "test builds the copied crate standalone, outside the workspace"
    )]
    let status = Command::new("cargo")
        // Deliberately NOT `--no-run`. Compiling the projected tree proves
        // the lockfile is in step with the manifest, but it cannot see a test
        // that *runs* against a file the projection does not carry -- and
        // that class of defect is exactly what ships a red `ci` to
        // `dx-corp/capobara`, whose `main` requires that check. Two instances
        // existed when this line was changed: the equivalence test read
        // `EQUIVALENCE.md` (UNPROJECTED) and the SDK-assembly test read
        // `tests/fixtures/definitions/` (excluded by the definition); both
        // now carry a `mono-fixtures` gate, and this executes the suite so
        // the next one cannot land unnoticed.
        .args(["test", "--locked", "--offline"])
        .current_dir(scratch.path())
        .env("CAPOBARA_TREE_ID", "0".repeat(40))
        .env("CARGO_TARGET_DIR", scratch.path().join("target"))
        .status()
        .unwrap();
    assert!(
        status.success(),
        "standalone build failed; the committed Cargo.lock is stale. In dx-corp/mono, regenerate it with rust/tools/capobara/scripts/standalone-lock.sh. Do NOT run `cargo generate-lockfile` inside rust/tools/capobara: the crate is a member of the rust/ workspace and cargo will rewrite rust/Cargo.lock instead."
    );
}
