//! `capobara run`: the whole publication for one projection in a single
//! process, plus the post-publication proof.
//!
//! This is a transcription of the `sync` job's steps in
//! `.github/workflows/repository-projections.yml` -- "Clone destination and
//! inspect sync-hold", "Prepare and verify the standalone projection",
//! "Recheck and render the Copybara transport", the publication step, and
//! "Prove the published or converged destination" -- with the Copybara
//! runtime replaced by this crate's own `transport::git` publication.
//!
//! Two steps of that job have no analogue here: `render-copybara.mjs`
//! exists only to feed the Copybara runtime this command replaces, and
//! `scripts/projections/validate.mjs` (yml:160, yml:287) has not been
//! ported to this crate.
//!
//! The `validate.mjs` gap is deliberately *visible* rather than silent:
//! `Command::Run`'s `long_about` names it and `run` prints
//! `VALIDATION_NOTICE` to stderr on every invocation. It is not fail-closed
//! because the phase-1 workflow keeps Node's two validate steps around
//! `capobara run` until Task 19 ports them; see the `TODO(task-19)` markers
//! at the two sites where the calls belong.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::Value;

use crate::cli::project::{ProjectArgs, ProjectCommand, run as project_run};
use crate::cli::transport::{api_from_env, published_json, resolve_definition, source_root};
use crate::definition::Definition;
use crate::git::{self, is_ancestor};
use crate::preflight::{Preflight, preflight_publication};
use crate::transport::git::{read_report, verify_command_failed};
use crate::transport::github::{GitHubApi, open_sync_pr_endpoint};
use crate::transport::{Published, prepare_destination, publish_prepared_tree};
use crate::{Error, Result, contract};

#[derive(Args, Debug)]
pub struct RunArgs {
    /// The catalog projection name.
    pub name: String,
    /// The immutable Mono revision being projected.
    #[arg(long = "source-sha")]
    pub source_sha: String,
    /// Where to clone the destination. Defaults to a fresh directory under
    /// `RUNNER_TEMP` (the workflow's `$RUNNER_TEMP/projection-target`) or,
    /// failing that, the system temporary directory.
    #[arg(long)]
    pub destination: Option<PathBuf>,
    /// Run everything up to and including preflight, then stop without
    /// publishing or proving.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// The workflow's preflight-hold line (yml:179). Also used for the
/// `prepare` hold, where the workflow writes nothing but the brief requires
/// a summary.
const HOLD_SUMMARY: &str = "sync-hold appeared; remote publication skipped.\n";

/// The workflow's publication-hold line (yml:254). Distinct from
/// `HOLD_SUMMARY` so a runbook grep can tell the two stages apart; this
/// crate replaces the native-tree publication path, so it inherits that
/// path's wording.
const PUBLISH_HOLD_SUMMARY: &str = "sync-hold appeared; native publication skipped.\n";

/// Printed to stderr on every `run`. The workflow still applies the
/// projection validation policies itself (yml:160, yml:287); this command
/// does not, until Task 19 ports `scripts/projections/validate.mjs`.
pub const VALIDATION_NOTICE: &str =
    "capobara run: distribution validation is not applied by this command";

/// Appends to `$GITHUB_STEP_SUMMARY` when the workflow set it, and does
/// nothing otherwise -- the same conditional the workflow's `>>`
/// redirections have by virtue of only running inside Actions.
fn append_step_summary(text: &str) -> Result<()> {
    let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") else {
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(Error::Io)?;
    file.write_all(text.as_bytes()).map_err(Error::Io)
}

fn step_summary_path() -> Option<PathBuf> {
    std::env::var_os("GITHUB_STEP_SUMMARY").map(PathBuf::from)
}

/// The URL the destination is cloned from: always
/// `https://github.com/{repository}.git`, the literal URL the workflow
/// clones, unless the debug-only test seam below replaces it.
///
/// `CAPOBARA_DESTINATION_REMOTE` is gated on
/// `#[cfg(all(debug_assertions, feature = "recorded-api"))]` -- exactly
/// like its sibling `CAPOBARA_RECORDED_API` (`cli::transport`), and
/// deliberately narrower than `CAPOBARA_TREE_ID_OVERRIDE`, which is
/// `debug_assertions` alone. The difference matters: the tree-id override
/// only relaxes a self-check, whereas this one chooses the tree that is
/// subsequently committed and pushed to the *real* GitHub destination, so
/// an ordinary debug build with a live `GH_TOKEN` must not honor it. Its
/// only consumer, `tests/run_cli.rs`, already carries
/// `required-features = ["recorded-api"]`, so nothing is lost.
#[cfg(all(debug_assertions, feature = "recorded-api"))]
fn destination_remote(github_url: &str) -> String {
    match std::env::var_os("CAPOBARA_DESTINATION_REMOTE") {
        Some(remote) => remote.to_string_lossy().into_owned(),
        None => github_url.to_string(),
    }
}

/// The production twin of the seam above: the destination is always cloned
/// from its GitHub URL, and the environment variable does not exist.
#[cfg(not(all(debug_assertions, feature = "recorded-api")))]
fn destination_remote(github_url: &str) -> String {
    github_url.to_string()
}

/// `git clone [--quiet] [--no-checkout] <remote> <into>`, through
/// `crate::git`'s reviewed process boundary with `into`'s parent as the
/// working directory, exactly as the scratch clones in `transport::git` do.
///
/// `quiet` mirrors the workflow: its destination clone (yml:139) is not
/// quiet, its proof clone (yml:275) is.
fn clone_from(remote: &str, into: &Path, quiet: bool, no_checkout: bool) -> Result<()> {
    let parent = into
        .parent()
        .ok_or_else(|| Error::Invalid(format!("Invalid destination path: {}", into.display())))?;
    std::fs::create_dir_all(parent).map_err(Error::Io)?;
    let into = into
        .to_str()
        .ok_or_else(|| Error::Invalid(format!("Non-UTF-8 path: {}", into.display())))?;
    let mut args = vec!["clone"];
    if quiet {
        args.push("--quiet");
    }
    if no_checkout {
        args.push("--no-checkout");
    }
    args.push(remote);
    args.push(into);
    git::git(parent, &args)?;
    Ok(())
}

/// Makes a clone taken from the test seam's local remote indistinguishable,
/// to every production identity check, from one taken from GitHub:
/// `origin`'s URL becomes the real repository URL, and
/// `remote.origin.pushurl` points back at the local remote so a publication
/// push stays hermetic.
///
/// In production `remote == github_url`, so this is a no-op: no `set-url`
/// runs and no `pushurl` is ever configured.
///
/// `pushurl` -- not `url.<remote>.insteadOf` -- is what makes this work.
/// `git remote get-url origin`, which both `assert_destination_checkout`
/// (porting transport.mjs:88) and `cli::project::repo_identity` read,
/// *expands* `insteadOf`, so an `insteadOf` seam would make those two
/// checks see the local path and fail. Plain `get-url` does not expand
/// `pushurl` (only `get-url --push` does). Both directions were verified
/// against git on the host before this was written; see the task-13
/// fix-round report.
///
/// The proof clone needs one more thing this cannot provide -- its `fetch`
/// also has to stay local -- so `prove_publication` calls this *after* the
/// fetch rather than immediately after the clone.
fn adopt_destination_origin(into: &Path, remote: &str, github_url: &str) -> Result<()> {
    if remote == github_url {
        return Ok(());
    }
    git::git(into, &["remote", "set-url", "origin", github_url])?;
    git::git(into, &["config", "remote.origin.pushurl", remote])?;
    Ok(())
}

/// `path`, made absolute against the process's current directory without
/// touching the filesystem (`std::path::absolute`, not `canonicalize`:
/// the destination does not exist yet).
fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).map_err(Error::Io)
}

/// A scratch directory under `RUNNER_TEMP` when the runner provides one
/// (so it lands on the same volume the workflow uses), else the system
/// temporary directory.
fn work_dir() -> Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    let builder = builder.prefix("capobara-run-");
    match std::env::var_os("RUNNER_TEMP") {
        Some(runner_temp) => builder.tempdir_in(runner_temp),
        None => builder.tempdir(),
    }
    .map_err(Error::Io)
}

fn github_url(definition: &Definition) -> String {
    format!(
        "https://github.com/{}.git",
        definition.destination.repository
    )
}

/// Runs `capobara run`, returning the process exit code (3 when a
/// sync-hold stopped the run, else 0). Every failure is an `Err`, which
/// `main.rs` prints and turns into exit 1.
pub fn run(args: RunArgs) -> Result<i32> {
    eprintln!("{VALIDATION_NOTICE}");

    let root = source_root()?;
    let loaded = resolve_definition(&root, &args.name)?;
    let definition = &loaded.definition;
    let url = github_url(definition);
    let remote = destination_remote(&url);

    let work = work_dir()?;
    // Absolute: `clone_from` runs `git -C <parent> clone ... <into>`, where
    // a relative `into` would resolve against `<parent>` and nest the clone
    // one level too deep.
    let target = match &args.destination {
        Some(path) => absolute(path)?,
        None => work.path().join("projection-target"),
    };
    let report_path = work.path().join("projection-plan.json");
    let definition_path = root.join(format!("config/projections/{}.json", definition.name));

    // "Clone destination and inspect sync-hold".
    clone_from(&remote, &target, false, false)?;
    adopt_destination_origin(&target, &remote, &url)?;
    let api = api_from_env()?;
    let prepared = prepare_destination(
        definition,
        &loaded.text,
        &target,
        &args.source_sha,
        api.as_ref(),
    )?;
    if prepared.held {
        println!("{{\"held\":true}}");
        append_step_summary(HOLD_SUMMARY)?;
        return Ok(3);
    }

    // "Prepare and verify the standalone projection". The workflow writes
    // the projection's markdown summary straight to `$GITHUB_STEP_SUMMARY`
    // (truncating it, as Node's `writeFileSync` does); the receipt block at
    // the end of the proof is appended after it.
    project_run(ProjectArgs {
        command: ProjectCommand::Apply,
        definition: definition_path.clone(),
        source: root.clone(),
        source_sha: args.source_sha.clone(),
        target: target.clone(),
        report: Some(report_path.clone()),
        status_output: None,
        markdown_output: step_summary_path(),
        draft: false,
    })?;
    // TODO(task-19): the workflow runs
    // `node scripts/projections/validate.mjs "$PROJECTION" <target>` here
    // (yml:160). Wire the ported call in once Task 19 lands, and drop the
    // corresponding sentence from `VALIDATION_NOTICE` and `long_about`.
    let verify_definition = definition_path;
    let verified = project_run(ProjectArgs {
        command: ProjectCommand::Verify,
        definition: verify_definition.clone(),
        source: root.clone(),
        source_sha: args.source_sha.clone(),
        target: target.clone(),
        report: None,
        status_output: None,
        markdown_output: None,
        draft: false,
    })?;
    // The workflow runs under `set -euo pipefail`, so `project.mjs verify`
    // reporting drift (exit 1) fails the step. The workflow's own
    // invocation passes no `--report`, which is why the message builder is
    // given `None`.
    contract(
        verified == 0,
        verify_command_failed(&verify_definition, &root, &args.source_sha, &target, None),
    )?;

    // "Recheck and render the Copybara transport".
    let preflight = preflight_publication(
        definition,
        &root,
        &args.source_sha,
        &target,
        &report_path,
        api.as_ref(),
    )?;
    println!(
        "{}",
        serde_json::to_string(&preflight)
            .map_err(|e| Error::Invalid(format!("Failed to serialize preflight: {e}")))?
    );
    if preflight.held {
        append_step_summary(HOLD_SUMMARY)?;
        return Ok(3);
    }

    if args.dry_run {
        println!("{{\"dryRun\":true}}");
        return Ok(0);
    }

    // The publication step, skipped when the destination already converged.
    if !preflight.unchanged {
        let report = read_report(&report_path)?;
        let published = publish_prepared_tree(
            definition,
            &root,
            &args.source_sha,
            &target,
            &report,
            api.as_ref(),
        )?;
        let json = published_json(&published)?;
        println!("{json}");
        if matches!(published, Published::Held) {
            append_step_summary(PUBLISH_HOLD_SUMMARY)?;
            return Ok(3);
        }
        // The workflow's only operator-visible record of what was published
        // (yml:258). Its companion line, the native executable's SHA-256
        // (yml:259), has no meaning here: this binary *is* the tool.
        append_step_summary(&format!("Publication: {json}\n"))?;
    }

    // "Prove the published or converged destination".
    prove_publication(
        definition,
        &root,
        &args.source_sha,
        &preflight,
        &remote,
        &url,
        work.path(),
        api.as_ref(),
    )?;
    Ok(0)
}

/// The workflow's "Prove the published or converged destination" step,
/// transcribed: a fresh `--no-checkout` clone of the destination, the
/// published (or converged) ref checked out detached, `verify` run against
/// it, and the open generated PR set required to match the outcome.
///
/// Returns the proven destination head.
#[allow(clippy::too_many_arguments, reason = "one transcribed workflow step")]
pub fn prove_publication(
    definition: &Definition,
    source: &Path,
    source_sha: &str,
    preflight: &Preflight,
    remote: &str,
    github_url: &str,
    work: &Path,
    api: &dyn GitHubApi,
) -> Result<String> {
    let proof = work.join("projection-proof");
    clone_from(remote, &proof, true, true)?;

    let destination_ref = if preflight.unchanged {
        format!("refs/remotes/origin/{}", definition.destination.branch)
    } else {
        let sync_branch = &definition.destination.sync_branch;
        git::git(
            &proof,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("+refs/heads/{sync_branch}:refs/remotes/origin/{sync_branch}"),
            ],
        )?;
        let published_ref = format!("refs/remotes/origin/{sync_branch}");
        // `git merge-base --is-ancestor "$PRIOR_HEAD" "$destination_ref"`
        // under `set -e`: the published branch must build on the head
        // preflight recorded, never replace it.
        contract(
            is_ancestor(&proof, &preflight.prior_head, &published_ref),
            "Published destination head does not descend from the preflight head",
        )?;
        published_ref
    };

    // Deliberately after the fetch above, not right after the clone: under
    // the test seam the fetch has to reach the local remote, and only the
    // checks below (`cli::project::repo_identity`, via `verify`) need
    // `origin` to read as the GitHub URL. In production this is a no-op
    // either way -- `remote == github_url` -- so the ordering is invisible.
    adopt_destination_origin(&proof, remote, github_url)?;

    git::git(
        &proof,
        &["checkout", "--quiet", "--detach", &destination_ref],
    )?;
    let destination_head = git::git(&proof, &["rev-parse", "HEAD"])?.trim().to_string();

    // TODO(task-19): the workflow runs
    // `node scripts/projections/validate.mjs "$PROJECTION" <proof>` here
    // (yml:287), against the published tree rather than the prepared one.
    let verify_definition = source.join(format!("config/projections/{}.json", definition.name));
    let verified = project_run(ProjectArgs {
        command: ProjectCommand::Verify,
        definition: verify_definition.clone(),
        source: source.to_path_buf(),
        source_sha: source_sha.to_string(),
        target: proof.clone(),
        report: None,
        status_output: None,
        markdown_output: None,
        draft: false,
    })?;
    contract(
        verified == 0,
        verify_command_failed(&verify_definition, source, source_sha, &proof, None),
    )?;

    // `gh pr list --repo ... --state open --base main --head
    // sync/mono-projection --json number,headRefOid,url`, through this
    // crate's own REST transport. `headRefOid` is the REST API's
    // `head.sha`.
    let prs = api
        .call("GET", &open_sync_pr_endpoint(definition), None)?
        .unwrap_or(Value::Null);
    let prs = prs.as_array().ok_or_else(|| {
        Error::Contract("Unreadable destination PR state in the publication proof".into())
    })?;
    if preflight.unchanged {
        contract(
            prs.is_empty(),
            "Converged projection still has an open generated PR",
        )?;
    } else {
        let unique = prs.len() == 1
            && prs[0]
                .get("head")
                .and_then(|head| head.get("sha"))
                .and_then(Value::as_str)
                == Some(destination_head.as_str());
        contract(
            unique,
            "Copybara PR does not uniquely match the verified destination head",
        )?;
    }

    append_step_summary(&format!(
        "### {} Capobara receipt\n\n- Source: `{source_sha}`\n- Destination head: `{destination_head}`\n- Converged without publication: `{}`\n",
        definition.name, preflight.unchanged
    ))?;
    Ok(destination_head)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `clone_from` runs `git -C <parent> clone ... <into>`, so a relative
    /// `--destination` must be resolved against the process's current
    /// directory before it is split into parent and target -- otherwise git
    /// resolves it a second time against `<parent>` and the clone lands one
    /// directory too deep.
    #[test]
    fn a_relative_destination_resolves_against_the_current_directory() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            absolute(Path::new("scratch/projection-target")).unwrap(),
            cwd.join("scratch/projection-target")
        );
        // Positive control: an absolute path is returned unchanged.
        assert_eq!(
            absolute(Path::new("/tmp/projection-target")).unwrap(),
            PathBuf::from("/tmp/projection-target")
        );
    }

    /// The destination clone URL is the literal GitHub HTTPS URL the
    /// workflow clones; `github_url` is what `assert_destination_checkout`
    /// later compares `origin` against, so the two must agree.
    #[test]
    fn the_destination_clone_url_is_the_github_https_url() {
        let raw = serde_json::json!({
            "schemaVersion": 1, "name": "sample", "class": "source-tree", "mode": "copy-v1",
            "sourceRepository": "dx-corp/mono", "visibility": "public",
            "mappings": [{"source": "pkg", "destination": ".", "include": ["src/**"], "exclude": []}],
            "destination": {"repository": "dx-corp/sample", "branch": "main", "syncBranch": "sync/mono-projection", "holdLabel": "sync-hold"},
            "destinationOwned": [".github/**", "SECURITY.md"],
            "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
            "outputManaged": ["src/**"]
        });
        let definition = crate::definition::definition_from_value(raw, &|_| None)
            .unwrap()
            .definition;
        assert_eq!(
            github_url(&definition),
            "https://github.com/dx-corp/sample.git"
        );
    }

    /// `adopt_destination_origin` must be a true no-op in production, where
    /// the clone already came from the GitHub URL: it may not touch
    /// `origin` and must never configure a `pushurl`. A non-existent path
    /// is a sufficient repository here precisely because no git command
    /// should run.
    #[test]
    fn adopting_the_origin_is_a_no_op_when_the_clone_came_from_github() {
        let url = "https://github.com/dx-corp/sample.git";
        assert!(
            adopt_destination_origin(Path::new("/nonexistent/repo"), url, url).is_ok(),
            "a production clone must not run any git command here"
        );
        // Positive control: with a different remote it does try, and fails
        // against the same non-repository path.
        assert!(
            adopt_destination_origin(Path::new("/nonexistent/repo"), "/somewhere/bare", url)
                .is_err()
        );
    }
}
