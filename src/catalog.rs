//! The approved repository catalog, main-authorized revision checks, and
//! the publication matrix. Ports `scripts/projections/catalog.mjs` (Node).
//! Line references below are against that file as read on
//! `feat/capobara-catalog`.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::definition::{LoadedDefinition, load_definition, projection_input_roots};
use crate::git::{self, is_ancestor, is_sha};
use crate::tree::order::sort_js;
use crate::{Error, Result, invalid};

/// Ports `MAIN_AUTHORITY_REF`.
pub const MAIN_AUTHORITY_REF: &str = "refs/remotes/origin/main";

/// Ports `PROJECTION_RUNTIME_INPUTS`, with Node's `scripts/projections`
/// replaced by this crate's own path, `rust/tools/capobara`. Until Mono
/// cuts over to this binary, CI still runs the Node projector using Node's
/// own list; this list is what a Rust shadow job uses to compute the same
/// `sourceSha`, which only happens when both lists select the same
/// projector-affecting commit -- the shadow job asserts that before
/// trusting either result (see the Task 9 brief).
pub const PROJECTION_RUNTIME_INPUTS: [&str; 9] = [
    ".github/actions/setup-mise",
    ".github/workflows/repository-projections.yml",
    "mise.toml",
    "rust-toolchain.toml",
    "scripts/ci/gcs-directory-cache.sh",
    "scripts/ci/mise-cache-key.py",
    "scripts/dev/mise-install-retry.sh",
    "scripts/dev/prepare-mise-rust.sh",
    "rust/tools/capobara",
];

/// Ports `SHARED_SOURCE_REVISION_GROUPS`: the examples repository installs
/// both standalone SDKs at the exact source revision recorded in its own
/// provenance, so all three artifacts share one selected `sourceSha`.
pub const SHARED_SOURCE_REVISION_GROUPS: [[&str; 3]; 1] =
    [["examples", "deixic-node", "deixic-python"]];

const SOURCE_REPOSITORY: &str = "dx-corp/mono";
const CATALOG_FIELDS: [&str; 3] = ["projections", "schemaVersion", "sourceRepository"];
const REQUIRED_DESTINATION_OWNED: [&str; 2] = [".github/**", "SECURITY.md"];

/// The publication matrix's JSON shape, exactly
/// `{"include":[{"name":...,"repository":...,"sourceSha":...}]}` --
/// `serde`'s struct serialization keeps declared field order (unlike
/// `serde_json::Value`'s object map, which is why this doesn't need
/// `ordered::OrderedValue`).
#[derive(Debug, Clone, Serialize)]
pub struct Matrix {
    pub include: Vec<MatrixEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatrixEntry {
    pub name: String,
    pub repository: String,
    pub source_sha: String,
}

/// `^[a-z][a-z0-9-]*$`, written out rather than compiled as a `Regex`
/// (matching `readCatalog`'s catalog-name check; `load_definition`'s own
/// name-pattern check, on the definition's own `name` field, is separate
/// and already enforced by `validate_definition_value`).
fn is_safe_catalog_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Ports `readCatalog`: parses and validates
/// `config/projections/repositories.json`, then loads and cross-checks each
/// named `config/projections/<name>.json`, in the catalog's own order.
pub fn read_catalog(
    root: &Path,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<Vec<LoadedDefinition>> {
    let catalog_path = root.join("config/projections/repositories.json");
    let text = std::fs::read_to_string(&catalog_path)
        .map_err(|e| Error::Invalid(format!("{}: {e}", catalog_path.display())))?;
    let catalog: Value = serde_json::from_str(&text)
        .map_err(|e| Error::Invalid(format!("Invalid projection JSON: {e}")))?;

    let object = catalog.as_object();
    let schema_version_ok = object
        .and_then(|o| o.get("schemaVersion"))
        .and_then(Value::as_i64)
        == Some(1);
    let source_repository_ok = object
        .and_then(|o| o.get("sourceRepository"))
        .and_then(Value::as_str)
        == Some(SOURCE_REPOSITORY);
    let keys_ok = object.is_some_and(|o| {
        let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys == CATALOG_FIELDS
    });
    let projections = object
        .and_then(|o| o.get("projections"))
        .and_then(Value::as_array);
    let projections_ok = projections.is_some_and(|items| {
        !items.is_empty()
            && items
                .iter()
                .enumerate()
                .all(|(i, item)| !items[..i].contains(item))
    });
    // The `Option` produced above is consumed by this same `match`, not by a
    // separate `.expect()` after an independent `invalid()` call: a future
    // edit to any of the four `_ok` booleans can no longer desynchronize the
    // guard from the value it guards, because there is only one place where
    // both are read together.
    let items = match projections
        .filter(|_| schema_version_ok && source_repository_ok && keys_ok && projections_ok)
    {
        Some(items) => items,
        None => return Err(Error::Invalid("Invalid repository catalog".into())),
    };

    items
        .iter()
        .map(|item| load_catalog_entry(root, item, sdk_inputs))
        .collect()
}

/// One iteration of `readCatalog`'s `catalog.projections.map(...)` body: the
/// unsafe-name check, then loading and validating the definition, then the
/// catalog-identity and destination-ownership cross-checks.
fn load_catalog_entry(
    root: &Path,
    item: &Value,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<LoadedDefinition> {
    let name = item
        .as_str()
        .filter(|name| is_safe_catalog_name(name))
        .ok_or_else(|| Error::Invalid("Unsafe catalog name".into()))?;
    let definition_path = root.join("config/projections").join(format!("{name}.json"));
    let loaded = load_definition(&definition_path, sdk_inputs)?;
    let d = &loaded.definition;
    let identity_ok = d.name == name
        && d.source_repository == SOURCE_REPOSITORY
        && d.destination.repository == format!("dx-corp/{name}")
        && d.visibility == "public"
        && d.destination.branch == "main"
        && d.destination.sync_branch == "sync/mono-projection";
    invalid(identity_ok, format!("Catalog identity mismatch: {name}"))?;
    for path in REQUIRED_DESTINATION_OWNED {
        invalid(
            d.destination_owned.iter().any(|owned| owned == path),
            format!("Missing destination ownership: {name}: {path}"),
        )?;
    }
    Ok(loaded)
}

/// Ports `assertMainAuthorizedRevision`: `revision` must be a full SHA that
/// is an ancestor of `MAIN_AUTHORITY_REF`. Returns the authority SHA (not
/// `revision`) on success.
pub fn assert_main_authorized_revision(root: &Path, revision: &str) -> Result<String> {
    invalid(is_sha(revision), "Missing immutable source revision")?;
    let authorized = git::git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("{MAIN_AUTHORITY_REF}^{{commit}}"),
        ],
    )
    .ok()
    .map(|authority| authority.trim().to_string())
    .filter(|authority| is_ancestor(root, revision, authority));
    authorized.ok_or_else(|| {
        Error::Invalid(format!(
            "Projection revision is not authorized by {MAIN_AUTHORITY_REF}: {revision}"
        ))
    })
}

/// Ports `sourceRevisionInputs`: the sorted, deduplicated union of input
/// roots (from `projectionInputRoots`) over `definition`'s coupled group
/// (the `SHARED_SOURCE_REVISION_GROUPS` member containing its name, else
/// the name alone), plus `PROJECTION_RUNTIME_INPUTS`, plus each coupled
/// definition's own catalog file, plus the catalog file itself.
fn source_revision_inputs(
    name: &str,
    definitions: &[LoadedDefinition],
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<Vec<String>> {
    let group: Vec<&str> = SHARED_SOURCE_REVISION_GROUPS
        .iter()
        .find(|names| names.contains(&name))
        .map(|names| names.to_vec())
        .unwrap_or_else(|| vec![name]);
    let coupled: Vec<&LoadedDefinition> = group
        .iter()
        .map(|coupled_name| {
            definitions
                .iter()
                .find(|candidate| candidate.definition.name == *coupled_name)
                .ok_or_else(|| {
                    Error::Invalid(format!("Missing coupled projection: {coupled_name}"))
                })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut inputs: Vec<String> = Vec::new();
    {
        let mut push = |value: String| {
            if seen.insert(value.clone()) {
                inputs.push(value);
            }
        };
        for candidate in &coupled {
            for root in projection_input_roots(&candidate.definition, sdk_inputs) {
                push(root);
            }
        }
        for input in PROJECTION_RUNTIME_INPUTS {
            push(input.to_string());
        }
        for candidate in &coupled {
            push(format!(
                "config/projections/{}.json",
                candidate.definition.name
            ));
        }
        push("config/projections/repositories.json".to_string());
    }
    sort_js(&mut inputs);
    Ok(inputs)
}

/// Ports `publicationMatrix`: validates the catalog, authorizes `HEAD`
/// against `MAIN_AUTHORITY_REF`, then for each selected definition finds
/// the latest commit touching its (coupled) source-revision inputs and
/// authorizes that commit too.
pub fn publication_matrix(
    root: &Path,
    requested: &str,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<Matrix> {
    let definitions = read_catalog(root, sdk_inputs)?;
    invalid(
        requested == "all" || definitions.iter().any(|d| d.definition.name == requested),
        format!("Unknown projection: {requested}"),
    )?;
    let head = git::git(root, &["rev-parse", "HEAD^{commit}"])?
        .trim()
        .to_string();
    assert_main_authorized_revision(root, &head)?;

    let include = definitions
        .iter()
        .filter(|d| requested == "all" || d.definition.name == requested)
        .map(|d| {
            let name = &d.definition.name;
            let inputs = source_revision_inputs(name, &definitions, sdk_inputs)?;
            let mut args: Vec<&str> = vec!["log", "-1", "--format=%H", "HEAD", "--"];
            args.extend(inputs.iter().map(String::as_str));
            let source_sha = git::git(root, &args)?.trim().to_string();
            invalid(
                is_sha(&source_sha),
                format!("Missing source revision: {name}"),
            )?;
            assert_main_authorized_revision(root, &source_sha)?;
            Ok(MatrixEntry {
                name: name.clone(),
                repository: d.definition.destination.repository.clone(),
                source_sha,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Matrix { include })
}
