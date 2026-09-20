//! `plan`, `apply`, `verify`, and `check`: read, compare, and (`apply` only)
//! write a projection between a Mono source checkout and a destination
//! checkout. Ports the `"preview"|"apply"|"check"|"verify"` branch of
//! `main` from `scripts/projections/project.mjs`; the numbered comments
//! below match the eleven steps transcribed in the Task 8 brief, which in
//! turn match Node's `requireValue` calls in source order.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use serde_json::Value;

use crate::build::{BuildInput, build_projection};
use crate::definition::{Definition, definition_digest, load_definition, projection_input_roots};
use crate::git::{self, is_ancestor, is_sha, is_tree_id_or_digest};
use crate::modes::sdk_assembly;
use crate::receipt::Provenance;
use crate::report::{Report, markdown_summary};
use crate::snapshot::with_snapshot;
use crate::tooldigest;
use crate::tree::apply_tree;
use crate::{Error, Result, invalid};

/// The stored receipt's field names, exactly as Node's `keys(previous,
/// [...], "stored provenance")` lists them. Order-independent; used only
/// to compare against the receipt's actual key set.
const STORED_PROVENANCE_FIELDS: [&str; 11] = [
    "schemaVersion",
    "projection",
    "projectionSchemaVersion",
    "sourceRepository",
    "sourceSha",
    "destinationRepository",
    "priorProjectedBase",
    "definitionDigest",
    "toolDigest",
    "contentDigest",
    "publicationEligible",
];

/// Which of the four projection subcommands is running. Only `apply`
/// writes to the destination checkout; only `verify` requires the built
/// provenance to match a stored receipt; `check` and `verify` (and only
/// those two) turn detected drift into exit code 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectCommand {
    Plan,
    Apply,
    Verify,
    Check,
}

/// Everything `run` needs, independent of how it was gathered: `main.rs`
/// builds this from a parsed `cli::ProjectCliArgs` plus which subcommand
/// matched (`ProjectCliArgs::into_project_args`); Tasks 12 and 13 build it
/// directly to call `run` in-process, without going through argv.
pub struct ProjectArgs {
    pub command: ProjectCommand,
    pub definition: PathBuf,
    pub source: PathBuf,
    pub source_sha: String,
    pub target: PathBuf,
    pub report: Option<PathBuf>,
    pub status_output: Option<PathBuf>,
    pub markdown_output: Option<PathBuf>,
    pub draft: bool,
}

static REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:https://github\.com/|git@github\.com:)([^/]+/[^/]+?)(?:\.git)?$")
        .expect("static regex is valid")
});

/// Ports `repoIdentity`: `root`'s `origin` remote URL, parsed into its
/// GitHub `owner/name` identity.
pub fn repo_identity(root: &Path) -> Result<String> {
    let remote = git::git(root, &["remote", "get-url", "origin"])?;
    REMOTE
        .captures(remote.trim())
        .map(|captures| captures[1].to_string())
        .ok_or_else(|| Error::Invalid("Unrecognized source/destination remote".into()))
}

/// `path.resolve(path)`-equivalent: makes `path` absolute against the
/// current directory (if it is not already) and collapses `.`/`..`
/// components lexically, without touching the filesystem or following
/// symlinks -- matching Node's `path.resolve`, and unlike
/// `Path::canonicalize`, which does both.
fn absolutize(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(Error::Io)?.join(path)
    };
    Ok(normalize_lexically(&joined))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `sdk-assembly-v1` policy lookup: the reviewed, immutable input list of
/// one of the three policies in `modes::sdk_assembly::policies`, or `None`
/// for any other name. `definition::validate_definition` rejects an
/// `sdk-assembly-v1` definition whose policy this does not know, so every
/// caller that loads a real definition -- `plan`/`apply`/`verify`/`check`
/// here, and `catalog check`/`catalog matrix` in `cli::catalog` -- must use
/// this and not a stand-in. It stood in as a constant `None` between the
/// task that added the subcommands and the task that added the policies;
/// while it did, all three `sdk-assembly-v1` projections failed to load with
/// "Unknown SDK assembly policy".
pub(crate) fn sdk_inputs(name: &str) -> Option<Vec<String>> {
    sdk_assembly::input_roots(name)
}

fn write_json_report(path: &Path, report: &Report) -> Result<()> {
    let mut text = serde_json::to_string_pretty(report)
        .map_err(|e| Error::Invalid(format!("Failed to serialize report: {e}")))?;
    text.push('\n');
    std::fs::write(path, text).map_err(Error::Io)
}

/// Loads and validates the receipt at `receipt_path`, if any. Ports the
/// `previous !== null` branch of `main` in `scripts/projections/project.mjs`,
/// including Node's two-tier `keys()` shape check: a non-object receipt
/// (an array, string, number, bool, or `null` after JSON parsing) fails
/// *first* with `Invalid stored provenance`; only a JSON object with the
/// wrong key set fails with `Unknown or missing stored provenance fields`.
///
/// Every check after the key-set check runs on the raw `serde_json::Value`
/// fields -- exactly mirroring Node's untyped `===`/regex comparisons,
/// which simply evaluate to `false` on a wrong-typed field instead of
/// throwing -- so a receipt whose fields have the wrong JSON type (for
/// example `"schemaVersion": "1"` or `"publicationEligible": "yes"`) is
/// *not* rejected here at deserialization; it reaches the later, more
/// specific checks (identity, then SHA/digest/bool shape) exactly as it
/// would in Node. Only once every check has passed does this deserialize
/// into a typed `Provenance`, which by construction cannot fail; a
/// residual error is folded into `Malformed stored provenance` rather than
/// given its own message.
fn load_stored_receipt(
    receipt_path: &Path,
    definition: &Definition,
    draft: bool,
) -> Result<Option<Provenance>> {
    if !receipt_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(receipt_path).map_err(Error::Io)?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Invalid("Invalid stored provenance".into()))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::Invalid("Invalid stored provenance".into()))?;

    let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
    actual.sort_unstable();
    let mut expected: Vec<&str> = STORED_PROVENANCE_FIELDS.to_vec();
    expected.sort_unstable();
    invalid(
        actual == expected,
        "Unknown or missing stored provenance fields",
    )?;

    let identity_ok = object.get("schemaVersion").and_then(Value::as_i64) == Some(1)
        && object
            .get("projectionSchemaVersion")
            .and_then(Value::as_i64)
            == Some(i64::from(definition.schema_version))
        && object.get("projection").and_then(Value::as_str) == Some(definition.name.as_str())
        && object.get("sourceRepository").and_then(Value::as_str)
            == Some(definition.source_repository.as_str())
        && object.get("destinationRepository").and_then(Value::as_str)
            == Some(definition.destination.repository.as_str());
    invalid(identity_ok, "Stored provenance identity mismatch")?;

    let string_field = |key: &str| object.get(key).and_then(Value::as_str);
    let publication_eligible_value = object.get("publicationEligible").and_then(Value::as_bool);
    let shape_ok = string_field("sourceSha").is_some_and(is_sha)
        && string_field("priorProjectedBase").is_some_and(is_sha)
        && ["definitionDigest", "toolDigest", "contentDigest"]
            .into_iter()
            .all(|key| string_field(key).is_some_and(is_tree_id_or_digest))
        && publication_eligible_value.is_some();
    invalid(shape_ok, "Malformed stored provenance")?;

    let publication_eligible =
        publication_eligible_value.expect("checked by the shape_ok invalid() call above");
    invalid(
        draft || publication_eligible,
        "Draft receipt cannot be a publication base",
    )?;

    let previous: Provenance = serde_json::from_value(value)
        .map_err(|_| Error::Invalid("Malformed stored provenance".into()))?;
    Ok(Some(previous))
}

/// Runs one of `plan`/`apply`/`verify`/`check` and returns the process
/// exit code: 0 when clean (or for `plan`/`apply` regardless of drift), 1
/// when `check` or `verify` reports drift. Every failure along the way is
/// an `Err`; `main` prints its message and exits 2 for all four of these
/// subcommands regardless of the `Error` variant (Node's file-level
/// `catch` sets `process.exitCode = 2` for any thrown error, including a
/// `verify` provenance mismatch).
pub fn run(args: ProjectArgs) -> Result<i32> {
    // Step 1: source/target must be absolute and disjoint in one direction
    // (the destination cannot contain the source repository).
    let source = absolutize(&args.source)?;
    let target = absolutize(&args.target)?;
    invalid(
        target != source && !source.starts_with(&target),
        "Destination cannot contain source repository",
    )?;

    // Step 2: load and validate the definition; every mapping's source
    // root and the destination root must be disjoint.
    let loaded = load_definition(&args.definition, &sdk_inputs)?;
    let definition = &loaded.definition;
    for mapping in &definition.mappings {
        let mapped = normalize_lexically(&source.join(&mapping.source));
        invalid(
            target != mapped && !target.starts_with(&mapped) && !mapped.starts_with(&target),
            "Mapped source and destination must be disjoint",
        )?;
    }

    // Step 3
    invalid(is_sha(&args.source_sha), "Invalid source SHA")?;
    let source_sha = args.source_sha.as_str();

    // Step 4
    invalid(
        repo_identity(&source)? == definition.source_repository
            && repo_identity(&target)? == definition.destination.repository,
        "Repository identity mismatch",
    )?;

    // Step 5
    let head = git::git(&source, &["rev-parse", "HEAD"])?;
    invalid(head.trim() == source_sha, "Source revision mismatch")?;

    // Step 6: outside draft mode, the definition and this tool's own
    // sources must match what is committed at `source_sha`.
    let tool_digest = tooldigest::embedded();
    if !args.draft {
        let committed = git::git(
            &source,
            &[
                "show",
                &format!("{source_sha}:config/projections/{}.json", definition.name),
            ],
        )?;
        invalid(
            committed.trim() == loaded.text.trim(),
            "Definition differs from source revision",
        )?;
        let at_revision = tooldigest::at_revision(&source, source_sha)?;
        invalid(
            at_revision == tool_digest,
            "Projector differs from source revision: rust/tools/capobara",
        )?;
    }

    // Step 7
    let target_head = git::git(&target, &["rev-parse", "HEAD^{commit}"])?
        .trim()
        .to_string();
    let destination_base = git::git(
        &target,
        &[
            "rev-parse",
            &format!(
                "refs/remotes/origin/{}^{{commit}}",
                definition.destination.branch
            ),
        ],
    )?
    .trim()
    .to_string();
    let mut prior_projected_base = destination_base.clone();

    // Step 8: validate any stored receipt at the destination.
    let receipt_path = target.join(&definition.provenance);
    let stored = load_stored_receipt(&receipt_path, definition, args.draft)?;

    // Step 9: decide whether the stored receipt (if any) is for the exact
    // projection revision being run now, or belongs to a prior one.
    let current_definition_digest = definition_digest(&loaded.text)?;
    let same_projection_revision = stored.as_ref().is_some_and(|previous| {
        previous.source_sha == source_sha && previous.definition_digest == current_definition_digest
    });
    if same_projection_revision {
        let previous = stored
            .as_ref()
            .expect("same_projection_revision is true only when stored is Some");
        invalid(
            target_head == destination_base
                || is_ancestor(&target, &previous.prior_projected_base, &target_head),
            "Stored projection base is not an ancestor of the destination",
        )?;
        prior_projected_base = previous.prior_projected_base.clone();
    } else {
        invalid(
            is_ancestor(&target, &destination_base, &target_head),
            "Prepared destination does not contain the current default branch",
        )?;
    }

    // Step 10: build the projection from a snapshot of the source at
    // `source_sha`, optionally verify it against the stored receipt, write
    // any requested report/status/markdown output, and (`apply` only)
    // write the projected tree.
    let roots = projection_input_roots(definition, &sdk_inputs);
    let publication_eligible = !args.draft;
    let command = args.command;
    let definition_text = loaded.text.as_str();

    let (message, exit_code) = with_snapshot(&source, source_sha, &roots, |snapshot_root| {
        let built = build_projection(
            BuildInput {
                definition,
                definition_text,
                source_root: snapshot_root,
                target_root: &target,
                source_sha,
                prior_projected_base: prior_projected_base.as_str(),
                tool_digest,
                publication_eligible,
            },
            &sdk_assembly::assemble,
        )?;

        if command == ProjectCommand::Verify {
            match &stored {
                None => return Err(Error::Invalid("Invalid provenance".into())),
                Some(previous) => built.provenance.verify_against(previous)?,
            }
        }

        let report = Report::from(&built);
        for path in args.report.iter().chain(args.status_output.iter()) {
            write_json_report(path, &report)?;
        }
        if let Some(path) = &args.markdown_output {
            let markdown = markdown_summary(
                definition,
                source_sha,
                prior_projected_base.as_str(),
                &built,
                &report,
            );
            std::fs::write(path, markdown).map_err(Error::Io)?;
        }

        if command == ProjectCommand::Apply {
            apply_tree(&target, &built.plan)?;
        }

        let message = format!(
            "{}: {} changed, {} deleted; content {}",
            definition.name,
            built.plan.copied_count(),
            built.plan.deleted_count(),
            built.provenance.content_digest
        );
        let drift = built.plan.copied_count() + built.plan.deleted_count() > 0;
        let exit_code =
            if matches!(command, ProjectCommand::Check | ProjectCommand::Verify) && drift {
                1
            } else {
                0
            };
        Ok((message, exit_code))
    })?;

    println!("{message}");
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::definition_from_value;

    fn definition() -> Definition {
        let raw = serde_json::json!({
            "schemaVersion": 1, "name": "sample", "class": "source-tree", "mode": "copy-v1",
            "sourceRepository": "dx-corp/mono", "visibility": "public",
            "mappings": [{"source": "pkg", "destination": ".", "include": ["src/**"], "exclude": []}],
            "destination": {"repository": "dx-corp/sample", "branch": "main", "syncBranch": "sync/mono-projection", "holdLabel": "sync-hold"},
            "destinationOwned": [".github/**", "SECURITY.md"],
            "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
            "outputManaged": ["src/**"]
        });
        definition_from_value(raw, &|_| None).unwrap().definition
    }

    fn valid_receipt() -> Value {
        serde_json::json!({
            "schemaVersion": 1,
            "projection": "sample",
            "projectionSchemaVersion": 1,
            "sourceRepository": "dx-corp/mono",
            "sourceSha": "1".repeat(40),
            "destinationRepository": "dx-corp/sample",
            "priorProjectedBase": "2".repeat(40),
            "definitionDigest": "d".repeat(64),
            "toolDigest": "3".repeat(64),
            "contentDigest": "c".repeat(64),
            "publicationEligible": true,
        })
    }

    fn write_receipt(dir: &std::path::Path, value: &Value) -> PathBuf {
        let path = dir.join("receipt.json");
        std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    // Positive control: a well-formed receipt still loads, so the checks
    // added for the malformed cases below have not made every receipt
    // fail.
    #[test]
    fn a_well_formed_receipt_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_receipt(dir.path(), &valid_receipt());
        let loaded = load_stored_receipt(&path, &definition(), false).unwrap();
        assert!(loaded.is_some());
    }

    #[test]
    fn a_missing_receipt_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        assert!(
            load_stored_receipt(&path, &definition(), false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_non_object_receipt_fails_the_shape_check_before_the_key_set_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_receipt(dir.path(), &serde_json::json!([]));
        let err = load_stored_receipt(&path, &definition(), false)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Invalid stored provenance");
    }

    #[test]
    fn an_extra_key_is_unknown_or_missing_stored_provenance_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut receipt = valid_receipt();
        receipt["extra"] = serde_json::json!(true);
        let path = write_receipt(dir.path(), &receipt);
        let err = load_stored_receipt(&path, &definition(), false)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Unknown or missing stored provenance fields");
    }

    #[test]
    fn a_wrong_typed_identity_field_reaches_identity_mismatch_not_a_parse_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut receipt = valid_receipt();
        receipt["schemaVersion"] = serde_json::json!("1");
        let path = write_receipt(dir.path(), &receipt);
        let err = load_stored_receipt(&path, &definition(), false)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Stored provenance identity mismatch");
    }

    #[test]
    fn a_malformed_digest_reaches_the_malformed_check_not_identity_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut receipt = valid_receipt();
        receipt["contentDigest"] = serde_json::json!("zz");
        let path = write_receipt(dir.path(), &receipt);
        let err = load_stored_receipt(&path, &definition(), false)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Malformed stored provenance");
    }

    #[test]
    fn a_wrong_typed_publication_eligible_reaches_the_malformed_check() {
        let dir = tempfile::tempdir().unwrap();
        let mut receipt = valid_receipt();
        receipt["publicationEligible"] = serde_json::json!("yes");
        let path = write_receipt(dir.path(), &receipt);
        let err = load_stored_receipt(&path, &definition(), false)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Malformed stored provenance");
    }

    #[test]
    fn a_non_publication_eligible_receipt_is_rejected_outside_draft_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut receipt = valid_receipt();
        receipt["publicationEligible"] = serde_json::json!(false);
        let path = write_receipt(dir.path(), &receipt);
        let err = load_stored_receipt(&path, &definition(), false)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Draft receipt cannot be a publication base");
        // Positive control: the same receipt is accepted in draft mode.
        assert!(
            load_stored_receipt(&path, &definition(), true)
                .unwrap()
                .is_some()
        );
    }
}
