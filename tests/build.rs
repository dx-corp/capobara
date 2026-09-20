mod support;
use std::fs;
use std::path::Path;

use capobara::build::{BuildInput, build_projection, check_public_entry};
use capobara::definition::definition_from_value;
use capobara::tree::{Entry, apply_tree};

const SHA: &str = "1111111111111111111111111111111111111111";
const BASE: &str = "2222222222222222222222222222222222222222";
const TOOL: &str = "3333333333333333333333333333333333333333333333333333333333333333";

fn definition() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1, "name": "sample", "class": "source-tree", "mode": "copy-v1",
        "sourceRepository": "dx-corp/mono", "visibility": "public",
        "mappings": [
            {"source": "pkg", "destination": ".", "include": ["src/**", "README.md"], "exclude": ["src/secret/**"]},
            {"source": ".", "destination": ".", "include": ["LICENSE"], "exclude": []}
        ],
        "destination": {"repository": "dx-corp/sample", "branch": "main", "syncBranch": "sync/mono-projection", "holdLabel": "sync-hold"},
        "destinationOwned": [".github/**", "SECURITY.md"],
        "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
        "outputManaged": ["src/**", "README.md", "LICENSE"]
    })
}

fn no_sdk(_: &str) -> Option<Vec<String>> {
    None
}

fn build(
    source: &Path,
    target: &Path,
    raw: serde_json::Value,
) -> capobara::Result<capobara::build::Built> {
    let loaded = definition_from_value(raw, &no_sdk)?;
    build_projection(
        BuildInput {
            definition: &loaded.definition,
            definition_text: &loaded.text,
            source_root: source,
            target_root: target,
            source_sha: SHA,
            prior_projected_base: BASE,
            tool_digest: TOOL,
            publication_eligible: true,
        },
        &|_, _| unreachable!("copy-v1 never assembles"),
    )
}

#[test]
fn deterministic_mapping_stale_deletion_and_destination_ownership() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    for (p, b) in [
        ("pkg/src/a.rs", "a"),
        ("pkg/src/secret/k", "k"),
        ("pkg/README.md", "r"),
        ("pkg/ignored.txt", "i"),
        ("LICENSE", "l"),
    ] {
        let full = source.path().join(p);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, b).unwrap();
    }
    for (p, b) in [
        ("src/stale.rs", "old"),
        (".github/workflows/ci.yml", "owned"),
        ("SECURITY.md", "owned"),
        ("unrelated.txt", "kept"),
    ] {
        let full = target.path().join(p);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, b).unwrap();
    }
    let built = build(source.path(), target.path(), definition()).unwrap();
    assert_eq!(
        built.plan.copied_paths,
        vec![
            ".repository-projection.json",
            "LICENSE",
            "README.md",
            "src/a.rs"
        ]
    );
    assert_eq!(built.plan.deleted_paths, vec!["src/stale.rs"]);
    assert!(!built.plan.entries.contains_key("src/secret/k"));
    apply_tree(target.path(), &built.plan).unwrap();
    assert_eq!(
        fs::read_to_string(target.path().join(".github/workflows/ci.yml")).unwrap(),
        "owned"
    );
    assert_eq!(
        fs::read_to_string(target.path().join("unrelated.txt")).unwrap(),
        "kept"
    );
    let receipt: serde_json::Value = serde_json::from_slice(
        &fs::read(target.path().join(".repository-projection.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["sourceSha"], SHA);
    assert_eq!(receipt["contentDigest"], built.provenance.content_digest);
    let again = build(source.path(), target.path(), definition()).unwrap();
    assert!(again.plan.copied_paths.is_empty() && again.plan.deleted_paths.is_empty());
    assert_eq!(again.provenance, built.provenance);
}

#[test]
fn ownership_conflicts_and_overlapping_mappings_fail_before_a_write() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("pkg/src")).unwrap();
    fs::write(source.path().join("pkg/src/a.rs"), "a").unwrap();
    fs::write(source.path().join("pkg/README.md"), "r").unwrap();
    fs::write(source.path().join("LICENSE"), "l").unwrap();
    let mut raw = definition();
    raw["destinationOwned"] = serde_json::json!(["README.md"]);
    let err = build(source.path(), target.path(), raw)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "Projection would overwrite destination-owned path: README.md"
    );
    let mut raw = definition();
    raw["mappings"].as_array_mut().unwrap().push(serde_json::json!({"source": "pkg", "destination": ".", "include": ["README.md"], "exclude": []}));
    let err = build(source.path(), target.path(), raw)
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Overlapping mappings: README.md");
    assert!(fs::read_dir(target.path()).unwrap().next().is_none());
}

#[test]
fn private_files_credentials_and_keys_cannot_cross_a_public_boundary() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("pkg/src")).unwrap();
    fs::write(source.path().join("pkg/README.md"), "r").unwrap();
    fs::write(source.path().join("LICENSE"), "l").unwrap();
    fs::write(source.path().join("pkg/src/.env"), "SECRET=1").unwrap();
    let err = build(source.path(), target.path(), definition())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Private path in public projection: src/.env");
    fs::remove_file(source.path().join("pkg/src/.env")).unwrap();
    fs::write(source.path().join("pkg/src/.env.example"), "SECRET=").unwrap();
    fs::write(
        source.path().join("pkg/src/key.pem"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();
    let err = build(source.path(), target.path(), definition())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Private key in public projection: src/key.pem");

    // Segment-boundary cases for the private-path rule (task-7 review
    // addendum). The test definition's outputManaged is `src/**`,
    // `README.md`, `LICENSE`, so every probe that must flow through a full
    // build lives under `pkg/src/`; each case starts from a freshly emptied
    // `pkg/src` so earlier probe files never leak into a later assertion.
    let reset_src = || {
        let _ = fs::remove_dir_all(source.path().join("pkg/src"));
        fs::create_dir_all(source.path().join("pkg/src")).unwrap();
    };

    // src/.env.example alone is allowed: the sole exception, and only when
    // it is the path's last segment.
    reset_src();
    fs::write(source.path().join("pkg/src/.env.example"), "x").unwrap();
    build(source.path(), target.path(), definition()).unwrap();

    // src/.env.example/x is private: the exception is for the exact last
    // segment, not for paths beneath a directory of that name.
    reset_src();
    fs::create_dir_all(source.path().join("pkg/src/.env.example")).unwrap();
    fs::write(source.path().join("pkg/src/.env.example/x"), "x").unwrap();
    let err = build(source.path(), target.path(), definition())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Private path in public projection: src/.env.example/x");

    // src/.env.examples is private: not an exact match for the exception.
    reset_src();
    fs::write(source.path().join("pkg/src/.env.examples"), "x").unwrap();
    let err = build(source.path(), target.path(), definition())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Private path in public projection: src/.env.examples");

    // src/.env.example.bak is private: same reason.
    reset_src();
    fs::write(source.path().join("pkg/src/.env.example.bak"), "x").unwrap();
    let err = build(source.path(), target.path(), definition())
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "Private path in public projection: src/.env.example.bak"
    );

    // src/.envrc is allowed: it neither is the exact ".env" segment nor
    // starts with the ".env." prefix.
    reset_src();
    fs::write(source.path().join("pkg/src/.envrc"), "x").unwrap();
    build(source.path(), target.path(), definition()).unwrap();

    // src/id_rsa.pub is allowed: only the exact "id_rsa" segment is private.
    reset_src();
    fs::write(source.path().join("pkg/src/id_rsa.pub"), "x").unwrap();
    build(source.path(), target.path(), definition()).unwrap();

    // src/id_rsa is private.
    reset_src();
    fs::write(source.path().join("pkg/src/id_rsa"), "x").unwrap();
    let err = build(source.path(), target.path(), definition())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Private path in public projection: src/id_rsa");

    // src/gha-creds-abc.json is private.
    reset_src();
    fs::write(source.path().join("pkg/src/gha-creds-abc.json"), "x").unwrap();
    let err = build(source.path(), target.path(), definition())
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "Private path in public projection: src/gha-creds-abc.json"
    );

    // .agents/x is private, but it can never reach a full build here: it
    // matches no mapping's include list and outputManaged does not cover it,
    // so it is never a candidate entry. check_public_entry enforces the rule
    // directly instead, independent of the managed-boundary check.
    let def = definition_from_value(definition(), &no_sdk)
        .unwrap()
        .definition;
    let entry = Entry {
        content: b"x".to_vec(),
        mode: 0o644,
    };
    let err = check_public_entry(".agents/x", &entry, &def)
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Private path in public projection: .agents/x");
}

#[test]
fn source_and_destination_symlinks_fail_closed_without_touching_outside_files() {
    use std::os::unix::fs::symlink;
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("victim"), "v").unwrap();
    fs::create_dir_all(source.path().join("pkg/src")).unwrap();
    fs::write(source.path().join("pkg/README.md"), "r").unwrap();
    fs::write(source.path().join("LICENSE"), "l").unwrap();
    symlink(
        outside.path().join("victim"),
        source.path().join("pkg/src/link.rs"),
    )
    .unwrap();
    assert!(build(source.path(), target.path(), definition()).is_err());
    fs::remove_file(source.path().join("pkg/src/link.rs")).unwrap();
    symlink(outside.path(), target.path().join("src")).unwrap();
    assert!(build(source.path(), target.path(), definition()).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("victim")).unwrap(),
        "v"
    );
}
