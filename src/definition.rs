//! Projection definition schema and validation.
//!
//! Ports `validateDefinition`, `definitionDigest`, and `projectionInputRoots`
//! from `scripts/projections/project.mjs` (Node). Definitions live at
//! `config/projections/<name>.json` in Mono. The numbered rules below match
//! the transcription in the Task 5 brief, which in turn matches the order of
//! `requireValue` calls in `validateDefinition` (lines 71-240 of project.mjs):
//! the first failing rule's message must match Node's for a given input.
//!
//! Validation runs on the raw `serde_json::Value` (object key order is
//! irrelevant to validation; `serde_json::Map` is a sorted `BTreeMap` in
//! this crate) and only *afterward* deserializes into the typed `Definition`
//! (see `validate_definition_value` and `definition_from_value`). This is
//! deliberate, not incidental: Node's `keys()` helper enforces exact field
//! sets (top level, `destination`, and each mapping) as independent checks
//! that run in source order alongside the other rules. Deserializing into a
//! typed struct first would let serde's own field-set and enum-variant
//! checks fire in JSON key order instead of Node's rule order, so a
//! definition with two problems (e.g. an unsupported `mode` *and* an unknown
//! top-level field) could report the wrong rule's message depending on which
//! key came first in the file. See "fix round 1" in the Task 5 report.
//!
//! `maestro-public-tree-v1` (a third mode Node supports) is out of scope for
//! this crate: any unrecognized `mode` string, including that one, is
//! rejected by rule 3 as an unsupported class/mode pair.

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ordered::canonical_compact_json;
use crate::tree::{Matcher, PathOpts, has_wildcard, safe_path, sha256_hex};
use crate::{Error, Result, invalid};

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]*$").expect("static regex is valid"));
static REPO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$").expect("static regex is valid")
});
static REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_/-]*$").expect("static regex is valid"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Mode {
    #[serde(rename = "copy-v1")]
    CopyV1,
    #[serde(rename = "sdk-assembly-v1")]
    SdkAssemblyV1,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mapping {
    pub source: String,
    pub destination: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Destination {
    pub repository: String,
    pub branch: String,
    pub sync_branch: String,
    pub hold_label: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Definition {
    pub schema_version: u32,
    pub name: String,
    pub class: String,
    pub mode: Mode,
    pub source_repository: String,
    pub visibility: String,
    pub mappings: Vec<Mapping>,
    pub destination: Destination,
    pub destination_owned: Vec<String>,
    pub deletion: String,
    pub provenance: String,
    pub validation: String,
    /// Required for `copy-v1`, forbidden otherwise (rule 1). `#[serde(default)]`
    /// so a missing key deserializes to `None` for both modes, and
    /// `skip_serializing_if` so re-serializing a `sdk-assembly-v1` definition
    /// (e.g. from `validate_definition`, which round-trips through `Value`)
    /// omits the key rather than writing `"outputManaged": null` -- which
    /// would otherwise make `validate_definition_value`'s field-set check see
    /// a key that was never in the original JSON. The conditional presence
    /// per mode is enforced by `value_keys` in `validate_definition_value`,
    /// not by `Option` itself, which accepts absence regardless of `mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_managed: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct LoadedDefinition {
    pub definition: Definition,
    pub raw: Value,
    pub text: String,
}

/// Reads `path`, parses it into a `serde_json::Value`, deserializes and
/// validates it. `text` is the file's exact contents, in the file's own key
/// order; pass it to `definition_digest` for a Node-comparable digest.
pub fn load_definition(
    path: &Path,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<LoadedDefinition> {
    let text = std::fs::read_to_string(path)?;
    let raw: Value = serde_json::from_str(&text)
        .map_err(|e| Error::Invalid(format!("Invalid projection JSON: {e}")))?;
    let loaded = definition_from_value(raw, sdk_inputs)?;
    Ok(LoadedDefinition {
        definition: loaded.definition,
        raw: loaded.raw,
        text,
    })
}

/// Builds a `LoadedDefinition` from an already-parsed `Value`: validates it
/// (on the raw value, in Node's rule order -- see the module doc comment)
/// and only then deserializes it into a `Definition`. Used by
/// `load_definition` and directly by tests. There is no source file in this
/// path, so `text` is reconstructed from `raw` via
/// `serde_json::to_string_pretty` plus a trailing newline, rather than read
/// from disk; its key order is therefore `Value`'s (sorted), not any
/// original file's. A digest computed from this `text` via
/// `definition_digest` is NOT Node-comparable -- it is for tests only, where
/// `definition_from_value` is called directly rather than via
/// `load_definition`.
pub fn definition_from_value(
    raw: Value,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<LoadedDefinition> {
    validate_definition_value(&raw, sdk_inputs)?;
    let definition = finalize_definition(raw.clone())?;
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&raw)
            .map_err(|e| Error::Invalid(format!("Invalid projection JSON: {e}")))?
    );
    Ok(LoadedDefinition {
        definition,
        raw,
        text,
    })
}

/// Deserializes an already-validated raw `Value` into a typed `Definition`.
/// By the time this runs, `validate_definition_value` has already confirmed
/// exact field sets (top level, `destination`, every mapping) and
/// rule-compliant values, so this conversion should always succeed;
/// `#[serde(deny_unknown_fields)]` on the structs remains as a second line
/// of defense, and a residual failure here is treated as unreachable in
/// normal operation rather than given its own diagnostic message.
fn finalize_definition(raw: Value) -> Result<Definition> {
    serde_json::from_value(raw).map_err(|_| Error::Invalid("Invalid projection".into()))
}

/// Ports `keys()` from project.mjs: `value` must be a JSON object whose key
/// set is exactly `expected` (order-independent). Used for the top-level
/// definition, `destination`, and each mapping; the label says which one so
/// the error message names the right thing, matching Node exactly.
fn value_keys(value: &Value, expected: &[&str], label: &str) -> Result<()> {
    let object = value.as_object();
    invalid(object.is_some(), format!("Invalid {label}"))?;
    let object = object.expect("checked by the invalid() call above");
    let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
    actual.sort_unstable();
    let mut expected: Vec<&str> = expected.to_vec();
    expected.sort_unstable();
    invalid(
        actual == expected,
        format!("Unknown or missing {label} fields"),
    )
}

/// Ports `patterns()` from project.mjs: `value` must be a JSON array (and,
/// when `nonempty`, a non-empty one) of strings, each a safe pattern
/// (`safe_path` with `pattern: true`), with no duplicates. A non-string
/// element is treated the same as the array itself being the wrong shape:
/// `Invalid {label}`. Returns the patterns as owned `String`s so callers can
/// use them further (matcher construction, membership checks, and so on).
fn value_patterns(value: &Value, label: &str, nonempty: bool) -> Result<Vec<String>> {
    let array = value.as_array();
    invalid(
        array.is_some_and(|a| !nonempty || !a.is_empty()),
        format!("Invalid {label}"),
    )?;
    let array = array.expect("checked by the invalid() call above");
    let mut patterns = Vec::with_capacity(array.len());
    for item in array {
        let pattern = item
            .as_str()
            .ok_or_else(|| Error::Invalid(format!("Invalid {label}")))?;
        safe_path(
            pattern,
            PathOpts {
                pattern: true,
                root: false,
            },
        )?;
        patterns.push(pattern.to_string());
    }
    let unique: HashSet<&str> = patterns.iter().map(String::as_str).collect();
    invalid(unique.len() == patterns.len(), format!("Duplicate {label}"))?;
    Ok(patterns)
}

/// A mapping's fields, read off the raw `Value` once its shape has already
/// passed `value_keys`/`value_patterns`. Collected during rule 9 so rules 10
/// and 11 (which also iterate the mappings) don't have to re-read `Value`.
struct MappingFields {
    source: String,
    destination: String,
    include: Vec<String>,
    exclude: Vec<String>,
}

/// Validates a definition's raw JSON `Value` against the rules transcribed
/// from `validateDefinition` in project.mjs, checked in Node's exact order
/// so that, for any invalid input, the first rule to fail here is the same
/// rule that fails first in Node, with the same message. Reads fields
/// straight off `Value` (via `as_str`/`as_i64`/`as_array`/`as_object`)
/// rather than a typed `Definition`, mirroring the way Node's `===`/regex
/// tests simply evaluate to `false` on a wrong-shaped value instead of
/// throwing a type error.
fn validate_definition_value(
    raw: &Value,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<()> {
    // Rule 1: top-level field set is exact. `outputManaged` is required only
    // for copy-v1, checked here against the raw "mode" string before
    // anything else -- exactly Node's
    // `if (definition?.mode === "copy-v1") definitionFields.push("outputManaged")`.
    let mut definition_fields = vec![
        "schemaVersion",
        "name",
        "class",
        "mode",
        "sourceRepository",
        "visibility",
        "mappings",
        "destination",
        "destinationOwned",
        "deletion",
        "provenance",
        "validation",
    ];
    if raw.get("mode").and_then(Value::as_str) == Some("copy-v1") {
        definition_fields.push("outputManaged");
    }
    value_keys(raw, &definition_fields, "projection")?;

    let schema_version_ok = raw.get("schemaVersion").and_then(Value::as_i64) == Some(1);
    let name = raw.get("name").and_then(Value::as_str).unwrap_or_default();
    let class = raw.get("class").and_then(Value::as_str).unwrap_or_default();
    let mode = raw.get("mode").and_then(Value::as_str).unwrap_or_default();
    let source_repository = raw
        .get("sourceRepository")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let visibility = raw
        .get("visibility")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let deletion = raw
        .get("deletion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let provenance = raw
        .get("provenance")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let validation = raw
        .get("validation")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // Rule 2
    invalid(
        schema_version_ok && NAME.is_match(name),
        "Unsupported projection identity/schema",
    )?;

    // Rule 3
    invalid(
        (class == "source-tree" && mode == "copy-v1")
            || (class == "generated-sdk" && mode == "sdk-assembly-v1"),
        "Unsupported projection class/mode",
    )?;

    // Rule 4
    invalid(
        REPO.is_match(source_repository) && matches!(visibility, "public" | "private" | "customer"),
        "Invalid source/visibility",
    )?;

    // Rule 5 (destination field set, then destination.repository)
    let destination = raw.get("destination").cloned().unwrap_or_default();
    value_keys(
        &destination,
        &["repository", "branch", "syncBranch", "holdLabel"],
        "destination",
    )?;
    let destination_repository = destination
        .get("repository")
        .and_then(Value::as_str)
        .unwrap_or_default();
    invalid(
        REPO.is_match(destination_repository) && destination_repository != source_repository,
        "Invalid destination repository",
    )?;

    // Rule 6
    for field in ["branch", "syncBranch"] {
        let ok = destination
            .get(field)
            .and_then(Value::as_str)
            .map(|value| REF.is_match(value) && !value.contains("//") && !value.ends_with('/'))
            .unwrap_or(false);
        invalid(ok, "Invalid destination ref")?;
    }

    // Rule 7
    let branch = destination
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sync_branch = destination
        .get("syncBranch")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let hold_label = destination
        .get("holdLabel")
        .and_then(Value::as_str)
        .unwrap_or_default();
    invalid(
        branch != sync_branch && hold_label == "sync-hold",
        "Publication must use a separate PR branch and sync-hold",
    )?;

    // Rule 8
    let destination_owned_value = raw.get("destinationOwned").cloned().unwrap_or_default();
    let destination_owned = value_patterns(&destination_owned_value, "ownership", false)?;
    safe_path(provenance, PathOpts::default())?;
    let ownership = Matcher::new(&destination_owned)?;
    invalid(
        !ownership.matches(provenance),
        "Provenance cannot be destination-owned",
    )?;

    // Rule 9
    let mappings_value = raw.get("mappings").cloned().unwrap_or_default();
    let mappings_array = mappings_value.as_array();
    invalid(
        mappings_array.is_some_and(|a| !a.is_empty()),
        "Mappings are required",
    )?;
    let mappings_array = mappings_array.expect("checked by the invalid() call above");
    let mut mapping_fields = Vec::with_capacity(mappings_array.len());
    for mapping in mappings_array {
        value_keys(
            mapping,
            &["source", "destination", "include", "exclude"],
            "mapping",
        )?;
        let source = mapping
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mapping_destination = mapping
            .get("destination")
            .and_then(Value::as_str)
            .unwrap_or_default();
        safe_path(
            source,
            PathOpts {
                pattern: false,
                root: true,
            },
        )?;
        safe_path(
            mapping_destination,
            PathOpts {
                pattern: false,
                root: true,
            },
        )?;
        let include_value = mapping.get("include").cloned().unwrap_or_default();
        let include = value_patterns(&include_value, "includes", true)?;
        let exclude_value = mapping.get("exclude").cloned().unwrap_or_default();
        let exclude = value_patterns(&exclude_value, "excludes", false)?;
        invalid(
            !include.iter().any(|item| item == "**"),
            "Allowlist must name explicit files or subtrees",
        )?;
        if source == "." {
            invalid(
                include
                    .iter()
                    .all(|item| !has_wildcard(item) && !item.contains('/')),
                "Root mappings must name exact root files",
            )?;
        }
        mapping_fields.push(MappingFields {
            source: source.to_string(),
            destination: mapping_destination.to_string(),
            include,
            exclude,
        });
    }

    if name == "api" {
        for (source, expected) in [
            ("proto", "deixicpublic/v1/sdk.proto"),
            ("gen/openapi", "deixicpublic/v1/sdk.openapi.yaml"),
        ] {
            invalid(
                mapping_fields
                    .iter()
                    .filter(|mapping| mapping.source == source)
                    .count()
                    == 1
                    && mapping_fields.iter().any(|mapping| {
                        mapping.source == source
                            && mapping.include.len() == 1
                            && mapping.include[0] == expected
                            && mapping.exclude.is_empty()
                    }),
                format!("Public API {source} projection must contain only {expected}"),
            )?;
        }
    }

    if mode == "sdk-assembly-v1" {
        // Rule 10
        let policy_inputs = sdk_inputs(name);
        invalid(
            policy_inputs.is_some()
                && source_repository == "dx-corp/mono"
                && destination_repository == format!("dx-corp/{name}")
                && visibility == "public"
                && validation == "sdk-standalone-v1"
                && deletion == "owned-paths",
            "Unknown SDK assembly policy",
        )?;
        let mut inputs = Vec::new();
        for mapping in &mapping_fields {
            invalid(
                mapping.destination == "." && mapping.exclude.is_empty(),
                "SDK mapping cannot alter the registered assembly",
            )?;
            for path in &mapping.include {
                safe_path(path, PathOpts::default())?;
                inputs.push(format!("{}/{}", mapping.source, path));
            }
        }
        inputs.sort();
        let mut policy_inputs = policy_inputs.unwrap_or_default();
        policy_inputs.sort();
        invalid(
            inputs == policy_inputs,
            "SDK input allowlist differs from registered policy",
        )?;
    } else {
        // Rule 11 (mode == "copy-v1"; rule 3 admits no other value here)
        invalid(
            deletion == "owned-paths" && validation == "tree-v1",
            "Unsupported copy policy",
        )?;
        let output_managed_value = raw.get("outputManaged").cloned().unwrap_or_default();
        let output_managed = value_patterns(&output_managed_value, "managed outputs", true)?;
        invalid(
            output_managed
                .iter()
                .all(|path| !has_wildcard(path) || path.ends_with("/**")),
            "Managed outputs must name explicit files or subtrees",
        )?;
        let managed = Matcher::new(&output_managed)?;
        for mapping in &mapping_fields {
            for include in &mapping.include {
                invalid(
                    !has_wildcard(include) || include.ends_with("/**"),
                    "Copy includes must name exact files or subtrees",
                )?;
                let relative = include.strip_suffix("/**").unwrap_or(include);
                let output = if mapping.destination == "." {
                    relative.to_string()
                } else {
                    format!("{}/{}", mapping.destination, relative)
                };
                let probe_ok = !include.ends_with("/**")
                    || managed.matches(&format!("{output}/__managed_probe__"));
                invalid(
                    managed.matches(&output) && probe_ok,
                    format!("Copy output is outside its stable managed boundary: {output}"),
                )?;
            }
        }
    }

    Ok(())
}

/// Typed re-validation of an already-built `Definition`, for callers (later
/// tasks) that hold one rather than a raw `Value`. Re-serializes to a
/// `Value` and delegates to `validate_definition_value` rather than
/// duplicating the rule bodies, so the two entry points can never disagree
/// on a message: this is the smaller diff, at the cost of one JSON
/// round-trip per call. `output_managed`'s `skip_serializing_if` keeps that
/// round-trip faithful to the original shape for both modes (see the field's
/// doc comment).
pub fn validate_definition(
    definition: &Definition,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<()> {
    let raw = serde_json::to_value(definition)
        .map_err(|e| Error::Invalid(format!("Invalid projection JSON: {e}")))?;
    validate_definition_value(&raw, sdk_inputs)
}

/// `sha256_hex` of `text` reparsed and recompacted through `OrderedValue`,
/// matching Node's `sha256(JSON.stringify(JSON.parse(text)))`: object keys
/// stay in `text`'s order, not a re-serialization in a different field
/// order. Callers that hold a `LoadedDefinition` pass `&loaded.text`.
pub fn definition_digest(text: &str) -> Result<String> {
    Ok(sha256_hex(canonical_compact_json(text)?.as_bytes()))
}

/// Ports `projectionInputRoots` from project.mjs (the `sdk-assembly-v1` and
/// `copy-v1` branches only; `maestro-public-tree-v1` is out of scope).
pub fn projection_input_roots(
    definition: &Definition,
    sdk_inputs: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Vec<String> {
    match definition.mode {
        Mode::SdkAssemblyV1 => sdk_inputs(&definition.name).unwrap_or_default(),
        Mode::CopyV1 => {
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            for mapping in &definition.mappings {
                for pattern in &mapping.include {
                    let wildcard = pattern.find(['*', '?', '[', ']']);
                    let prefix = match wildcard {
                        None => pattern.clone(),
                        Some(idx) => {
                            let before = &pattern[..idx];
                            match before.rfind('/') {
                                Some(slash) => before[..slash].to_string(),
                                None => String::new(),
                            }
                        }
                    };
                    let root = if mapping.source == "." {
                        prefix
                    } else if !prefix.is_empty() {
                        format!("{}/{}", mapping.source, prefix)
                    } else {
                        mapping.source.clone()
                    };
                    if seen.insert(root.clone()) {
                        out.push(root);
                    }
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/definitions")
            .join(format!("{name}.json"))
    }

    fn sdk_inputs(name: &str) -> Option<Vec<String>> {
        // Task 10 replaces this with the real policy table; the fixture's include list is used here.
        (name == "deixic-python").then(|| {
            let raw: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap();
            let mut inputs: Vec<String> = raw["mappings"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|m| {
                    let source = m["source"].as_str().unwrap().to_owned();
                    m["include"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(move |i| format!("{source}/{}", i.as_str().unwrap()))
                })
                .collect();
            inputs.sort();
            inputs
        })
    }

    #[test]
    #[cfg_attr(not(feature = "mono-fixtures"), ignore)]
    fn approved_definitions_load_and_digest_like_node() {
        let api = load_definition(&fixture("api"), &sdk_inputs).unwrap();
        assert_eq!(api.definition.name, "api");
        assert!(matches!(api.definition.mode, Mode::CopyV1));
        // Recorded from: node -e 'import {definitionDigest} from "./scripts/projections/project.mjs"; ...'
        insta::assert_snapshot!(definition_digest(&api.text).unwrap());
    }

    #[test]
    #[cfg_attr(not(feature = "mono-fixtures"), ignore)]
    fn malformed_manifests_and_executable_configuration_are_rejected() {
        let text = std::fs::read_to_string(fixture("api")).unwrap();
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["command"] = serde_json::json!("rm -rf /");
        assert!(definition_from_value(raw.clone(), &sdk_inputs).is_err());
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["destination"]["holdLabel"] = serde_json::json!("other");
        assert!(definition_from_value(raw, &sdk_inputs).is_err());
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["mappings"][0]["include"] = serde_json::json!(["**"]);
        assert!(definition_from_value(raw, &sdk_inputs).is_err());
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["mode"] = serde_json::json!("maestro-public-tree-v1");
        assert!(definition_from_value(raw, &sdk_inputs).is_err());
    }

    #[test]
    #[cfg_attr(not(feature = "mono-fixtures"), ignore)]
    fn root_file_mappings_cannot_expand_into_the_whole_source_repository() {
        let text = std::fs::read_to_string(fixture("api")).unwrap();
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["mappings"][1]["include"] = serde_json::json!(["proto/**"]);
        let err = definition_from_value(raw, &sdk_inputs)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Root mappings must name exact root files");
    }

    #[test]
    #[cfg_attr(not(feature = "mono-fixtures"), ignore)]
    fn copy_outputs_must_sit_inside_the_managed_boundary() {
        let text = std::fs::read_to_string(fixture("api")).unwrap();
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["outputManaged"] = serde_json::json!(["README.md"]);
        let err = definition_from_value(raw, &sdk_inputs)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Copy output is outside its stable managed boundary: "));
    }

    #[test]
    #[cfg_attr(not(feature = "mono-fixtures"), ignore)]
    fn input_roots_strip_wildcards_to_their_directory() {
        let api = load_definition(&fixture("api"), &sdk_inputs).unwrap();
        let roots = projection_input_roots(&api.definition, &sdk_inputs);
        assert!(roots.contains(&"distributions/api/README.md".to_string()));
        assert!(roots.contains(&"LICENSE".to_string()));
    }

    #[test]
    #[cfg_attr(not(feature = "mono-fixtures"), ignore)]
    fn field_set_violations_win_over_mode_violations() {
        // A definition that is wrong in two ways at once (unsupported mode,
        // and an unknown top-level key appended after `mode`) must report
        // the field-set violation, because Node's `keys()` check for the
        // top-level object runs before the class/mode check, regardless of
        // where in the JSON the offending keys sit.
        let text = std::fs::read_to_string(fixture("api")).unwrap();
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["mode"] = serde_json::json!("maestro-public-tree-v1");
        raw["zzz_extra"] = serde_json::json!(true);
        let err = definition_from_value(raw, &sdk_inputs)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Unknown or missing projection fields");
    }

    #[test]
    #[cfg_attr(not(feature = "mono-fixtures"), ignore)]
    fn nested_field_sets_use_their_own_labels() {
        let text = std::fs::read_to_string(fixture("api")).unwrap();

        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["destination"]["extra"] = serde_json::json!(1);
        let err = definition_from_value(raw, &sdk_inputs)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Unknown or missing destination fields");

        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["mappings"][0]["extra"] = serde_json::json!(1);
        let err = definition_from_value(raw, &sdk_inputs)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Unknown or missing mapping fields");

        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw["destinationOwned"] = serde_json::json!("not-an-array");
        let err = definition_from_value(raw, &sdk_inputs)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Invalid ownership");
    }
}
