//! Integration tests for `capobara run` (and, through it, `prepare`,
//! `preflight`, and the post-publication proof), driven end to end as a
//! child process against a local bare remote.
//!
//! Ports two tests from `scripts/projections/copybara-preflight.test.mjs`:
//! `stops_before_projection_work_when_the_generated_pr_is_held` and
//! `converged_projection_without_an_open_pr_is_unchanged`; adds
//! `run_dry_run_publishes_nothing_and_prints_the_plan`.
//!
//! Three seams keep this hermetic, all honored only in debug builds (see
//! `cli::transport::api_from_env` and `cli::run::destination_remote`):
//! - `CAPOBARA_TREE_ID_OVERRIDE` pins `tooldigest::embedded()` in the child,
//!   the same way `tests/project_cli.rs` and `tests/transport_git.rs` do.
//! - `CAPOBARA_RECORDED_API` points at a JSON recording replayed by
//!   `transport::github::RecordedApi`, so no GitHub call is ever made. The
//!   recording is an ordered sequence, so a run that calls an unexpected
//!   `(method, endpoint)` -- or calls at all once the recording is spent --
//!   panics in the child. Note the asymmetry: `RecordedApi` does not assert
//!   that the recording was fully consumed, so these tests pin the order
//!   and the identity of every call, and an upper bound on their number,
//!   but not a lower bound.
//! - `CAPOBARA_DESTINATION_REMOTE` replaces the `https://github.com/...`
//!   clone URL with the local bare repository, for both the destination
//!   clone and the post-publication proof clone. `origin`'s URL is then
//!   rewritten back to the GitHub URL, so every production check that
//!   inspects the remote (`assert_destination_checkout`,
//!   `cli::project::repo_identity`) sees exactly what it would in CI, while
//!   `remote.origin.pushurl` keeps the publication push local and the proof
//!   clone's `fetch` runs before the rewrite. That is what lets
//!   `publish_and_prove_pushes_the_sync_branch_and_matches_the_published_head`
//!   drive the real `push_sync_branch` and the real published-branch proof
//!   without a network. The bare repository is inspected directly (never
//!   through a remote name) whenever a test needs to prove what was or was
//!   not pushed.

// `cli::run`'s and `cli::transport`'s test seams, and the in-process
// `tooldigest` override they depend on, exist only in debug builds; compile
// this target to an empty binary under `cargo test --release` rather than
// failing.
#![cfg(debug_assertions)]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

use support::Repo;

const NAME: &str = "fixture";
const REPO: &str = "dx-corp/fixture";
const SYNC_BRANCH: &str = "sync/mono-projection";
const STAND_IN: &[u8] = b"// stand-in for the crate tree";

const REPOS_ENDPOINT: &str = "repos/dx-corp/fixture";
const INSTALLATION_ENDPOINT: &str = "installation/repositories?per_page=100";
const PULLS_ENDPOINT: &str =
    "repos/dx-corp/fixture/pulls?state=open&base=main&head=dx-corp%3Async%2Fmono-projection";

fn definition_json() -> Value {
    json!({
        "schemaVersion": 1, "name": NAME, "class": "source-tree", "mode": "copy-v1",
        "sourceRepository": "dx-corp/mono", "visibility": "public",
        "mappings": [
            {"source": "pkg", "destination": ".", "include": ["src/**", "README.md"], "exclude": []}
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

fn call(method: &str, endpoint: &str, response: Value) -> Value {
    json!({"method": method, "endpoint": endpoint, "response": response})
}

/// The three calls `read_publication_state` makes, with no open PR.
fn no_pr_state() -> Vec<Value> {
    vec![
        call("GET", REPOS_ENDPOINT, info_response()),
        call("GET", INSTALLATION_ENDPOINT, installation_response()),
        call("GET", PULLS_ENDPOINT, json!([])),
    ]
}

/// The three calls `read_publication_state` makes with one open,
/// sync-held PR.
fn held_pr_state() -> Vec<Value> {
    let pr = json!({
        "number": 7,
        "labels": [{"name": "sync-hold"}],
        "head": {"repo": {"full_name": REPO}, "ref": SYNC_BRANCH, "sha": "1".repeat(40)},
        "base": {"ref": "main"},
    });
    vec![
        call("GET", REPOS_ENDPOINT, info_response()),
        call("GET", INSTALLATION_ENDPOINT, installation_response()),
        call("GET", PULLS_ENDPOINT, json!([pr])),
    ]
}

fn write_recording(dir: &Path, calls: Vec<Value>) -> PathBuf {
    let path = dir.join("recorded-api.json");
    std::fs::write(&path, serde_json::to_vec(&Value::Array(calls)).unwrap()).unwrap();
    path
}

#[allow(
    clippy::disallowed_methods,
    reason = "integration test executes the capobara binary"
)]
fn capobara() -> Command {
    Command::new(env!("CARGO_BIN_EXE_capobara"))
}

#[allow(
    clippy::disallowed_methods,
    reason = "integration tests drive scratch git repositories outside the support::Repo helper"
)]
fn run_git(path: &Path, args: &[&str]) -> String {
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

#[allow(
    clippy::disallowed_methods,
    reason = "integration test inspects a scratch bare git repository's refs"
)]
fn ref_sha(path: &Path, refname: &str) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "--quiet", refname])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The whole rig: a Mono source checkout with a one-entry catalog, a bare
/// destination remote, and a seeded `main` on that remote.
struct Fixture {
    source: Repo,
    source_sha: String,
    tree_id: String,
    bare: PathBuf,
    work: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let source = Repo::init("https://github.com/dx-corp/mono.git");
        source.write(
            "config/projections/repositories.json",
            br#"{"schemaVersion":1,"sourceRepository":"dx-corp/mono","projections":["fixture"]}"#,
        );
        source.write(
            "config/projections/fixture.json",
            serde_json::to_string(&definition_json())
                .unwrap()
                .as_bytes(),
        );
        source.write("pkg/src/client.txt", b"public code\n");
        source.write("pkg/README.md", b"SDK\n");
        source.write("rust/tools/capobara/src/lib.rs", STAND_IN);
        let source_sha = source.commit("source");
        source.set_remote_main(&source_sha);
        let tree_id = source
            .git(&["rev-parse", "HEAD:rust/tools/capobara"])
            .trim()
            .to_owned();

        // `.keep()` deliberately leaks the bare remote for the test
        // process's lifetime, matching `support::Repo::into_path` and
        // `tests/transport_git.rs`'s own convention.
        let bare = tempfile::tempdir().unwrap().keep();
        run_git(&bare, &["init", "-q", "--bare", "-b", "main"]);

        let seed = Repo::init(bare.to_str().unwrap());
        seed.write("SECURITY.md", b"owned\n");
        seed.write(".github/workflows/ci.yml", b"ci\n");
        seed.commit("destination");
        seed.git(&["push", "-q", "origin", "main"]);

        Fixture {
            source,
            source_sha,
            tree_id,
            bare,
            work: tempfile::tempdir().unwrap(),
        }
    }

    /// Projects the fixture into the bare remote's `main`, so a later run
    /// finds an already-converged destination.
    fn converge_main(&self) {
        let seed = Repo::init(self.bare.to_str().unwrap());
        seed.git(&["fetch", "-q", "origin", "main"]);
        seed.git(&["reset", "-q", "--hard", "origin/main"]);
        seed.git(&[
            "remote",
            "set-url",
            "origin",
            &format!("https://github.com/{REPO}.git"),
        ]);
        let out = capobara()
            .env("CAPOBARA_TREE_ID_OVERRIDE", &self.tree_id)
            .args(["apply", "--definition"])
            .arg(self.source.path().join("config/projections/fixture.json"))
            .arg("--source")
            .arg(self.source.path())
            .arg("--source-sha")
            .arg(&self.source_sha)
            .arg("--target")
            .arg(seed.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "seed apply failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        seed.commit("projection");
        seed.git(&["remote", "set-url", "origin", self.bare.to_str().unwrap()]);
        seed.git(&["push", "-q", "origin", "main"]);
    }

    fn destination(&self) -> PathBuf {
        self.work.path().join("projection-target")
    }

    /// Each `run` clones into a fresh directory: `git clone` refuses a
    /// non-empty target, so a test with several phases cannot reuse one.
    fn destination_named(&self, name: &str) -> PathBuf {
        self.work.path().join(name)
    }

    fn sync_tip(&self) -> Option<String> {
        ref_sha(&self.bare, &format!("refs/heads/{SYNC_BRANCH}"))
    }

    /// `capobara run` with an explicit destination directory, an optional
    /// `GH_TOKEN`, and an optional `$GITHUB_STEP_SUMMARY`.
    fn run_phase(
        &self,
        recording: &Path,
        destination: &Path,
        token: Option<&str>,
        summary: Option<&Path>,
    ) -> Output {
        let mut command = capobara();
        command
            .current_dir(self.source.path())
            .env("CAPOBARA_TREE_ID_OVERRIDE", &self.tree_id)
            .env("CAPOBARA_RECORDED_API", recording)
            .env("CAPOBARA_DESTINATION_REMOTE", &self.bare)
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_STEP_SUMMARY")
            .args([
                "run",
                NAME,
                "--source-sha",
                &self.source_sha,
                "--destination",
            ])
            .arg(destination);
        if let Some(token) = token {
            command.env("GH_TOKEN", token);
        }
        if let Some(summary) = summary {
            command.env("GITHUB_STEP_SUMMARY", summary);
        }
        command.output().unwrap()
    }

    /// `capobara run fixture --source-sha <sha> --destination <dir> [...]`,
    /// from the source checkout, with every test seam wired up.
    fn run(&self, recording: &Path, extra: &[&str]) -> Output {
        let mut command = capobara();
        command
            .current_dir(self.source.path())
            .env("CAPOBARA_TREE_ID_OVERRIDE", &self.tree_id)
            .env("CAPOBARA_RECORDED_API", recording)
            .env("CAPOBARA_DESTINATION_REMOTE", &self.bare)
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_STEP_SUMMARY")
            .args([
                "run",
                NAME,
                "--source-sha",
                &self.source_sha,
                "--destination",
            ])
            .arg(self.destination())
            .args(extra);
        command.output().unwrap()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// An open, non-held PR whose head is `head_sha`.
fn open_pr_state(head_sha: &str) -> Vec<Value> {
    let pr = json!({
        "number": 7,
        "labels": [],
        "head": {"repo": {"full_name": REPO}, "ref": SYNC_BRANCH, "sha": head_sha},
        "base": {"ref": "main"},
        "html_url": "https://github.com/dx-corp/fixture/pull/7",
    });
    vec![
        call("GET", REPOS_ENDPOINT, info_response()),
        call("GET", INSTALLATION_ENDPOINT, installation_response()),
        call("GET", PULLS_ENDPOINT, json!([pr])),
    ]
}

/// The `POST repos/{repo}/pulls` that `create_or_update_pr` makes when no
/// PR is open yet.
fn create_pr_call() -> Value {
    call(
        "POST",
        "repos/dx-corp/fixture/pulls",
        json!({"number": 7, "html_url": "https://github.com/dx-corp/fixture/pull/7"}),
    )
}

fn detail(out: &Output) -> String {
    format!(
        "status={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `copybara-preflight.test.mjs`: "stops before projection work when the
/// generated PR is held". A sync-hold on the open destination PR stops the
/// run at `prepare`, before any projection work: the destination checkout
/// never receives a projected file, and the remote is untouched.
#[test]
fn stops_before_projection_work_when_the_generated_pr_is_held() {
    let f = Fixture::new();
    let recording = write_recording(f.work.path(), held_pr_state());
    let main_before = ref_sha(&f.bare, "refs/heads/main");

    let out = f.run(&recording, &[]);

    assert_eq!(out.status.code(), Some(3), "{}", detail(&out));
    assert_eq!(stdout(&out), "{\"held\":true}\n", "{}", detail(&out));
    assert!(
        !f.destination().join("src/client.txt").exists(),
        "projection work ran despite the hold"
    );
    assert!(
        !f.destination().join(".repository-projection.json").exists(),
        "a receipt was written despite the hold"
    );
    assert_eq!(
        ref_sha(&f.bare, "refs/heads/main"),
        main_before,
        "the remote's main moved"
    );
    assert_eq!(
        ref_sha(&f.bare, &format!("refs/heads/{SYNC_BRANCH}")),
        None,
        "a sync branch was pushed despite the hold"
    );
}

/// `copybara-preflight.test.mjs`: "converged projection without an open PR
/// is unchanged" -- both the pure disposition table and the end-to-end
/// consequence. A destination whose `main` already carries the projection,
/// with no open generated PR, preflights as `unchanged`, publishes nothing,
/// and still proves itself through the post-publication proof.
#[test]
fn converged_projection_without_an_open_pr_is_unchanged() {
    use capobara::preflight::publication_disposition;
    assert_eq!(publication_disposition(0, false, 0), "unchanged");
    assert_eq!(publication_disposition(0, true, 0), "publish");
    assert_eq!(publication_disposition(1, false, 0), "publish");
    assert_eq!(publication_disposition(0, false, 1), "publish");

    let f = Fixture::new();
    f.converge_main();
    let main_before = ref_sha(&f.bare, "refs/heads/main");

    // prepare (3) + preflight's two publication-state reads (6) + the
    // proof's open-PR listing (1). No publication call is recorded, so a
    // run that tried to publish would panic in the child.
    let mut calls = no_pr_state();
    calls.extend(no_pr_state());
    calls.extend(no_pr_state());
    calls.push(call("GET", PULLS_ENDPOINT, json!([])));
    let recording = write_recording(f.work.path(), calls);

    let summary = f.work.path().join("step-summary.md");
    let out = capobara()
        .current_dir(f.source.path())
        .env("CAPOBARA_TREE_ID_OVERRIDE", &f.tree_id)
        .env("CAPOBARA_RECORDED_API", &recording)
        .env("CAPOBARA_DESTINATION_REMOTE", &f.bare)
        .env("GITHUB_STEP_SUMMARY", &summary)
        .args(["run", NAME, "--source-sha", &f.source_sha, "--destination"])
        .arg(f.destination())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "{}", detail(&out));
    let text = stdout(&out);
    assert!(
        text.contains("\"held\":false,\"unchanged\":true,"),
        "{}",
        detail(&out)
    );
    assert!(
        text.contains("\"destinationFetch\":\"main\""),
        "a converged destination must fetch the default branch, not the sync branch: {}",
        detail(&out)
    );
    assert_eq!(
        ref_sha(&f.bare, "refs/heads/main"),
        main_before,
        "the remote's main moved"
    );
    assert_eq!(
        ref_sha(&f.bare, &format!("refs/heads/{SYNC_BRANCH}")),
        None,
        "a converged run pushed a sync branch"
    );

    let head = ref_sha(&f.bare, "refs/heads/main").unwrap();
    let receipt = std::fs::read_to_string(&summary).unwrap();
    assert!(
        receipt.ends_with(&format!(
            "### {NAME} Capobara receipt\n\n- Source: `{}`\n- Destination head: `{head}`\n- Converged without publication: `true`\n",
            f.source_sha
        )),
        "unexpected step summary tail: {receipt}"
    );
}

/// A run that would publish, stopped by `--dry-run` immediately after
/// preflight: the plan is printed, `{"dryRun":true}` marks the skip, and
/// the bare remote's refs are byte-identical to before the run.
#[test]
fn run_dry_run_publishes_nothing_and_prints_the_plan() {
    let f = Fixture::new();
    let main_before = ref_sha(&f.bare, "refs/heads/main");

    // prepare (3) + preflight's two publication-state reads (6). Nothing
    // else may be called.
    let mut calls = no_pr_state();
    calls.extend(no_pr_state());
    calls.extend(no_pr_state());
    let recording = write_recording(f.work.path(), calls);

    let out = f.run(&recording, &["--dry-run"]);

    assert_eq!(out.status.code(), Some(0), "{}", detail(&out));
    let text = stdout(&out);
    assert!(
        text.contains("\"held\":false,\"unchanged\":false,"),
        "a destination without the projection must preflight as publishable: {}",
        detail(&out)
    );
    assert!(
        text.contains("\"destinationFetch\":\"main\""),
        "{}",
        detail(&out)
    );
    assert!(text.ends_with("{\"dryRun\":true}\n"), "{}", detail(&out));

    // The projection really was produced locally -- otherwise "nothing was
    // pushed" would hold for a run that simply did nothing.
    assert!(
        f.destination().join("src/client.txt").exists(),
        "the projection was never applied, so this test proves nothing"
    );
    assert_eq!(
        run_git(&f.destination(), &["branch", "--show-current"]).trim(),
        SYNC_BRANCH
    );

    // Review fix round 1 (I3): assert the origin rewrite directly rather
    // than inferring it from `assert_destination_checkout` having passed.
    // `get-url` must read as GitHub (what every production identity check
    // sees) while `get-url --push` reads as the bare remote (what keeps a
    // publication push hermetic). An `insteadOf` seam cannot produce this
    // split -- `get-url` expands `insteadOf` -- which is why `pushurl` is
    // the mechanism.
    assert_eq!(
        run_git(&f.destination(), &["remote", "get-url", "origin"]).trim(),
        "https://github.com/dx-corp/fixture.git"
    );
    assert_eq!(
        run_git(&f.destination(), &["remote", "get-url", "--push", "origin"]).trim(),
        f.bare.to_str().unwrap()
    );

    assert_eq!(
        ref_sha(&f.bare, "refs/heads/main"),
        main_before,
        "the remote's main moved"
    );
    assert_eq!(
        ref_sha(&f.bare, &format!("refs/heads/{SYNC_BRANCH}")),
        None,
        "--dry-run pushed a sync branch"
    );
}

/// Review fix round 1 (I1): the publish -> push -> prove lane, end to end
/// and hermetically, in three phases against one bare remote.
///
/// Before the `remote.origin.pushurl` seam this lane had no coverage at
/// all: every earlier test stops before `push_sync_branch` and before the
/// proof's `fetch`, both of which addressed `origin` -- which
/// `clone_destination` had already rewritten to `https://github.com/...`.
#[test]
fn publish_and_prove_pushes_the_sync_branch_and_matches_the_published_head() {
    let f = Fixture::new();
    let main_tip = ref_sha(&f.bare, "refs/heads/main").unwrap();

    // Phase A -- no GH_TOKEN. Everything up to the push runs; the push
    // itself refuses. Proves the assertions in phase B are not vacuous:
    // the sync branch appears there because a push happened, not because
    // the fixture created it.
    let recording = write_recording(f.work.path(), {
        let mut calls = no_pr_state(); // prepare
        calls.extend(no_pr_state()); // preflight, first
        calls.extend(no_pr_state()); // preflight, final
        calls.extend(no_pr_state()); // publish, first
        calls.extend(no_pr_state()); // publish, final
        calls
    });
    let out = f.run_phase(&recording, &f.destination_named("phase-a"), None, None);
    assert_eq!(out.status.code(), Some(1), "{}", detail(&out));
    assert!(
        stderr(&out).contains("Missing publication token"),
        "{}",
        detail(&out)
    );
    assert_eq!(
        f.sync_tip(),
        None,
        "a sync branch was pushed without a token"
    );

    // Phase B -- with a token, but the proof is told about a PR whose head
    // is some other commit. The push must happen and the proof must reject
    // the mismatch with the runbook-grep string, verbatim.
    let recording = write_recording(f.work.path(), {
        let mut calls = no_pr_state();
        calls.extend(no_pr_state());
        calls.extend(no_pr_state());
        calls.extend(no_pr_state());
        calls.extend(no_pr_state());
        calls.push(create_pr_call());
        // The proof's `gh pr list` equivalent, answered with a stale head.
        calls.push(call(
            "GET",
            PULLS_ENDPOINT,
            json!([{"number": 7, "head": {"sha": "9".repeat(40)}}]),
        ));
        calls
    });
    let out = f.run_phase(
        &recording,
        &f.destination_named("phase-b"),
        Some("x-token"),
        None,
    );
    assert_eq!(out.status.code(), Some(1), "{}", detail(&out));
    assert_eq!(
        stderr(&out).lines().next_back(),
        Some("Copybara PR does not uniquely match the verified destination head"),
        "{}",
        detail(&out)
    );
    // Node's full publication object, in Node's key order
    // (transport.mjs:441-446). `tree` is `HEAD^{tree}` of the commit that
    // was actually pushed, read back out of the bare remote rather than
    // copied from stdout.
    let pushed_for_tree = ref_sha(&f.bare, &format!("refs/heads/{SYNC_BRANCH}"))
        .expect("push_sync_branch did not push");
    let pushed_tree = run_git(
        &f.bare,
        &["rev-parse", &format!("{pushed_for_tree}^{{tree}}")],
    )
    .trim()
    .to_owned();
    assert!(
        stdout(&out).contains(&format!(
            "{{\"held\":false,\"pullRequest\":\"https://github.com/dx-corp/fixture/pull/7\",\"engine\":\"rust-prepared-tree\",\"tree\":\"{pushed_tree}\"}}"
        )),
        "{}",
        detail(&out)
    );
    assert_eq!(
        capobara::transport::PUBLICATION_ENGINE,
        "rust-prepared-tree"
    );

    // push_sync_branch really ran, against the bare remote.
    let pushed = f.sync_tip().expect("push_sync_branch did not push");
    assert_ne!(pushed, main_tip, "the pushed head is just main");
    assert_eq!(
        ref_sha(&f.bare, "refs/heads/main").unwrap(),
        main_tip,
        "publication moved the destination's default branch"
    );

    // Phase C -- the destination now carries the published sync branch and
    // an open PR at exactly that head. The proof must pass, and the receipt
    // must name that head.
    let summary = f.work.path().join("phase-c-summary.md");
    let recording = write_recording(f.work.path(), {
        let mut calls = open_pr_state(&pushed); // prepare
        calls.extend(open_pr_state(&pushed)); // preflight, first
        calls.extend(open_pr_state(&pushed)); // preflight, final
        calls.extend(open_pr_state(&pushed)); // publish, first (converges)
        calls.push(call(
            "GET",
            PULLS_ENDPOINT,
            json!([{"number": 7, "head": {"sha": pushed}}]),
        ));
        calls
    });
    let out = f.run_phase(
        &recording,
        &f.destination_named("phase-c"),
        Some("x-token"),
        Some(&summary),
    );
    assert_eq!(out.status.code(), Some(0), "{}", detail(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("\"destinationFetch\":\"{SYNC_BRANCH}\"")),
        "an existing remote sync branch must be the fetch ref: {}",
        detail(&out)
    );
    assert!(
        text.contains("\"held\":false,\"unchanged\":false,"),
        "an open PR means the projection is still publishable: {}",
        detail(&out)
    );
    // Nothing new to commit, so publication converges on the existing PR.
    assert!(
        text.contains("{\"held\":false,\"unchanged\":true}"),
        "{}",
        detail(&out)
    );
    assert_eq!(f.sync_tip().as_deref(), Some(pushed.as_str()));

    let receipt = std::fs::read_to_string(&summary).unwrap();
    assert!(
        receipt.ends_with(&format!(
            "### {NAME} Capobara receipt\n\n- Source: `{}`\n- Destination head: `{pushed}`\n- Converged without publication: `false`\n",
            f.source_sha
        )),
        "unexpected step summary tail: {receipt}"
    );
    assert!(
        receipt.contains("Publication: {\"held\":false,\"unchanged\":true}\n"),
        "the publication result was not recorded in the step summary: {receipt}"
    );
}

/// Review fix round 1 (I2): `run` does not apply the workflow's
/// `validate.mjs` policies, and says so on every invocation rather than
/// only in a source comment.
#[test]
fn run_announces_that_it_applies_no_distribution_validation() {
    let f = Fixture::new();
    let recording = write_recording(f.work.path(), held_pr_state());
    let out = f.run(&recording, &[]);
    assert_eq!(
        stderr(&out).lines().next(),
        Some("capobara run: distribution validation is not applied by this command"),
        "{}",
        detail(&out)
    );

    // The same gap is named in `capobara run --help`, which is where an
    // operator swapping the workflow's steps for this command would look.
    let help = capobara().args(["run", "--help"]).output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout).into_owned();
    assert!(
        help.contains("performs no distribution validation"),
        "run --help does not disclose the validation gap: {help}"
    );
}

// ---------------------------------------------------------------------
// sdk-assembly-v1 through `run`
//
// Fix round 2 integration. `cli::project` carried a constant-`None` policy
// lookup and an `unreachable!()` assembler until Task 15's
// `ec116b6bc40d`; with that stand-in, `capobara run` could not project any
// of the three `sdk-assembly-v1` repositories at all -- it failed at the
// catalog load with "Unknown SDK assembly policy" before reaching a single
// git command. Every other test in this file uses a `copy-v1` fixture and
// would have passed throughout.
// ---------------------------------------------------------------------

const SDK_NAME: &str = "deixic-python";
const SDK_REPO: &str = "dx-corp/deixic-python";
const SDK_PULLS_ENDPOINT: &str =
    "repos/dx-corp/deixic-python/pulls?state=open&base=main&head=dx-corp%3Async%2Fmono-projection";

fn sdk_info_response() -> Value {
    json!({
        "full_name": SDK_REPO, "archived": false, "disabled": false,
        "default_branch": "main", "visibility": "public",
    })
}

/// `read_publication_state`'s three calls for the SDK destination, with no
/// open PR.
fn sdk_no_pr_state() -> Vec<Value> {
    vec![
        call("GET", "repos/dx-corp/deixic-python", sdk_info_response()),
        call(
            "GET",
            INSTALLATION_ENDPOINT,
            json!({"total_count": 1, "repositories": [{"full_name": SDK_REPO}]}),
        ),
        call("GET", SDK_PULLS_ENDPOINT, json!([])),
    ]
}

/// The committed `deixic-python` definition, as this crate's fixture copy
/// records it. Written into the source checkout verbatim, because
/// `cli::project::run` compares the on-disk definition byte-for-byte
/// against `git show {sha}:config/projections/{name}.json`.
fn sdk_definition_text() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/definitions/deixic-python.json"),
    )
    .unwrap()
}

/// Every file in `root` except `.git`, relative and sorted.
fn tracked_files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    found.sort();
    found
}

/// `capobara run` against a real `sdk-assembly-v1` definition and the real
/// reviewed `deixic-python` policy -- the smallest of the three -- driven
/// to a converged destination so the run completes through the
/// post-publication proof and exits 0.
///
/// The assembler is exercised four times over one run (the seeding
/// `apply`, `run`'s own `apply`, preflight's `verify`, and the candidate
/// rebuild inside `assert_candidate_matches_main_projection`), and the
/// destination is required to hold exactly the policy's registered outputs.
#[test]
fn run_projects_a_real_sdk_assembly_definition_end_to_end() {
    use capobara::modes::sdk_assembly::policies;

    let source = Repo::init("https://github.com/dx-corp/mono.git");
    support::populate_python_snapshot(source.path());
    source.write(
        "config/projections/repositories.json",
        br#"{"schemaVersion":1,"sourceRepository":"dx-corp/mono","projections":["deixic-python"]}"#,
    );
    source.write(
        "config/projections/deixic-python.json",
        sdk_definition_text().as_bytes(),
    );
    source.write("rust/tools/capobara/src/lib.rs", STAND_IN);
    let source_sha = source.commit("source");
    source.set_remote_main(&source_sha);
    let tree_id = source
        .git(&["rev-parse", "HEAD:rust/tools/capobara"])
        .trim()
        .to_owned();

    let bare = tempfile::tempdir().unwrap().keep();
    run_git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let seed = Repo::init(bare.to_str().unwrap());
    seed.write("SECURITY.md", b"owned\n");
    seed.write(".github/workflows/ci.yml", b"ci\n");
    let base = seed.commit("destination");
    seed.git(&["push", "-q", "origin", "main"]);

    // Converge `main` on the assembled projection, so the run under test
    // reaches the proof rather than a publication.
    seed.git(&["update-ref", "refs/remotes/origin/main", &base]);
    seed.git(&[
        "remote",
        "set-url",
        "origin",
        &format!("https://github.com/{SDK_REPO}.git"),
    ]);
    let seeded = capobara()
        .env("CAPOBARA_TREE_ID_OVERRIDE", &tree_id)
        .args(["apply", "--definition"])
        .arg(source.path().join("config/projections/deixic-python.json"))
        .arg("--source")
        .arg(source.path())
        .arg("--source-sha")
        .arg(&source_sha)
        .arg("--target")
        .arg(seed.path())
        .output()
        .unwrap();
    assert!(
        seeded.status.success(),
        "seed apply failed: {}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    seed.commit("projection");
    seed.git(&["remote", "set-url", "origin", bare.to_str().unwrap()]);
    seed.git(&["push", "-q", "origin", "main"]);
    let main_before = ref_sha(&bare, "refs/heads/main");

    let work = tempfile::tempdir().unwrap();
    let mut calls = sdk_no_pr_state(); // prepare
    calls.extend(sdk_no_pr_state()); // preflight, first
    calls.extend(sdk_no_pr_state()); // preflight, final
    calls.push(call("GET", SDK_PULLS_ENDPOINT, json!([]))); // the proof
    let recording = write_recording(work.path(), calls);
    let destination = work.path().join("projection-target");
    let summary = work.path().join("step-summary.md");

    let out = capobara()
        .current_dir(source.path())
        .env("CAPOBARA_TREE_ID_OVERRIDE", &tree_id)
        .env("CAPOBARA_RECORDED_API", &recording)
        .env("CAPOBARA_DESTINATION_REMOTE", &bare)
        .env("GITHUB_STEP_SUMMARY", &summary)
        .env_remove("GH_TOKEN")
        .args([
            "run",
            SDK_NAME,
            "--source-sha",
            &source_sha,
            "--destination",
        ])
        .arg(&destination)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "{}", detail(&out));
    assert!(
        stdout(&out).contains("\"held\":false,\"unchanged\":true,"),
        "a converged SDK destination must preflight as unchanged: {}",
        detail(&out)
    );

    // The assembled destination holds exactly the reviewed policy's
    // registered outputs, plus the receipt and the destination-owned files
    // it started with -- nothing extra, nothing missing. With the old
    // `unreachable!()` assembler this list would have been unreachable
    // code; with the constant-`None` lookup the run would not have started.
    let policy = policies::policy(SDK_NAME).unwrap();
    let mut expected: Vec<String> = policy.output_include.clone();
    expected.push(".repository-projection.json".to_owned());
    expected.push(".github/workflows/ci.yml".to_owned());
    expected.push("SECURITY.md".to_owned());
    expected.sort();
    assert_eq!(tracked_files(&destination), expected);

    // The destination-identity transform ran on the assembled manifest.
    let pyproject = std::fs::read_to_string(destination.join("pyproject.toml")).unwrap();
    assert!(!pyproject.contains("dx-corp/mono"), "{pyproject}");

    assert_eq!(
        ref_sha(&bare, "refs/heads/main"),
        main_before,
        "a converged run moved the destination's default branch"
    );
    assert_eq!(
        ref_sha(&bare, &format!("refs/heads/{SYNC_BRANCH}")),
        None,
        "a converged run pushed a sync branch"
    );

    let head = main_before.unwrap();
    let receipt = std::fs::read_to_string(&summary).unwrap();
    assert!(
        receipt.ends_with(&format!(
            "### {SDK_NAME} Capobara receipt\n\n- Source: `{source_sha}`\n- Destination head: `{head}`\n- Converged without publication: `true`\n"
        )),
        "unexpected step summary tail: {receipt}"
    );
}
