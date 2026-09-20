//! Destination git transport: prepares, validates, and publishes a
//! projection against a destination checkout. Ports `assertDestinationCheckout`,
//! `prepareDestination`, `assertCandidateMatchesMainProjection`, and
//! `publishPreparedTree` from `scripts/projections/transport.mjs`.
//!
//! Every check in this module enforces a publication contract (untrusted git
//! or GitHub state), matching the sibling `transport::github` module's use
//! of `contract()` (exit 1) rather than `invalid()` (exit 2) throughout.
//!
//! All git access goes through `crate::git`'s reviewed process boundary; the
//! `plan`/`apply`/`verify` projection subcommands run in-process via
//! `crate::cli::project::run`, never by spawning the `capobara` binary.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::cli::project::{ProjectArgs, ProjectCommand, run as project_run};
use crate::definition::{Definition, definition_digest};
use crate::git::{self, is_sha};
use crate::report::Report;
use crate::transport::github::{
    GitHubApi, create_or_update_pr, publication_body, read_publication_state,
};
use crate::tree;
use crate::{Error, Result, contract};

/// `pub(crate)` so `crate::preflight` can reuse the same scratch-clone
/// path handling rather than duplicating it.
pub(crate) fn require_utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::Invalid(format!("Non-UTF-8 path: {}", path.display())))
}

/// `pub(crate)` so `crate::preflight` and `crate::cli::run` read a report
/// file exactly the way this module does, including the error message.
pub(crate) fn read_report(path: &Path) -> Result<Report> {
    let bytes = std::fs::read(path).map_err(Error::Io)?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Invalid(format!("Invalid report: {e}")))
}

/// Ports `assertDestinationCheckout`: `target`'s `origin` remote must be the
/// GitHub HTTPS URL for `definition.destination.repository`.
pub fn assert_destination_checkout(definition: &Definition, target: &Path) -> Result<()> {
    let remote = git::git(target, &["remote", "get-url", "origin"])?;
    contract(
        remote.trim()
            == format!(
                "https://github.com/{}.git",
                definition.destination.repository
            ),
        "Unexpected destination checkout remote",
    )
}

#[derive(Debug)]
pub struct Prepared {
    pub held: bool,
}

/// Ports `prepareDestination`. `definition_text` is the destination
/// definition's exact committed text (a `LoadedDefinition::text`, or the
/// on-disk file's contents), needed to compute a `definitionDigest`
/// comparable to the one `build_projection` wrote into any existing
/// receipt; this is a deliberate addition beyond the brief's abbreviated
/// `(definition, target, source_sha, api)` signature, documented in the
/// task-12 report. `serde_json::to_value(definition)` round-tripping (as
/// `validate_definition` does) is not a substitute here: it re-serializes
/// in the `Definition` struct's declared field order rather than the
/// original file's key order, and would not reliably match the digest
/// already embedded in a stored receipt.
///
/// If a receipt file exists at the destination's provenance path, it must
/// parse as JSON (`Error::Invalid("Malformed stored provenance")` if not,
/// matching Node's `JSON.parse` throwing on malformed input). Once parsed,
/// its `sourceSha`/`definitionDigest` fields are read loosely -- as raw
/// `serde_json::Value` lookups, not deserialized into `Provenance` -- with
/// no key-set validation, matching Node's `previous?.sourceSha`/
/// `previous?.definitionDigest` optional-chaining exactly: a missing or
/// wrong-typed field just fails the comparison and triggers a merge, the
/// same as no receipt at all.
pub fn prepare_destination(
    definition: &Definition,
    definition_text: &str,
    target: &Path,
    source_sha: &str,
    api: &dyn GitHubApi,
) -> Result<Prepared> {
    contract(is_sha(source_sha), "Missing immutable source revision")?;
    assert_destination_checkout(definition, target)?;
    let status = git::git(target, &["status", "--porcelain", "--untracked-files=all"])?;
    contract(
        status.trim().is_empty(),
        "Prepare requires a clean destination checkout",
    )?;

    let state = read_publication_state(definition, api)?;
    if state.held {
        return Ok(Prepared { held: true });
    }

    let d = &definition.destination;
    git::git(target, &["config", "user.name", "github-actions[bot]"])?;
    git::git(
        target,
        &[
            "config",
            "user.email",
            "github-actions[bot]@users.noreply.github.com",
        ],
    )?;

    let refs = git::git(
        target,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/",
        ],
    )?;
    let sync_ref = format!("refs/remotes/origin/{}", d.sync_branch);
    let has_sync_branch = refs.trim().split('\n').any(|line| line == sync_ref);

    if has_sync_branch {
        git::git(
            target,
            &["switch", "--track", &format!("origin/{}", d.sync_branch)],
        )?;
        if let Some(pr) = &state.pr {
            let head = git::git(target, &["rev-parse", "HEAD"])?;
            contract(
                head.trim() == pr.head_sha,
                "Destination branch advanced; retry from a fresh clone",
            )?;
        }
        let receipt_path = target.join(&definition.provenance);
        let previous: Option<Value> = if receipt_path.exists() {
            let bytes = std::fs::read(&receipt_path).map_err(Error::Io)?;
            Some(
                serde_json::from_slice(&bytes)
                    .map_err(|_| Error::Invalid("Malformed stored provenance".into()))?,
            )
        } else {
            None
        };
        let current_digest = definition_digest(definition_text)?;
        // Integrate destination-owned changes only while producing a real new
        // projection. An unrelated main advance never refreshes an existing PR.
        // Node reads `previous?.sourceSha`/`previous?.definitionDigest` loosely
        // here, with no key-set validation (unlike the stored-receipt check in
        // `cli::project::run`): a missing or wrong-typed field just fails the
        // comparison and triggers a merge, the same as an absent receipt. A
        // receipt that isn't even parseable JSON is a harder failure
        // (`Error::Invalid`), matching Node's `JSON.parse`, which throws
        // rather than yielding something `?.`-safe to read fields from.
        let needs_merge = previous.as_ref().is_none_or(|previous| {
            previous.get("sourceSha").and_then(Value::as_str) != Some(source_sha)
                || previous.get("definitionDigest").and_then(Value::as_str)
                    != Some(current_digest.as_str())
        });
        if needs_merge {
            git::git(
                target,
                &["merge", "--no-edit", &format!("origin/{}", d.branch)],
            )?;
        }
    } else {
        contract(state.pr.is_none(), "Open PR branch is absent from clone")?;
        git::git(
            target,
            &[
                "switch",
                "-c",
                &d.sync_branch,
                &format!("origin/{}", d.branch),
            ],
        )?;
    }

    Ok(Prepared { held: false })
}

#[derive(Debug)]
pub struct Candidate {
    pub main_sha: String,
    pub file_count: usize,
    pub main_changed_count: usize,
}

/// Ports `assertCandidateMatchesMainProjection`. Builds the destination's
/// declared main branch plus a freshly verified projection into a scratch
/// clone (`expected`), then compares every file except the provenance path
/// against `target`'s actual on-disk state, by mode and bytes.
pub fn assert_candidate_matches_main_projection(
    definition: &Definition,
    source: &Path,
    source_sha: &str,
    target: &Path,
) -> Result<Candidate> {
    assert_destination_checkout(definition, target)?;
    let target_abs = target.canonicalize().map_err(Error::Io)?;
    let scratch = tempfile::Builder::new()
        .prefix("projection-candidate-")
        .tempdir()
        .map_err(Error::Io)?;
    let expected = scratch.path().join("expected");
    git::git(
        scratch.path(),
        &[
            "clone",
            "--quiet",
            "--no-hardlinks",
            "--no-checkout",
            require_utf8(&target_abs)?,
            require_utf8(&expected)?,
        ],
    )?;

    let main_sha = git::git(
        target,
        &[
            "rev-parse",
            &format!("origin/{}^{{commit}}", definition.destination.branch),
        ],
    )?
    .trim()
    .to_string();
    git::git(&expected, &["checkout", "--detach", &main_sha])?;
    git::git(
        &expected,
        &[
            "remote",
            "set-url",
            "origin",
            &format!(
                "https://github.com/{}.git",
                definition.destination.repository
            ),
        ],
    )?;

    let expected_report_path = scratch.path().join("expected-report.json");
    project_run(ProjectArgs {
        command: ProjectCommand::Apply,
        definition: source.join(format!("config/projections/{}.json", definition.name)),
        source: source.to_path_buf(),
        source_sha: source_sha.to_string(),
        target: expected.clone(),
        report: Some(expected_report_path.clone()),
        status_output: None,
        markdown_output: None,
        draft: false,
    })?;
    let expected_report = read_report(&expected_report_path)?;

    // Project verification already authenticates the candidate provenance.
    // Its priorProjectedBase records branch history and can legitimately
    // differ after a squash merge, so compare every other file and
    // executable bit.
    let excluded = definition.provenance.as_str();
    let actual_paths = tree::files_under(target, &|path: &str| path == excluded)?;
    let expected_paths = tree::files_under(&expected, &|path: &str| path == excluded)?;
    let actual_set: BTreeSet<&str> = actual_paths.iter().map(String::as_str).collect();
    let expected_set: BTreeSet<&str> = expected_paths.iter().map(String::as_str).collect();

    let mut all_paths: Vec<String> = actual_paths
        .iter()
        .cloned()
        .chain(expected_paths.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    tree::sort_js(&mut all_paths);

    let mut differences = Vec::new();
    for path in &all_paths {
        if !actual_set.contains(path.as_str()) || !expected_set.contains(path.as_str()) {
            differences.push(path.clone());
            continue;
        }
        let actual_entry = tree::read_entry(target, path)?;
        let expected_entry = tree::read_entry(&expected, path)?;
        if actual_entry.mode != expected_entry.mode
            || actual_entry.content != expected_entry.content
        {
            differences.push(path.clone());
        }
    }
    if !differences.is_empty() {
        let shown = differences
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if differences.len() > 20 {
            format!(" (+{} more)", differences.len() - 20)
        } else {
            String::new()
        };
        return Err(Error::Contract(format!(
            "Candidate tree differs from destination main plus the verified projection: {shown}{suffix}"
        )));
    }

    Ok(Candidate {
        main_sha,
        file_count: actual_paths.len(),
        main_changed_count: expected_report.copied_count + expected_report.deleted_count,
    })
}

/// The `engine` field of Node's publication result: `engine: nativeBinary
/// ? "rust-prepared-tree" : "git-index"`, in `publishPreparedTree`'s
/// returned object literal (`scripts/projections/transport.mjs`). Capobara
/// *is* that native binary -- Node's `nativeBinary` argument is the path to
/// this very tool, passed only by its `publish-native` command, and used
/// only to call `buildNativeTree`/`assertNativeTreeMatchesIndex` -- so
/// every publication this crate makes reports `rust-prepared-tree`.
/// `git-index` names Node's own pure-git staging path and is unreachable
/// from here.
pub const PUBLICATION_ENGINE: &str = "rust-prepared-tree";

/// What `publishPreparedTree` resolved to: one variant per object Node
/// returns. Node's CLI prints `JSON.stringify(result)`, so each variant's
/// documented object is exactly what a `capobara publish` has to print:
///
/// - `Held` -- `{"held":true}` (both the early and the late held read
///   return this same one-key object).
/// - `Unchanged` -- `{"held":false,"unchanged":true}`.
/// - `PullRequest` -- `{"held":false,"pullRequest":<url>,"engine":<engine>,
///   "tree":<tree>}`, in that key order.
///
/// `held` and `unchanged` are per-variant constants in Node, so they are
/// carried by the variant itself rather than by a field; `url`, `engine`,
/// and `tree` are the only values Node computes.
#[derive(Debug)]
pub enum Published {
    Held,
    Unchanged,
    PullRequest {
        /// Node's `pullRequest`: the created or updated PR's `html_url`.
        url: String,
        /// Always `PUBLICATION_ENGINE`; see its documentation.
        engine: String,
        /// Node's `tree`: `git rev-parse HEAD^{tree}` in `target`, trimmed.
        tree: String,
    },
}

/// The union of `git diff HEAD --name-only -z` and
/// `git ls-files --others --exclude-standard -z`: every path with an
/// uncommitted change (tracked or untracked) in `target`'s working tree.
pub fn changed_paths(target: &Path) -> Result<BTreeSet<String>> {
    let diff = git::git(target, &["diff", "HEAD", "--name-only", "-z"])?;
    let untracked = git::git(
        target,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    Ok(diff
        .split('\0')
        .chain(untracked.split('\0'))
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

/// Ports the credential-scoped push inside `publishPreparedTree`, factored
/// into its own function. `GH_TOKEN` is read only to confirm it is present
/// (`RestApi::from_env`'s exact message); the pushed `git` child process
/// inherits it from this process's own environment for its credential
/// helper, and it is never written to a URL, an argv, or `.git/config`.
pub fn push_sync_branch(definition: &Definition, target: &Path) -> Result<()> {
    std::env::var("GH_TOKEN").map_err(|_| Error::Invalid("Missing publication token".into()))?;
    const CREDENTIAL_HELPER: &str = "!f() { if [ \"$1\" = get ]; then printf \"username=x-access-token\\npassword=%s\\n\" \"$GH_TOKEN\"; fi; }; f";
    git::git(
        target,
        &[
            "-c",
            "credential.helper=",
            "-c",
            &format!("credential.helper={CREDENTIAL_HELPER}"),
            "push",
            "origin",
            &format!("HEAD:refs/heads/{}", definition.destination.sync_branch),
        ],
    )?;
    Ok(())
}

/// The message every in-process `verify` failure reports, built from the
/// argv that `verify` was actually given. Node's `execFileSync` throws
/// `Command failed: ${[file, ...args].join(" ")}` (plus the child's stderr
/// when non-empty, which for this failure is empty); see
/// `publish_prepared_tree`'s doc comment for the full analysis of which
/// tokens Rust can and cannot reproduce, and why `capobara` stands in for
/// Node's two leading absolute paths.
///
/// One builder, one condition, one message. It is shared by every site
/// that runs `cli::project::run` with `ProjectCommand::Verify` and has to
/// fail closed on a non-zero status: `publish_prepared_tree` below,
/// `preflight::preflight_publication`, and the two transcribed workflow
/// steps in `cli::run`. `report` is `None` at the sites whose invocation
/// passes no `--report` (the workflow's `verify` steps, yml:161-164 and
/// yml:288-291), so the message always names the real argv.
pub(crate) fn verify_command_failed(
    definition: &Path,
    source: &Path,
    source_sha: &str,
    target: &Path,
    report: Option<&Path>,
) -> String {
    let report = match report {
        Some(path) => format!(" --report {}", path.display()),
        None => String::new(),
    };
    format!(
        "Command failed: capobara verify --definition {} --source {} --source-sha {source_sha} --target {}{report}",
        definition.display(),
        source.display(),
        target.display()
    )
}

/// Ports `publishPreparedTree`.
///
/// Node runs the projector's `verify` step through `execFileSync`, which
/// throws when the child exits non-zero, so a `verify` that reports drift
/// aborts publication where it stands: before the baseline clone and plan,
/// before any staging or commit, and before any remote write. The
/// in-process `cli::project::run` reports that same drift as `Ok(1)`
/// (exit 1 for `Check`/`Verify` when `copied_count + deleted_count > 0`),
/// which `?` alone would discard, so the status is checked explicitly
/// below.
///
/// **Residual difference from Node's thrown message.** Node's
/// `execFileSync` failure message is
/// `Command failed: ${[file, ...args].join(" ")}`, with `\n${stderr}`
/// appended only when the child wrote to stderr. For this exact failure
/// the child writes its `N changed, M deleted` line to *stdout* and sets
/// `exitCode = 1` (`scripts/projections/project.mjs`), leaving stderr
/// empty, so Node's message is the joined argv and nothing else -- and
/// Rust appends nothing either. Of that argv, Rust reproduces every token
/// from `verify` onward byte-for-byte (the same five flags with the same
/// five values). It cannot reproduce the two leading tokens: Node's are
/// the absolute `process.execPath` and the absolute path to
/// `scripts/projections/project.mjs`, and this crate runs `verify`
/// in-process with neither a node binary nor a script path in existence.
/// The single token `capobara` stands in for both. Node's CLI maps every
/// thrown error to exit 1, which is what `Error::Contract` does here.
///
/// The sibling `plan` call below is left unchecked deliberately:
/// `cli::project::run` returns a non-zero status only for `Check` and
/// `Verify`, so `Plan` provably always returns `Ok(0)` and any real
/// failure of it arrives as an `Err` that `?` already propagates.
pub fn publish_prepared_tree(
    definition: &Definition,
    source: &Path,
    source_sha: &str,
    target: &Path,
    report: &Report,
    api: &dyn GitHubApi,
) -> Result<Published> {
    assert_destination_checkout(definition, target)?;
    let current_branch = git::git(target, &["branch", "--show-current"])?;
    contract(
        current_branch.trim() == definition.destination.sync_branch,
        "Publication requires the declared generated PR branch",
    )?;

    let first = read_publication_state(definition, api)?;
    if first.held {
        return Ok(Published::Held);
    }

    // Recompute the full tree and provenance immediately before publication.
    let scratch = tempfile::Builder::new()
        .prefix("projection-publish-")
        .tempdir()
        .map_err(Error::Io)?;

    let definition_path = source.join(format!("config/projections/{}.json", definition.name));
    let verified_path = scratch.path().join("verified.json");
    let verify_status = project_run(ProjectArgs {
        command: ProjectCommand::Verify,
        definition: definition_path.clone(),
        source: source.to_path_buf(),
        source_sha: source_sha.to_string(),
        target: target.to_path_buf(),
        report: Some(verified_path.clone()),
        status_output: None,
        markdown_output: None,
        draft: false,
    })?;
    // Fail closed on a drifting `verify`: `?` above propagates only an
    // `Err`, while drift is `Ok(1)`. See this function's doc comment for
    // the message Node throws here and what of it Rust can reproduce.
    contract(
        verify_status == 0,
        verify_command_failed(
            &definition_path,
            source,
            source_sha,
            target,
            Some(&verified_path),
        ),
    )?;
    let verified = read_report(&verified_path)?;
    contract(
        report.provenance == verified.provenance && verified.provenance.publication_eligible,
        "Prepared report does not match verified projection",
    )?;

    // The submitted changed/deleted lists are data, not staging authority.
    // Regenerate the plan against the immutable destination base before
    // trusting them, so a modified report cannot smuggle an unrelated file
    // into a commit.
    let target_abs = target.canonicalize().map_err(Error::Io)?;
    let baseline = scratch.path().join("baseline");
    git::git(
        scratch.path(),
        &[
            "clone",
            "--quiet",
            "--no-hardlinks",
            "--no-checkout",
            require_utf8(&target_abs)?,
            require_utf8(&baseline)?,
        ],
    )?;
    let target_head = git::git(target, &["rev-parse", "HEAD"])?.trim().to_string();
    git::git(&baseline, &["checkout", "--detach", &target_head])?;
    git::git(
        &baseline,
        &[
            "remote",
            "set-url",
            "origin",
            &format!(
                "https://github.com/{}.git",
                definition.destination.repository
            ),
        ],
    )?;

    let planned_path = scratch.path().join("planned.json");
    project_run(ProjectArgs {
        command: ProjectCommand::Plan,
        definition: definition_path.clone(),
        source: source.to_path_buf(),
        source_sha: source_sha.to_string(),
        target: baseline.clone(),
        report: Some(planned_path.clone()),
        status_output: None,
        markdown_output: None,
        draft: false,
    })?;
    let planned = read_report(&planned_path)?;
    contract(
        planned.copied_paths == report.copied_paths,
        "Untrusted publication report: copiedPaths",
    )?;
    contract(
        planned.deleted_paths == report.deleted_paths,
        "Untrusted publication report: deletedPaths",
    )?;
    contract(
        planned.copied_count == report.copied_count,
        "Untrusted publication report: copiedCount",
    )?;
    contract(
        planned.deleted_count == report.deleted_count,
        "Untrusted publication report: deletedCount",
    )?;
    contract(
        planned.provenance == report.provenance,
        "Untrusted publication report: provenance",
    )?;

    let candidate =
        assert_candidate_matches_main_projection(definition, source, source_sha, target)?;

    let paths = git::git(target, &["status", "--porcelain", "--untracked-files=all"])?;
    let paths = paths.trim();
    if paths.is_empty() && (first.pr.is_some() || candidate.main_changed_count == 0) {
        return Ok(Published::Unchanged);
    }
    if !paths.is_empty() {
        // Stage only the declared diff. Ignored generated sources may
        // legitimately belong to the projection, while build outputs and
        // credentials never do.
        let changed: Vec<String> = report
            .copied_paths
            .iter()
            .chain(report.deleted_paths.iter())
            .cloned()
            .collect();
        contract(!changed.is_empty(), "Unexplained destination modifications")?;
        let changed_set: BTreeSet<&str> = changed.iter().map(String::as_str).collect();

        let actual = changed_paths(target)?;
        contract(
            actual
                .iter()
                .all(|path| changed_set.contains(path.as_str())),
            "Unplanned destination modifications",
        )?;

        let mut add_args = vec!["add", "--force", "--"];
        add_args.extend(changed.iter().map(String::as_str));
        git::git(target, &add_args)?;

        let staged = git::git(target, &["diff", "--cached", "--name-only", "-z"])?;
        let staged: BTreeSet<&str> = staged.split('\0').filter(|s| !s.is_empty()).collect();
        contract(
            staged.iter().all(|path| changed_set.contains(*path)),
            "Unplanned paths staged for publication",
        )?;

        let commit_message = format!(
            "chore: project {} from Mono {}",
            definition.name,
            &source_sha[..12]
        );
        git::git(
            target,
            &[
                "-c",
                "user.name=dx-corp projector",
                "-c",
                "user.email=noreply@dx-corp.net",
                "commit",
                "-m",
                &commit_message,
            ],
        )?;
    }

    let final_state = read_publication_state(definition, api)?;
    if final_state.held {
        return Ok(Published::Held);
    }

    push_sync_branch(definition, target)?;

    let body = publication_body(definition, report);
    let url = create_or_update_pr(definition, api, final_state.pr.as_ref(), &body)?;
    // Node evaluates `tree` last, inside the returned object literal, after
    // the PR call has already resolved.
    let tree = git::git(target, &["rev-parse", "HEAD^{tree}"])?
        .trim()
        .to_string();
    Ok(Published::PullRequest {
        url,
        engine: PUBLICATION_ENGINE.to_string(),
        tree,
    })
}
