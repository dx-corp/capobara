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
use std::process::Command;
use support::Repo;

fn capobara() -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test executes the capobara binary"
    )]
    Command::new(cargo_bin!("capobara"))
}

fn definition_json(name: &str) -> String {
    serde_json::json!({
        "schemaVersion": 1, "name": name, "class": "source-tree", "mode": "copy-v1",
        "sourceRepository": "dx-corp/mono", "visibility": "public",
        "mappings": [{"source": "pkg", "destination": ".", "include": ["src/**"], "exclude": []}],
        "destination": {"repository": format!("dx-corp/{name}"), "branch": "main", "syncBranch": "sync/mono-projection", "holdLabel": "sync-hold"},
        "destinationOwned": [".github/**", "SECURITY.md"],
        "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
        "outputManaged": ["src/**"]
    }).to_string()
}

#[test]
fn production_cli_verifies_committed_definition_reports_drift_and_rejects_malformed_prior_provenance()
 {
    let source = Repo::init("https://github.com/dx-corp/mono.git");
    source.write(
        "config/projections/sample.json",
        definition_json("sample").as_bytes(),
    );
    source.write("pkg/src/a.rs", b"a");
    source.write(
        "rust/tools/capobara/src/lib.rs",
        b"// stand-in for the crate tree",
    );
    let sha = source.commit("source");
    let tree_id = source
        .git(&["rev-parse", "HEAD:rust/tools/capobara"])
        .trim()
        .to_owned();
    let target = Repo::init("https://github.com/dx-corp/sample.git");
    target.write("SECURITY.md", b"owned");
    let base = target.commit("destination");
    target.set_remote_main(&base);
    let definition = source.path().join("config/projections/sample.json");
    let run = |cmd: &str| {
        capobara()
            .env("CAPOBARA_TREE_ID_OVERRIDE", &tree_id)
            .args([cmd, "--definition"])
            .arg(&definition)
            .arg("--source")
            .arg(source.path())
            .arg("--source-sha")
            .arg(&sha)
            .arg("--target")
            .arg(target.path())
            .output()
            .unwrap()
    };
    let check = run("check");
    assert_eq!(
        check.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let apply = run("apply");
    assert_eq!(
        apply.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(std::fs::read(target.path().join("src/a.rs")).unwrap(), b"a");
    let check = run("check");
    assert_eq!(check.status.code(), Some(0));
    let verify = run("verify");
    assert_eq!(verify.status.code(), Some(0));
    // Tamper with the stored receipt: verify fails, check reports drift on the receipt.
    let receipt = target.path().join(".repository-projection.json");
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    let mut stored = original.clone();
    stored["contentDigest"] = serde_json::json!("0".repeat(64));
    std::fs::write(
        &receipt,
        format!("{}\n", serde_json::to_string_pretty(&stored).unwrap()),
    )
    .unwrap();
    let verify = run("verify");
    assert_eq!(verify.status.code(), Some(2));
    // A non-object receipt fails Node's `keys()` shape check before the key
    // set is even looked at.
    std::fs::write(&receipt, "[]\n").unwrap();
    let check = run("check");
    assert_eq!(check.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&check.stderr).contains("Invalid stored provenance"));
    // A wrong-typed identity field reaches the identity-mismatch check, not
    // a deserialize failure.
    let mut wrong_type = original.clone();
    wrong_type["schemaVersion"] = serde_json::json!("1");
    std::fs::write(
        &receipt,
        format!("{}\n", serde_json::to_string_pretty(&wrong_type).unwrap()),
    )
    .unwrap();
    let check = run("check");
    assert_eq!(check.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&check.stderr).contains("Stored provenance identity mismatch"));
    // A malformed digest reaches the malformed check, not the identity check.
    let mut malformed = original.clone();
    malformed["contentDigest"] = serde_json::json!("zz");
    std::fs::write(
        &receipt,
        format!("{}\n", serde_json::to_string_pretty(&malformed).unwrap()),
    )
    .unwrap();
    let check = run("check");
    assert_eq!(check.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&check.stderr).contains("Malformed stored provenance"));
    // Restore the tampered-content-digest receipt before continuing.
    std::fs::write(
        &receipt,
        format!("{}\n", serde_json::to_string_pretty(&stored).unwrap()),
    )
    .unwrap();
    stored["extra"] = serde_json::json!(true);
    std::fs::write(
        &receipt,
        format!("{}\n", serde_json::to_string_pretty(&stored).unwrap()),
    )
    .unwrap();
    let check = run("check");
    assert_eq!(check.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&check.stderr).contains("stored provenance"));
    // A definition that differs from the committed one is rejected outside draft mode.
    std::fs::write(
        &definition,
        definition_json("sample").replace("\"exclude\":[]", "\"exclude\":[\"x\"]"),
    )
    .unwrap();
    let check = run("check");
    assert_eq!(check.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&check.stderr).contains("Definition differs from source revision")
    );
}
