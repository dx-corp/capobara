//! Integration tests for `capobara::transport::git`, using `RecordedApi`
//! (available here via the `recorded-api` feature). Ports, by name, six
//! tests from `scripts/projections/transport.test.mjs`:
//! `complete_candidate_rejects_committed_historical_additions_and_destination_owned_changes`,
//! `complete_candidate_accepts_stable_branch_reuse_after_a_squash_merge`,
//! `squash_merged_stable_branch_is_unchanged_when_main_already_has_the_projection`,
//! `a_new_revision_replaces_an_unreachable_sync_branch_base_with_durable_main`,
//! `publisher_rejects_report_tampering_and_unrelated_staged_files_before_committing`,
//! `a_hold_appearing_after_local_validation_stops_every_remote_write`.
//!
//! The destination checkout (`target`) is a clone of a local bare
//! repository (`bare`). `assert_destination_checkout` requires `origin` to
//! be the GitHub HTTPS URL for the destination repository, so `target`'s
//! `origin` is rewritten to that fake URL immediately after the initial
//! seed push; `bare` itself is inspected directly (never through a remote
//! name) whenever a test needs to prove nothing was pushed. This is the
//! smaller diff against `push_sync_branch`'s two-parameter signature (it
//! always pushes to `origin`, matching `transport.mjs`). None of the six
//! ported tests reaches a successful push (each either errors out, or
//! resolves to `Unchanged`/`Held` beforehand), so `push_sync_branch` is
//! covered separately by `push_sync_branch_body` below, which points
//! `origin` back at `bare` first so the push stays offline.
//!
//! Two hermetic mechanisms keep `tooldigest::embedded()` matching this
//! file's synthetic `rust/tools/capobara` stand-in, without ever needing
//! `CAPOBARA_TREE_ID_OVERRIDE` exported before `cargo test` starts (this
//! crate forbids unsafe code, so the same-process `std::env::set_var` --
//! `unsafe` as of this toolchain -- is not an option either):
//! - `Fixture`'s own `plan`/`apply`/`verify` steps run by spawning the
//!   compiled `capobara` binary (`run_capobara`, the same pattern
//!   `tests/project_cli.rs` uses), with `CAPOBARA_TREE_ID_OVERRIDE` set on
//!   that one child process only.
//! - Every other call goes through `transport::git`'s production functions
//!   (`assert_candidate_matches_main_projection`, `publish_prepared_tree`),
//!   which call `cli::project::run` in-process by design (never spawning a
//!   binary themselves) -- there is no `Command` for those to attach an
//!   env override to. `Fixture::new` instead calls
//!   `capobara::tooldigest::override_for_tests(&tree_id)` once per fixture,
//!   which pins this *test* process's own `tooldigest::embedded()` before
//!   any in-process call can read it.

// The in-process tooldigest override (`tooldigest::override_for_tests`) this
// file's fixture relies on exists only in debug builds; compile this whole
// test target to an empty binary under any profile with debug_assertions
// off (e.g. `cargo test --release`) instead of failing to compile.
#![cfg(debug_assertions)]
#![allow(dead_code)]
macro_rules! cargo_bin {
    ($name:literal) => {{
        option_env!(concat!("CARGO_BIN_EXE_", $name))
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os(concat!("CARGO_BIN_EXE_", $name)).map(std::path::PathBuf::from)
            })
            .expect(concat!("Cargo binary path unavailable: ", $name))
    }};
}

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use capobara::definition::{Definition, definition_from_value};
use capobara::receipt::Provenance;
use capobara::report::Report;
use capobara::transport::{
    Published, RecordedApi, assert_candidate_matches_main_projection, prepare_destination,
    publish_prepared_tree, push_sync_branch,
};
use support::Repo;

const REPO: &str = "test/public";
const SYNC_BRANCH: &str = "sync/mono-projection";
const STAND_IN: &[u8] = b"// stand-in for the crate tree";

const REPOS_ENDPOINT: &str = "repos/test/public";
const INSTALLATION_ENDPOINT: &str = "installation/repositories?per_page=100";
const PULLS_ENDPOINT: &str =
    "repos/test/public/pulls?state=open&base=main&head=test%3Async%2Fmono-projection";

fn no_sdk(_: &str) -> Option<Vec<String>> {
    None
}

fn definition_json() -> Value {
    json!({
        "schemaVersion": 1, "name": "fixture", "class": "source-tree", "mode": "copy-v1",
        "sourceRepository": "test/mono", "visibility": "public",
        "mappings": [
            {"source": "source", "destination": ".", "include": ["src/**", "README.md"], "exclude": []}
        ],
        "destination": {
            "repository": REPO, "branch": "main", "syncBranch": SYNC_BRANCH, "holdLabel": "sync-hold"
        },
        "destinationOwned": [".github/**", "SECURITY.md"],
        "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
        "outputManaged": ["src/**", "README.md"]
    })
}

fn info_response() -> Value {
    json!({
        "full_name": REPO, "archived": false, "disabled": false,
        "default_branch": "main", "visibility": "public",
    })
}

fn installation_response() -> Value {
    json!({"total_count": 1, "repositories": [{"full_name": REPO}]})
}

fn no_pr_api() -> RecordedApi {
    RecordedApi::new(vec![
        ("GET", REPOS_ENDPOINT, info_response()),
        ("GET", INSTALLATION_ENDPOINT, installation_response()),
        ("GET", PULLS_ENDPOINT, json!([])),
    ])
}

/// An open, non-held PR whose head is exactly `head_sha` -- used to exercise
/// `prepare_destination`'s "destination branch advanced" check on the path
/// that reuses an existing sync branch.
fn open_pr_api(head_sha: &str) -> RecordedApi {
    let pr = json!({
        "number": 1,
        "labels": [],
        "head": {
            "repo": {"full_name": REPO},
            "ref": SYNC_BRANCH,
            "sha": head_sha,
        },
        "base": {"ref": "main"},
    });
    RecordedApi::new(vec![
        ("GET", REPOS_ENDPOINT, info_response()),
        ("GET", INSTALLATION_ENDPOINT, installation_response()),
        ("GET", PULLS_ENDPOINT, json!([pr])),
    ])
}

fn read_report(path: &Path) -> Report {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn write_report(path: &Path, report: &Report) {
    std::fs::write(path, serde_json::to_string(report).unwrap()).unwrap();
}

fn capobara() -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test executes the capobara binary"
    )]
    Command::new(cargo_bin!("capobara"))
}

/// Runs `capobara <command> --definition ... --source ... --source-sha ...
/// --target ... --report ...` as a child process, with
/// `CAPOBARA_TREE_ID_OVERRIDE` set on that child only (see the module doc
/// comment for why an in-process call cannot do this hermetically).
/// Asserts the child exited successfully, including its stderr in the
/// panic message otherwise -- exactly `tests/project_cli.rs`'s pattern.
fn run_capobara(
    command: &str,
    definition: &Path,
    source: &Path,
    source_sha: &str,
    target: &Path,
    report: &Path,
    tree_id: &str,
) {
    let output = capobara()
        .env("CAPOBARA_TREE_ID_OVERRIDE", tree_id)
        .args([command, "--definition"])
        .arg(definition)
        .arg("--source")
        .arg(source)
        .arg("--source-sha")
        .arg(source_sha)
        .arg("--target")
        .arg(target)
        .arg("--report")
        .arg(report)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "capobara {command} failed (status {:?}): stderr={} stdout={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

fn run_git(path: &Path, args: &[&str]) -> String {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration tests drive scratch git repositories outside the support::Repo helper"
    )]
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn clone_no_local(src: &Path, dest: &Path) {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test drives a scratch git repository outside the support::Repo helper"
    )]
    let out = Command::new("git")
        .args(["clone", "--quiet", "--no-local"])
        .arg(src)
        .arg(dest)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git clone --no-local: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_exists(path: &Path, sha: &str) -> bool {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test probes object reachability in a scratch git repository"
    )]
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn ref_exists(path: &Path, refname: &str) -> bool {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test inspects a scratch bare git repository's refs"
    )]
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["show-ref", "--verify", "--quiet", refname])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// A plain git checkout outside the `support::Repo` helper: used for the
/// fresh clone in test 4, which is created by `git clone` itself rather
/// than `Repo::init`.
struct RawRepo(PathBuf);

impl RawRepo {
    fn path(&self) -> &Path {
        &self.0
    }
    fn git(&self, args: &[&str]) -> String {
        run_git(&self.0, args)
    }
}

struct Fixture {
    definition: Definition,
    definition_text: String,
    source: Repo,
    source_sha: String,
    /// `rust/tools/capobara`'s tree id at `source_sha`, computed once (the
    /// stand-in file never changes across any commit any test makes to
    /// `source`, so one value is valid for every `run_capobara` call this
    /// fixture makes).
    tree_id: String,
    target: Repo,
    bare: PathBuf,
    report_path: PathBuf,
    base: String,
}

impl Fixture {
    fn new() -> Fixture {
        // `.keep()` deliberately leaks these scratch directories for the
        // test process's lifetime (matching `support::Repo::into_path`'s
        // own convention), so `Fixture` never needs to hold a `TempDir`
        // field purely to keep it alive.
        let bare = tempfile::tempdir().unwrap().keep();
        run_git(&bare, &["init", "-q", "--bare", "-b", "main"]);

        let source = Repo::init("https://github.com/test/mono.git");
        source.write("source/src/client.txt", b"public code\n");
        source.write("source/README.md", b"SDK\n");
        let definition_text = serde_json::to_string(&definition_json()).unwrap();
        source.write(
            "config/projections/fixture.json",
            definition_text.as_bytes(),
        );
        source.write("rust/tools/capobara/src/lib.rs", STAND_IN);
        let source_sha = source.commit("source");
        let tree_id = source
            .git(&["rev-parse", "HEAD:rust/tools/capobara"])
            .trim()
            .to_owned();
        // Pins this process's `tooldigest::embedded()` before any in-process
        // `cli::project::run` call -- including the ones made internally by
        // `assert_candidate_matches_main_projection`/`publish_prepared_tree`,
        // which have no `Command` to attach an env override to. Every
        // fixture in this file writes the same `STAND_IN` bytes, so every
        // call computes the same `tree_id` and this is a no-op after the
        // first fixture in this test binary's process.
        capobara::tooldigest::override_for_tests(&tree_id);

        let target = Repo::init(bare.to_str().unwrap());
        target.write("SECURITY.md", b"owned\n");
        target.write(".github/workflows/ci.yml", b"ci\n");
        let base = target.commit("destination");
        target.git(&["push", "-q", "origin", "main"]);
        target.git(&["update-ref", "refs/remotes/origin/main", &base]);
        target.git(&[
            "remote",
            "set-url",
            "origin",
            &format!("https://github.com/{REPO}.git"),
        ]);
        target.git(&["switch", "-c", SYNC_BRANCH]);

        let definition = definition_from_value(definition_json(), &no_sdk)
            .unwrap()
            .definition;

        let report_path = tempfile::tempdir().unwrap().keep().join("report.json");

        run_capobara(
            "apply",
            &source.path().join("config/projections/fixture.json"),
            source.path(),
            &source_sha,
            target.path(),
            &report_path,
            &tree_id,
        );

        Fixture {
            definition,
            definition_text,
            source,
            source_sha,
            tree_id,
            target,
            bare,
            report_path,
            base,
        }
    }

    fn apply(&self, source_sha: &str) {
        run_capobara(
            "apply",
            &self.source.path().join("config/projections/fixture.json"),
            self.source.path(),
            source_sha,
            self.target.path(),
            &self.report_path,
            &self.tree_id,
        );
    }
}

#[test]
fn complete_candidate_rejects_committed_historical_additions_and_destination_owned_changes() {
    for (label, path, content) in [
        ("unmanaged source", "historical.txt", &b"unreviewed\n"[..]),
        (
            "destination policy",
            ".github/workflows/ci.yml",
            &b"sync-only policy\n"[..],
        ),
    ] {
        let f = Fixture::new();
        f.target.commit("initial projection");
        f.target.write(path, content);
        f.target.commit("historical change");
        let err = assert_candidate_matches_main_projection(
            &f.definition,
            f.source.path(),
            &f.source_sha,
            f.target.path(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.starts_with(
                "Candidate tree differs from destination main plus the verified projection"
            ),
            "{label}: unexpected error: {err}"
        );
    }
}

#[test]
fn complete_candidate_accepts_stable_branch_reuse_after_a_squash_merge() {
    let mut f = Fixture::new();
    let sync_head = f.target.commit("initial projection");
    f.target.git(&["switch", "main"]);
    f.target.git(&["checkout", SYNC_BRANCH, "--", "."]);
    let squash_head = f.target.commit("squash fixture");
    f.target
        .git(&["update-ref", "refs/remotes/origin/main", &squash_head]);
    f.target.git(&[
        "update-ref",
        &format!("refs/remotes/origin/{SYNC_BRANCH}"),
        &sync_head,
    ]);
    f.target.git(&["branch", "-D", SYNC_BRANCH]);

    f.source.write("source/src/client.txt", b"public code v2\n");
    f.source_sha = f.source.commit("second source revision");

    let prepared = prepare_destination(
        &f.definition,
        &f.definition_text,
        f.target.path(),
        &f.source_sha,
        &no_pr_api(),
    )
    .unwrap();
    assert!(!prepared.held);

    f.apply(&f.source_sha);

    if let Err(e) = assert_candidate_matches_main_projection(
        &f.definition,
        f.source.path(),
        &f.source_sha,
        f.target.path(),
    ) {
        panic!("{e}");
    }
}

#[test]
fn squash_merged_stable_branch_is_unchanged_when_main_already_has_the_projection() {
    let f = Fixture::new();
    let sync_head = f.target.commit("initial projection");
    f.target.git(&["switch", "main"]);
    f.target.git(&["checkout", SYNC_BRANCH, "--", "."]);
    let squash_head = f.target.commit("squash fixture");
    f.target
        .git(&["update-ref", "refs/remotes/origin/main", &squash_head]);
    f.target.git(&[
        "update-ref",
        &format!("refs/remotes/origin/{SYNC_BRANCH}"),
        &sync_head,
    ]);
    f.target.git(&["branch", "-D", SYNC_BRANCH]);

    let prepared = prepare_destination(
        &f.definition,
        &f.definition_text,
        f.target.path(),
        &f.source_sha,
        &no_pr_api(),
    )
    .unwrap();
    assert!(!prepared.held);

    f.apply(&f.source_sha);

    let report = read_report(&f.report_path);
    let result = publish_prepared_tree(
        &f.definition,
        f.source.path(),
        &f.source_sha,
        f.target.path(),
        &report,
        &no_pr_api(),
    )
    .unwrap();
    assert!(matches!(result, Published::Unchanged));
}

#[test]
fn a_new_revision_replaces_an_unreachable_sync_branch_base_with_durable_main() {
    let mut f = Fixture::new();
    f.target.commit("first projection");
    f.target.git(&["switch", "main"]);
    f.target.git(&["checkout", SYNC_BRANCH, "--", "."]);
    f.target.commit("first squash merge");

    f.target.git(&["switch", SYNC_BRANCH]);
    f.target.git(&["merge", "--no-ff", "--no-edit", "main"]);
    let transient_base = f.target.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let receipt_path = f.target.path().join(&f.definition.provenance);
    // Round-trip through the typed `Provenance` (not a raw `Value`) and
    // re-serialize with its own `to_receipt_bytes`, so the rewritten
    // receipt is byte-identical, field order included, to what
    // `build_projection` itself would write for these field values. A
    // `Value`-based rewrite is not safe here: `serde_json::Value`'s object
    // map does not preserve the original file's key order, so a later
    // `verify` rebuild -- which writes a receipt via `to_receipt_bytes` --
    // would (correctly) detect that on-disk byte sequence as drift, even
    // though every logical field matches.
    let mut receipt: Provenance =
        serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    receipt.prior_projected_base = transient_base.clone();
    std::fs::write(&receipt_path, receipt.to_receipt_bytes()).unwrap();
    f.target.commit("projection with a transient base");

    f.target.git(&["switch", "main"]);
    f.target.git(&["checkout", SYNC_BRANCH, "--", "."]);
    let squash_head = f.target.commit("second squash merge");
    f.target.git(&["branch", "-D", SYNC_BRANCH]);

    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh_path = fresh_dir.path().join("fresh-public");
    clone_no_local(f.target.path(), &fresh_path);
    let fresh = RawRepo(fresh_path);
    fresh.git(&[
        "remote",
        "set-url",
        "origin",
        &format!("https://github.com/{REPO}.git"),
    ]);

    // Exact main remains an authoritative verification boundary even though
    // the historical sync-branch merge object is absent from a clean clone.
    assert!(
        !commit_exists(fresh.path(), &transient_base),
        "transient base unexpectedly reachable in a fresh clone"
    );

    run_capobara(
        "verify",
        &f.source.path().join("config/projections/fixture.json"),
        f.source.path(),
        &f.source_sha,
        fresh.path(),
        &f.report_path,
        &f.tree_id,
    );

    f.source.write("source/src/client.txt", b"public code v2\n");
    f.source_sha = f.source.commit("second source revision");

    let prepared = prepare_destination(
        &f.definition,
        &f.definition_text,
        fresh.path(),
        &f.source_sha,
        &no_pr_api(),
    )
    .unwrap();
    assert!(!prepared.held);

    run_capobara(
        "apply",
        &f.source.path().join("config/projections/fixture.json"),
        f.source.path(),
        &f.source_sha,
        fresh.path(),
        &f.report_path,
        &f.tree_id,
    );

    let updated: Provenance = serde_json::from_slice(
        &std::fs::read(fresh.path().join(&f.definition.provenance)).unwrap(),
    )
    .unwrap();
    assert_eq!(updated.prior_projected_base, squash_head);
}

#[test]
fn publisher_rejects_report_tampering_and_unrelated_staged_files_before_committing() {
    let f = Fixture::new();
    let report = read_report(&f.report_path);

    f.target.write("unreviewed.txt", b"not an approved output");
    let mut tampered = report.clone();
    tampered.copied_paths.push("unreviewed.txt".to_string());
    tampered.copied_count += 1;
    write_report(&f.report_path, &tampered);

    let err = publish_prepared_tree(
        &f.definition,
        f.source.path(),
        &f.source_sha,
        f.target.path(),
        &tampered,
        &no_pr_api(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.starts_with("Untrusted publication report"),
        "unexpected error: {err}"
    );
    assert_eq!(f.target.head(), f.base);

    write_report(&f.report_path, &report);
    f.target.git(&["add", "unreviewed.txt"]);
    let err = publish_prepared_tree(
        &f.definition,
        f.source.path(),
        &f.source_sha,
        f.target.path(),
        &report,
        &no_pr_api(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.starts_with(
            "Candidate tree differs from destination main plus the verified projection"
        ),
        "unexpected error: {err}"
    );
    assert_eq!(f.target.head(), f.base);
}

#[test]
fn a_hold_appearing_after_local_validation_stops_every_remote_write() {
    let f = Fixture::new();
    let report = read_report(&f.report_path);

    let held_pr = json!({
        "number": 1,
        "labels": [{"name": "sync-hold"}],
        "head": {
            "repo": {"full_name": REPO},
            "ref": SYNC_BRANCH,
            "sha": "a".repeat(40),
        },
        "base": {"ref": "main"},
    });
    let held_api = RecordedApi::new(vec![
        ("GET", REPOS_ENDPOINT, info_response()),
        ("GET", INSTALLATION_ENDPOINT, installation_response()),
        ("GET", PULLS_ENDPOINT, json!([])),
        ("GET", REPOS_ENDPOINT, info_response()),
        ("GET", INSTALLATION_ENDPOINT, installation_response()),
        ("GET", PULLS_ENDPOINT, json!([held_pr])),
    ]);

    let result = publish_prepared_tree(
        &f.definition,
        f.source.path(),
        &f.source_sha,
        f.target.path(),
        &report,
        &held_api,
    )
    .unwrap();
    assert!(matches!(result, Published::Held));
    assert_eq!(
        held_api.calls().len(),
        6,
        "no write call should be attempted"
    );
    assert_ne!(
        f.target.head(),
        f.base,
        "the local commit still happens before the second (held) read"
    );

    assert!(
        !ref_exists(&f.bare, &format!("refs/heads/{SYNC_BRANCH}")),
        "sync branch unexpectedly present on the bare remote: push_sync_branch must not have run"
    );
}

/// Fix round 1: a stored receipt that isn't parseable JSON at all is a hard
/// failure (`Error::Invalid`), matching Node's `JSON.parse` throwing --
/// never silently treated as "no previous receipt" (which would otherwise
/// force an unconditional, unexamined merge).
#[test]
fn prepare_rejects_a_malformed_stored_receipt() {
    let f = Fixture::new();
    f.target.commit("initial projection");
    let receipt_path = f.target.path().join(&f.definition.provenance);
    std::fs::write(&receipt_path, b"{not json").unwrap();
    let corrupt_head = f.target.commit("corrupt receipt");

    f.target.git(&[
        "update-ref",
        &format!("refs/remotes/origin/{SYNC_BRANCH}"),
        &corrupt_head,
    ]);

    // Advance origin/main past corrupt_head's own history first: without
    // this, origin/main is still an ancestor of corrupt_head, so even a
    // wrongly-attempted merge would be a no-op ("Already up to date") and
    // leave HEAD unchanged regardless of whether prepare_destination
    // correctly refused to merge -- the assertion below would hold either
    // way and prove nothing. With a real, divergent commit on main, a
    // wrongly-attempted merge would produce a new merge commit and move
    // HEAD, so the assertion actually discriminates.
    f.target.git(&["switch", "main"]);
    f.target.write("NOTICE.md", b"main advanced\n");
    let main_tip = f.target.commit("advance main");
    f.target
        .git(&["update-ref", "refs/remotes/origin/main", &main_tip]);
    f.target.git(&["branch", "-D", SYNC_BRANCH]);

    let err = prepare_destination(
        &f.definition,
        &f.definition_text,
        f.target.path(),
        &f.source_sha,
        &open_pr_api(&corrupt_head),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(err, "Malformed stored provenance");
    assert_eq!(
        f.target.head(),
        corrupt_head,
        "no merge should have happened after a hard parse failure"
    );
}

/// Fix round 1: a stored receipt that parses as JSON but is missing
/// `sourceSha` reads as `None` (not a match for the current source SHA),
/// which -- exactly like Node's `previous?.sourceSha !== sourceSha` --
/// triggers the merge, rather than being rejected outright (Node performs
/// no key-set validation in `prepareDestination`).
#[test]
fn prepare_merges_main_when_the_stored_receipt_lacks_a_source_sha() {
    let f = Fixture::new();
    f.target.commit("initial projection");

    let receipt_path = f.target.path().join(&f.definition.provenance);
    let mut receipt: Value =
        serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    receipt.as_object_mut().unwrap().remove("sourceSha");
    std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let stale_head = f.target.commit("receipt missing sourceSha");
    f.target.git(&[
        "update-ref",
        &format!("refs/remotes/origin/{SYNC_BRANCH}"),
        &stale_head,
    ]);

    f.target.git(&["switch", "main"]);
    f.target.write("NOTICE.md", b"main advanced\n");
    let main_tip = f.target.commit("advance main");
    f.target
        .git(&["update-ref", "refs/remotes/origin/main", &main_tip]);
    f.target.git(&["branch", "-D", SYNC_BRANCH]);

    let prepared = prepare_destination(
        &f.definition,
        &f.definition_text,
        f.target.path(),
        &f.source_sha,
        &no_pr_api(),
    )
    .unwrap();
    assert!(!prepared.held);

    assert_ne!(
        f.target.head(),
        stale_head,
        "a merge commit should have been created"
    );
    assert!(
        f.target.path().join("NOTICE.md").exists(),
        "main's tip should have been merged into the sync branch"
    );
}

/// `git ls-remote <bare> <refname>`, run from `from`: the SHA the bare
/// repository currently advertises for `refname`, or `None` when it has no
/// such ref. Reads the remote the way a third party would, never through
/// `target`'s remote-tracking refs (which a push updates locally too).
fn ls_remote_sha(from: &Path, remote: &Path, refname: &str) -> Option<String> {
    let out = run_git(from, &["ls-remote", remote.to_str().unwrap(), refname]);
    out.split_whitespace().next().map(str::to_owned)
}

fn self_exe() -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test re-executes its own test binary with GH_TOKEN set"
    )]
    Command::new(std::env::current_exe().unwrap())
}

/// Fix round 5 (D1): Node runs the projector's `verify` through
/// `execFileSync`, which throws on a non-zero exit, so a `verify` that
/// reports drift aborts publication before the baseline plan, before any
/// staging or commit, and before any remote write. The Rust port runs
/// `verify` in-process, where drift arrives as `Ok(1)` rather than as an
/// `Err` -- a status `?` alone discards.
#[test]
fn a_drifting_verify_aborts_publication_before_any_remote_write() {
    let f = Fixture::new();
    let report = read_report(&f.report_path);

    // `Fixture::new`'s `apply` left the projection in the working tree and
    // a matching report on disk. Rewriting one projected file now is pure
    // drift: `verify` rebuilds the plan against this tree, finds one file
    // to re-copy, and exits 1 -- while still writing a report whose
    // provenance matches `report`'s, so the provenance equality check
    // downstream would not notice a thing.
    f.target
        .write("src/client.txt", b"tampered by the destination\n");

    let api = no_pr_api();
    let err = publish_prepared_tree(
        &f.definition,
        f.source.path(),
        &f.source_sha,
        f.target.path(),
        &report,
        &api,
    )
    .unwrap_err()
    .to_string();

    // Asserting the *verify* message, not merely `Err`: with the exit
    // status discarded this same tampering still fails, but much later, at
    // `assert_candidate_matches_main_projection` ("Candidate tree differs
    // from destination main plus the verified projection") -- after a
    // baseline clone and a second full projection. A bare `is_err()` could
    // not tell the two apart, so only this prefix proves publication
    // stopped at `verify`.
    assert!(
        err.starts_with("Command failed: capobara verify --definition "),
        "unexpected error: {err}"
    );

    assert_eq!(
        f.target.head(),
        f.base,
        "nothing may be committed once verify has failed"
    );
    assert!(
        !ref_exists(&f.bare, &format!("refs/heads/{SYNC_BRANCH}")),
        "sync branch unexpectedly present on the bare remote: no push may happen"
    );
    let calls = api.calls();
    assert_eq!(
        calls.len(),
        3,
        "only the first readPublicationState triple may run: {calls:?}"
    );
    assert!(
        calls.iter().all(|(method, _, _)| method == "GET"),
        "no PR may be created or updated: {calls:?}"
    );
}

/// Fix round 5 (D3): `push_sync_branch` reads `GH_TOKEN` from *this*
/// process's environment, and this crate forbids `unsafe`, so the
/// same-process `std::env::set_var` is not available to install it. This
/// test therefore re-executes this same test binary for the body below,
/// with `GH_TOKEN` set on that one child process only -- the same shape as
/// `run_capobara`'s `CAPOBARA_TREE_ID_OVERRIDE` handling, and the reason
/// the body carries `#[ignore]` (so it never runs in this parent process,
/// where the variable may be absent).
#[test]
fn push_sync_branch_is_covered_by_a_child_process_with_a_token() {
    let output = self_exe()
        .env("GH_TOKEN", "x-test-token")
        .args([
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
            "push_sync_branch_body",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child test binary failed (status {:?}): stdout={stdout} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    // libtest exits 0 when a filter matches nothing, so a typo in the name
    // above would make this a test that cannot fail. Require that the body
    // actually ran.
    assert!(
        stdout.contains("1 passed"),
        "the child did not run push_sync_branch_body: {stdout}"
    );
}

/// The body of the test above; see it for why this runs in a child
/// process. Drives `publish_prepared_tree` to the late-hold outcome, which
/// leaves exactly the state `push_sync_branch` exists to publish: a local
/// commit on the sync branch that the bare remote does not have.
#[test]
#[ignore = "re-executed with GH_TOKEN by push_sync_branch_is_covered_by_a_child_process_with_a_token"]
fn push_sync_branch_body() {
    let f = Fixture::new();
    let report = read_report(&f.report_path);

    let held_pr = json!({
        "number": 1,
        "labels": [{"name": "sync-hold"}],
        "head": {
            "repo": {"full_name": REPO},
            "ref": SYNC_BRANCH,
            "sha": "a".repeat(40),
        },
        "base": {"ref": "main"},
    });
    let held_api = RecordedApi::new(vec![
        ("GET", REPOS_ENDPOINT, info_response()),
        ("GET", INSTALLATION_ENDPOINT, installation_response()),
        ("GET", PULLS_ENDPOINT, json!([])),
        ("GET", REPOS_ENDPOINT, info_response()),
        ("GET", INSTALLATION_ENDPOINT, installation_response()),
        ("GET", PULLS_ENDPOINT, json!([held_pr])),
    ]);
    let result = publish_prepared_tree(
        &f.definition,
        f.source.path(),
        &f.source_sha,
        f.target.path(),
        &report,
        &held_api,
    )
    .unwrap();
    assert!(matches!(result, Published::Held));

    let tip = f.target.head();
    assert_ne!(tip, f.base, "the local commit happens before the held read");
    let refname = format!("refs/heads/{SYNC_BRANCH}");
    // Negative control for the ls-remote instrument below: the same call
    // returns nothing here, before the push, and the tip afterwards.
    assert_eq!(
        ls_remote_sha(f.target.path(), &f.bare, &refname),
        None,
        "the held publication must not have pushed"
    );

    // `push_sync_branch` always pushes to `origin` (matching
    // `transport.mjs`), and this fixture's `origin` is the fake GitHub
    // HTTPS URL `assert_destination_checkout` requires. Point it back at
    // the bare remote so the push stays local; the credential helper is
    // inert for a path remote.
    f.target
        .git(&["remote", "set-url", "origin", f.bare.to_str().unwrap()]);

    push_sync_branch(&f.definition, f.target.path()).unwrap();
    assert_eq!(
        ls_remote_sha(f.target.path(), &f.bare, &refname).as_deref(),
        Some(tip.as_str()),
        "the sync branch should now be at the local tip on the bare remote"
    );

    // A second push of an unchanged tip is a no-op ("Everything
    // up-to-date", exit 0), not an error and not a ref change.
    push_sync_branch(&f.definition, f.target.path()).unwrap();
    assert_eq!(
        ls_remote_sha(f.target.path(), &f.bare, &refname).as_deref(),
        Some(tip.as_str()),
        "a second push must leave the remote ref where it was"
    );
    assert_eq!(
        f.target.head(),
        tip,
        "a second push must not move the local branch either"
    );
}
