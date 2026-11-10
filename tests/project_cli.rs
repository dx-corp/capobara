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

use crate::support;
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
fn equivalent_source_retains_a_verified_receipt_without_writing_and_exact_verify_stays_strict() {
    let source = Repo::init("https://github.com/dx-corp/mono.git");
    source.write(
        "config/projections/sample.json",
        definition_json("sample").as_bytes(),
    );
    source.write("pkg/src/a.rs", b"public payload");
    source.write("rust/tools/capobara/src/lib.rs", b"// fixture tool");
    let published = source.commit("published source");
    let tool = source.git(&["rev-parse", "HEAD:rust/tools/capobara"]);
    let target = Repo::init("https://github.com/dx-corp/sample.git");
    target.write("SECURITY.md", b"destination owned");
    target.set_remote_main(&target.commit("destination"));
    let reports = tempfile::tempdir().unwrap();
    let report = reports.path().join("equivalence-report.json");
    let run = |command: &str, sha: &str, equivalent: bool| {
        let mut cmd = capobara();
        cmd.env("CAPOBARA_TREE_ID_OVERRIDE", tool.trim())
            .args([command, "--definition"])
            .arg(source.path().join("config/projections/sample.json"))
            .arg("--source")
            .arg(source.path())
            .args(["--source-sha", sha, "--target"])
            .arg(target.path())
            .arg("--report")
            .arg(&report);
        if equivalent {
            cmd.arg("--allow-equivalent-source");
        }
        cmd.output().unwrap()
    };
    assert!(run("apply", &published, false).status.success());
    target.set_remote_main(&target.commit("published projection"));
    let receipt_path = target.path().join(".repository-projection.json");
    let receipt = std::fs::read(&receipt_path).unwrap();
    source.write("private-note", b"private-only change");
    let current = source.commit("private-only change");
    for command in ["check", "apply", "verify"] {
        let output = run(command, &current, true);
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(&receipt_path).unwrap(), receipt);
        assert!(target.git(&["status", "--porcelain"]).trim().is_empty());
        let status: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
        assert_eq!(status["result"], "in_sync");
        assert_eq!(status["provenance"]["sourceSha"], published);
        assert_eq!(status["evaluatedSourceSha"], current);
    }
    let exact = run("verify", &current, false);
    assert_eq!(exact.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&exact.stderr).contains("Provenance mismatch: sourceSha"));

    let original: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
    for field in ["definitionDigest", "toolDigest", "contentDigest"] {
        let mut tampered = original.clone();
        tampered[field] = serde_json::json!("0".repeat(40));
        std::fs::write(&receipt_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert_eq!(
            run("check", &current, true).status.code(),
            Some(1),
            "{field}"
        );
    }
    for (field, value) in [
        ("publicationEligible", serde_json::json!(false)),
        ("projection", serde_json::json!("wrong-identity")),
        ("priorProjectedBase", serde_json::json!("0".repeat(40))),
    ] {
        let mut tampered = original.clone();
        tampered[field] = value;
        std::fs::write(&receipt_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert_eq!(
            run("check", &current, true).status.code(),
            Some(2),
            "{field}"
        );
    }
    std::fs::write(&receipt_path, &receipt).unwrap();
    use std::os::unix::fs::PermissionsExt;
    for path in [&receipt_path, &target.path().join("src/a.rs")] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run("check", &current, true).status.code(), Some(1));
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    target.write("SECURITY.md", b"public-owned edit must survive");
    assert!(run("apply", &current, true).status.success());
    assert_eq!(
        std::fs::read(target.path().join("SECURITY.md")).unwrap(),
        b"public-owned edit must survive"
    );
    target.write("SECURITY.md", b"destination owned");

    // Claiming an older ancestor produced these bytes is not enough: rebuild it.
    source.git(&["checkout", "--detach", &published]);
    source.write("pkg/src/a.rs", b"different old payload");
    let forged_source = source.commit("different source");
    source.git(&["checkout", "--detach", &current]);
    source.git(&[
        "merge",
        "--no-ff",
        "-s",
        "ours",
        "-m",
        "retain current payload",
        &forged_source,
    ]);
    let merged = source.head();
    let mut forged: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
    forged["sourceSha"] = serde_json::json!(forged_source);
    std::fs::write(&receipt_path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();
    let output = run("check", &merged, true);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Provenance mismatch: contentDigest"));
    std::fs::write(&receipt_path, &receipt).unwrap();

    target.write("src/a.rs", b"public drift");
    assert_eq!(run("check", &merged, true).status.code(), Some(1));
    target.write("src/a.rs", b"public payload");
    target.write("src/stale.rs", b"stale file");
    assert_eq!(run("check", &merged, true).status.code(), Some(1));
    std::fs::remove_file(target.path().join("src/stale.rs")).unwrap();
    source.write("pkg/src/a.rs", b"changed public payload");
    let changed = source.commit("public change");
    assert_eq!(run("check", &changed, true).status.code(), Some(1));
    assert!(run("apply", &changed, true).status.success());
    assert_eq!(
        std::fs::read(target.path().join("src/a.rs")).unwrap(),
        b"changed public payload"
    );
    assert!(run("verify", &changed, false).status.success());
    source.git(&["checkout", "--detach", &published]);
    assert_eq!(run("check", &published, true).status.code(), Some(2));
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

#[test]
fn publication_rejects_an_older_or_divergent_source_without_changing_destination() {
    let source = Repo::init("https://github.com/dx-corp/mono.git");
    source.write(
        "config/projections/sample.json",
        definition_json("sample").as_bytes(),
    );
    source.write("pkg/src/a.rs", b"old");
    source.write("rust/tools/capobara/src/lib.rs", b"// fixture tool");
    let older = source.commit("older source");
    let tree_id = source.git(&["rev-parse", "HEAD:rust/tools/capobara"]);
    source.write("pkg/src/computer.rs", b"new source that must survive");
    let newer = source.commit("newer source");
    let target = Repo::init("https://github.com/dx-corp/sample.git");
    target.write("SECURITY.md", b"destination owned");
    target.set_remote_main(&target.commit("destination"));
    let definition = source.path().join("config/projections/sample.json");
    let apply = |sha: &str| {
        source.git(&["checkout", "--detach", sha]);
        capobara()
            .env("CAPOBARA_TREE_ID_OVERRIDE", tree_id.trim())
            .args(["apply", "--definition"])
            .arg(&definition)
            .arg("--source")
            .arg(source.path())
            .args(["--source-sha", sha, "--target"])
            .arg(target.path())
            .output()
            .unwrap()
    };
    for sha in [&older, &newer, &newer] {
        let output = apply(sha);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        target.set_remote_main(&target.commit("projected source"));
    }
    let receipt = std::fs::read(target.path().join(".repository-projection.json")).unwrap();
    source.git(&["checkout", "--detach", &older]);
    source.write("pkg/src/a.rs", b"divergent");
    let divergent = source.commit("divergent source");
    for sha in [&older, &divergent] {
        let output = apply(sha);
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("Source revision would rewind or diverge")
        );
        assert_eq!(
            std::fs::read(target.path().join("src/computer.rs")).unwrap(),
            b"new source that must survive"
        );
        assert_eq!(
            std::fs::read(target.path().join(".repository-projection.json")).unwrap(),
            receipt
        );
        assert!(target.git(&["status", "--porcelain"]).trim().is_empty());
    }
}
