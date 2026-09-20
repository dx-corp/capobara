//! The immediate-pre-publication recheck. Ports
//! `scripts/projections/copybara-preflight.mjs`:
//! `publicationDisposition` and `preflightCopybaraPublication`.
//!
//! Every check here guards a publication contract over untrusted git or
//! GitHub state, so each one is a `contract()` (Node: a `requireValue`
//! throw, which the script's file-level `catch` turns into exit 1),
//! matching `transport::git` and `transport::github`.
//!
//! Node runs `project.mjs verify` and `project.mjs preview` as child
//! processes; this module calls `cli::project::run` in-process instead,
//! exactly as `transport::git::publish_prepared_tree` does. Node's
//! `preview` is this crate's `ProjectCommand::Plan` (see
//! `cli::project`'s module doc).

use std::collections::BTreeSet;
use std::path::Path;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::cli::project::{ProjectArgs, ProjectCommand, run as project_run};
use crate::definition::Definition;
use crate::git::{self, is_sha};
use crate::transport::git::{
    assert_candidate_matches_main_projection, assert_destination_checkout, changed_paths,
    read_report, require_utf8, verify_command_failed,
};
use crate::transport::github::{GitHubApi, read_publication_state};
use crate::{Error, Result, contract};

/// Ports `publicationDisposition`: `"unchanged"` only when the destination
/// working tree has no changed path, there is no open generated PR, and
/// re-projecting onto the destination's own default branch would change
/// nothing either.
pub fn publication_disposition(
    changed_path_count: usize,
    existing_pr: bool,
    main_changed_count: usize,
) -> &'static str {
    if changed_path_count == 0 && !existing_pr && main_changed_count == 0 {
        "unchanged"
    } else {
        "publish"
    }
}

/// The result of `preflight_publication`, printed as one JSON line by
/// `capobara preflight`.
///
/// `Serialize` is hand-written rather than derived because Node returns two
/// *different* object shapes from one function: a bare `{ held: true }`
/// when the destination PR is sync-held, and the full six-key object
/// otherwise (`copybara-preflight.mjs`: `if (first.held) return { held:
/// true };` versus the `return { held: false, unchanged, destinationFetch,
/// priorHead, fileCount, contentDigest }` at the end). The workflow step
/// that consumes this JSON only reads the other five keys on the
/// `status != 3` branch, but the printed bytes are a public interface, so
/// they are reproduced exactly. Field order matches Node's object literal;
/// `skip_serializing_if` cannot express "skip these five when `held`",
/// since a field predicate cannot see its sibling fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    pub held: bool,
    pub unchanged: bool,
    pub destination_fetch: String,
    pub prior_head: String,
    pub file_count: usize,
    pub content_digest: String,
}

impl Preflight {
    /// Node's `{ held: true }`: the five publication fields are never read
    /// (and never serialized) on this branch.
    fn held() -> Preflight {
        Preflight {
            held: true,
            unchanged: false,
            destination_fetch: String::new(),
            prior_head: String::new(),
            file_count: 0,
            content_digest: String::new(),
        }
    }
}

impl Serialize for Preflight {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        if self.held {
            let mut state = serializer.serialize_struct("Preflight", 1)?;
            state.serialize_field("held", &true)?;
            return state.end();
        }
        let mut state = serializer.serialize_struct("Preflight", 6)?;
        state.serialize_field("held", &false)?;
        state.serialize_field("unchanged", &self.unchanged)?;
        state.serialize_field("destinationFetch", &self.destination_fetch)?;
        state.serialize_field("priorHead", &self.prior_head)?;
        state.serialize_field("fileCount", &self.file_count)?;
        state.serialize_field("contentDigest", &self.content_digest)?;
        state.end()
    }
}

/// The five report keys Node compares between the prepared report and the
/// plan regenerated against the immutable destination base, in Node's own
/// iteration order (`copybara-preflight.mjs`'s `for (const key of [...])`).
const PLAN_KEYS: [&str; 5] = [
    "copiedPaths",
    "deletedPaths",
    "copiedCount",
    "deletedCount",
    "provenance",
];

/// Ports `preflightCopybaraPublication`.
///
/// The brief's abbreviated signature also lists `raw` and `tool_digest`.
/// Neither has a use here: `cli::project::run` loads and validates the
/// definition from its own path (so no raw definition text is needed --
/// unlike `transport::git::prepare_destination`, which computes a
/// `definitionDigest` itself), and it reads `tooldigest::embedded()`
/// internally (so no tool digest needs threading through). Both omissions
/// follow the precedent `transport::git::publish_prepared_tree` set for
/// the same abbreviated-signature style in Task 12; see the task-13 report.
pub fn preflight_publication(
    definition: &Definition,
    source: &Path,
    source_sha: &str,
    target: &Path,
    report_path: &Path,
    api: &dyn GitHubApi,
) -> Result<Preflight> {
    contract(is_sha(source_sha), "Missing immutable source revision")?;
    assert_destination_checkout(definition, target)?;
    let current_branch = git::git(target, &["branch", "--show-current"])?;
    contract(
        current_branch.trim() == definition.destination.sync_branch,
        "Preflight requires the declared generated PR branch",
    )?;

    let first = read_publication_state(definition, api)?;
    if first.held {
        return Ok(Preflight::held());
    }

    let scratch = tempfile::Builder::new()
        .prefix("copybara-preflight-")
        .tempdir()
        .map_err(Error::Io)?;

    let definition_path = source.join(format!("config/projections/{}.json", definition.name));

    // Recompute the full tree and provenance against the live destination.
    let verified_path = scratch.path().join("verified.json");
    let verified_code = project_run(ProjectArgs {
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
    // Node spawns `project.mjs verify` with `execFileSync`, which throws on
    // any non-zero exit -- including the exit code 1 `verify` uses to
    // report drift. An in-process call reports that as a returned code
    // instead of an error, so it is checked explicitly here, with the same
    // message `publish_prepared_tree` reports for the same condition.
    contract(
        verified_code == 0,
        verify_command_failed(
            &definition_path,
            source,
            source_sha,
            target,
            Some(&verified_path),
        ),
    )?;

    let report = read_report(report_path)?;
    let verified = read_report(&verified_path)?;
    contract(
        report.provenance == verified.provenance,
        "Prepared report does not match verified projection provenance",
    )?;

    // The submitted changed/deleted lists are data, not staging authority.
    // Regenerate the plan against the immutable destination base before
    // trusting them.
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
        definition: definition_path,
        source: source.to_path_buf(),
        source_sha: source_sha.to_string(),
        target: baseline.clone(),
        report: Some(planned_path.clone()),
        status_output: None,
        markdown_output: None,
        draft: false,
    })?;
    let planned = read_report(&planned_path)?;
    for key in PLAN_KEYS {
        let equal = match key {
            "copiedPaths" => report.copied_paths == planned.copied_paths,
            "deletedPaths" => report.deleted_paths == planned.deleted_paths,
            "copiedCount" => report.copied_count == planned.copied_count,
            "deletedCount" => report.deleted_count == planned.deleted_count,
            _ => report.provenance == planned.provenance,
        };
        contract(
            equal,
            format!("Prepared report does not match regenerated projection plan: {key}"),
        )?;
    }

    contract(
        verified.provenance.publication_eligible,
        "Prepared projection is not publication eligible",
    )?;

    let candidate =
        assert_candidate_matches_main_projection(definition, source, source_sha, target)?;

    let planned_paths: BTreeSet<&str> = report
        .copied_paths
        .iter()
        .chain(report.deleted_paths.iter())
        .map(String::as_str)
        .collect();
    let actual = changed_paths(target)?;
    contract(
        actual
            .iter()
            .all(|path| planned_paths.contains(path.as_str())),
        "Unplanned destination modifications",
    )?;

    let final_state = read_publication_state(definition, api)?;
    if final_state.held {
        return Ok(Preflight::held());
    }

    let remote_branch = format!("refs/remotes/origin/{}", definition.destination.sync_branch);
    let branch_exists = git::git_ok(target, &["show-ref", "--verify", "--quiet", &remote_branch]);
    let (destination_fetch, head_ref) = if branch_exists {
        (
            definition.destination.sync_branch.clone(),
            format!("{remote_branch}^{{commit}}"),
        )
    } else {
        (
            definition.destination.branch.clone(),
            format!(
                "refs/remotes/origin/{}^{{commit}}",
                definition.destination.branch
            ),
        )
    };
    let prior_head = git::git(target, &["rev-parse", &head_ref])?
        .trim()
        .to_string();
    contract(is_sha(&prior_head), "Invalid destination branch head")?;

    let unchanged = publication_disposition(
        actual.len(),
        first.pr.is_some(),
        candidate.main_changed_count,
    ) == "unchanged";

    Ok(Preflight {
        held: false,
        unchanged,
        destination_fetch,
        prior_head,
        file_count: candidate.file_count,
        content_digest: report.provenance.content_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Node: `publicationDisposition` is `"unchanged"` only when all three
    /// inputs are empty/false; every other corner is `"publish"`.
    #[test]
    fn disposition_is_unchanged_only_when_nothing_changed_anywhere() {
        assert_eq!(publication_disposition(0, false, 0), "unchanged");
        assert_eq!(publication_disposition(1, false, 0), "publish");
        assert_eq!(publication_disposition(0, true, 0), "publish");
        assert_eq!(publication_disposition(0, false, 1), "publish");
        assert_eq!(publication_disposition(1, true, 1), "publish");
    }

    /// The held shape is Node's bare `{"held":true}`, not the full
    /// six-key object with empty placeholders.
    #[test]
    fn a_held_preflight_serializes_to_exactly_one_key() {
        assert_eq!(
            serde_json::to_string(&Preflight::held()).unwrap(),
            r#"{"held":true}"#
        );
    }

    /// Positive control for the test above: the non-held shape carries all
    /// six keys, camelCased, in Node's object-literal order.
    #[test]
    fn a_publishable_preflight_serializes_every_key_in_node_order() {
        let preflight = Preflight {
            held: false,
            unchanged: false,
            destination_fetch: "sync/mono-projection".into(),
            prior_head: "a".repeat(40),
            file_count: 3,
            content_digest: "c".repeat(64),
        };
        assert_eq!(
            serde_json::to_string(&preflight).unwrap(),
            format!(
                r#"{{"held":false,"unchanged":false,"destinationFetch":"sync/mono-projection","priorHead":"{}","fileCount":3,"contentDigest":"{}"}}"#,
                "a".repeat(40),
                "c".repeat(64)
            )
        );
    }
}
