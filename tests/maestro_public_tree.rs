//! Byte and mode equivalence with the existing Node Code mirror projector.
//! The oracle and real source tree are Mono-only fixtures; standalone tests
//! retain no Node runtime dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

use capobara::build::{BuildInput, Built, build_projection};
use capobara::definition::{LoadedDefinition, load_definition};
use capobara::tree::{apply_tree, sha256_hex};
use serde_json::Value;

const SOURCE_SHA: &str = "1111111111111111111111111111111111111111";
const PRIOR_BASE: &str = "2222222222222222222222222222222222222222";
const TOOL_DIGEST: &str = "3333333333333333333333333333333333333333333333333333333333333333";

const NODE_ORACLE: &str = r#"
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
const { buildProjection } = await import(pathToFileURL(process.env.CAPOBARA_ORACLE));
const plan = buildProjection({
  definition: JSON.parse(readFileSync(process.env.CAPOBARA_DEFINITION, 'utf8')),
  sourceRoot: process.env.CAPOBARA_SOURCE,
  targetRoot: process.env.CAPOBARA_TARGET,
  sourceSha: process.env.CAPOBARA_SOURCE_SHA,
  priorProjectedBase: process.env.CAPOBARA_PRIOR_BASE,
  toolDigest: process.env.CAPOBARA_TOOL_DIGEST,
  publicationEligible: true,
});
console.log(JSON.stringify({
  entries: Object.fromEntries([...plan.entries].map(([path, entry]) => [path, {
    mode: entry.mode,
    sha256: createHash('sha256').update(entry.content).digest('hex'),
  }])),
  copiedPaths: plan.copiedPaths,
  deletedPaths: plan.deletedPaths,
  copiedCount: plan.copiedCount,
  deletedCount: plan.deletedCount,
  publicPackageName: plan.publicPackageName,
  provenance: plan.provenance,
}));
"#;

fn mono_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .to_path_buf()
}

fn node_output(source: &Path, target: &Path) -> std::process::Output {
    #[allow(
        clippy::disallowed_methods,
        reason = "Mono-only equivalence test invokes the independent Node projector oracle"
    )]
    Command::new("node")
        .args(["--input-type=module", "-e", NODE_ORACLE])
        .env(
            "CAPOBARA_ORACLE",
            source.join("scripts/projections/project.mjs"),
        )
        .env(
            "CAPOBARA_DEFINITION",
            source.join("config/projections/deixic-code.json"),
        )
        .env("CAPOBARA_SOURCE", source)
        .env("CAPOBARA_TARGET", target)
        .env("CAPOBARA_SOURCE_SHA", SOURCE_SHA)
        .env("CAPOBARA_PRIOR_BASE", PRIOR_BASE)
        .env("CAPOBARA_TOOL_DIGEST", TOOL_DIGEST)
        .output()
        .unwrap()
}

fn node_plan(source: &Path, target: &Path) -> Value {
    let output = node_output(source, target);
    assert!(
        output.status.success(),
        "Node oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("Node oracle must emit one JSON plan")
}

fn equivalent_plan(source: &Path, target: &Path, loaded: &LoadedDefinition) -> Built {
    let expected = node_plan(source, target);
    let actual = build_projection(
        BuildInput {
            definition: &loaded.definition,
            definition_text: &loaded.text,
            source_root: source,
            target_root: target,
            source_sha: SOURCE_SHA,
            prior_projected_base: PRIOR_BASE,
            tool_digest: TOOL_DIGEST,
            publication_eligible: true,
        },
        &|_, _| panic!("Code projection must not invoke SDK assembly"),
    )
    .unwrap();
    let expected_entries = expected["entries"].as_object().unwrap();
    let actual_paths: std::collections::BTreeSet<&str> =
        actual.plan.entries.keys().map(String::as_str).collect();
    let expected_paths: std::collections::BTreeSet<&str> =
        expected_entries.keys().map(String::as_str).collect();
    assert_eq!(
        actual_paths, expected_paths,
        "Code projection output paths differ"
    );
    for (path, entry) in &actual.plan.entries {
        let expected_entry = &expected_entries[path];
        assert_eq!(
            Value::from(entry.mode),
            expected_entry["mode"],
            "file mode differs: {path}"
        );
        assert_eq!(
            Value::from(sha256_hex(&entry.content)),
            expected_entry["sha256"],
            "file bytes differ: {path}"
        );
    }
    assert_eq!(
        serde_json::to_value(&actual.plan.copied_paths).unwrap(),
        expected["copiedPaths"],
        "copied paths differ"
    );
    assert_eq!(
        serde_json::to_value(&actual.plan.deleted_paths).unwrap(),
        expected["deletedPaths"],
        "deleted paths differ"
    );
    assert_eq!(
        Value::from(actual.plan.copied_count()),
        expected["copiedCount"]
    );
    assert_eq!(
        Value::from(actual.plan.deleted_count()),
        expected["deletedCount"]
    );
    assert_eq!(
        serde_json::to_value(&actual.public_package_name).unwrap(),
        expected["publicPackageName"]
    );
    assert_eq!(
        serde_json::to_value(&actual.provenance).unwrap(),
        expected["provenance"],
        "provenance, including content digest, differs"
    );
    actual
}

fn write(target: &Path, path: &str, content: &[u8]) {
    let absolute = target.join(path);
    std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
    std::fs::write(absolute, content).unwrap();
}

#[test]
#[cfg_attr(
    not(feature = "mono-fixtures"),
    ignore = "requires the Mono Code source tree and Node reference projector"
)]
fn native_code_mirror_matches_node_bytes_modes_deletions_and_idempotence() {
    let source = mono_root();
    let definition = load_definition(&source.join("config/projections/deixic-code.json"), &|_| {
        None
    })
    .unwrap();
    let target = tempfile::tempdir().unwrap();
    let owned_paths = [
        ".github/workflows/public-owned.yml",
        ".github/actionlint.yaml",
        ".github/public-release-mirror.exclude",
        "scripts/validate-public-package-deps.js",
        "test/scripts/validate-public-package-deps.test.ts",
    ];
    for path in owned_paths {
        write(target.path(), path, b"destination-owned fixture\n");
    }
    let stale_paths = [
        "README.md",
        "docs/planning/private-fixture.md",
        "packages/removed/file.txt",
        "packages/old/gha-creds-fixture.json",
        ".agents/memory.md",
        ".context/state.json",
        "scripts/prepare-public-release-mirror.mjs",
        ".github/actions/configure-private-cargo-git/action.yml",
    ];
    for path in stale_paths {
        write(target.path(), path, b"stale target fixture\n");
    }

    let first = equivalent_plan(&source, target.path(), &definition);
    assert!(!first.plan.copied_paths.is_empty());
    for path in stale_paths.iter().filter(|path| **path != "README.md") {
        assert!(
            first
                .plan
                .deleted_paths
                .iter()
                .any(|deleted| deleted.as_str() == *path),
            "stale private path must be deleted: {path}"
        );
    }
    apply_tree(target.path(), &first.plan).unwrap();
    for path in owned_paths {
        assert_eq!(
            std::fs::read(target.path().join(path)).unwrap(),
            b"destination-owned fixture\n",
            "destination-owned file changed: {path}"
        );
    }

    let second = equivalent_plan(&source, target.path(), &definition);
    assert!(second.plan.copied_paths.is_empty());
    assert!(second.plan.deleted_paths.is_empty());
    assert_eq!(
        first.provenance.content_digest,
        second.provenance.content_digest
    );
}

#[test]
#[cfg_attr(
    not(feature = "mono-fixtures"),
    ignore = "requires the Mono Code source tree and Node reference projector"
)]
fn credentials_inside_destination_owned_paths_fail_closed_like_node() {
    let source = mono_root();
    let loaded = load_definition(&source.join("config/projections/deixic-code.json"), &|_| {
        None
    })
    .unwrap();
    let target = tempfile::tempdir().unwrap();
    let credential = ".github/workflows/gha-creds-fixture.json";
    write(target.path(), credential, b"credential fixture\n");
    let expected = node_output(&source, target.path());
    assert!(!expected.status.success());
    let message = format!("Projection would delete destination-owned path: {credential}");
    assert!(String::from_utf8_lossy(&expected.stderr).contains(&message));
    let actual = build_projection(
        BuildInput {
            definition: &loaded.definition,
            definition_text: &loaded.text,
            source_root: &source,
            target_root: target.path(),
            source_sha: SOURCE_SHA,
            prior_projected_base: PRIOR_BASE,
            tool_digest: TOOL_DIGEST,
            publication_eligible: true,
        },
        &|_, _| panic!("Code projection must not invoke SDK assembly"),
    )
    .unwrap_err();
    assert!(actual.to_string().contains(&message), "{actual}");
    assert_eq!(
        std::fs::read(target.path().join(credential)).unwrap(),
        b"credential fixture\n"
    );
    assert!(!target.path().join("package.json").exists());
}
