mod support;

use std::process::Command;

fn bin() -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test executes the capobara binary"
    )]
    Command::new(env!("CARGO_BIN_EXE_capobara"))
}

#[test]
fn help_lists_every_subcommand() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for name in [
        "catalog",
        "plan",
        "apply",
        "verify",
        "check",
        "prepare",
        "preflight",
        "publish",
        "run",
    ] {
        assert!(text.contains(name), "missing subcommand {name}");
    }
}

#[test]
fn vendor_check_preserves_success_drift_and_invalid_input_exit_codes() {
    let upstream = support::Repo::init("https://example.invalid/upstream.git");
    upstream.write("src/a.txt", b"original");
    let pin = upstream.commit("upstream");
    let mono = support::Repo::init("https://example.invalid/mono.git");
    mono.write("vendor/thing/src/a.txt", b"original");
    mono.commit("vendor");
    mono.write("vendor.json", serde_json::json!({
        "schemaVersion": 1, "name": "thing", "class": "vendor-import",
        "upstream": { "repository": "https://example.invalid/upstream.git", "commit": pin, "include": ["src/**"] },
        "destination": { "path": "vendor/thing" }
    }).to_string().as_bytes());
    let run = |definition: &str| {
        bin()
            .args(["vendor", "check", "--definition"])
            .arg(mono.path().join(definition))
            .arg("--root")
            .arg(mono.path())
            .arg("--upstream")
            .arg(upstream.path())
            .output()
            .unwrap()
    };
    let clean = run("vendor.json");
    assert_eq!(
        clean.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    mono.write("vendor/thing/src/a.txt", b"changed");
    mono.commit("drift");
    let drift = run("vendor.json");
    assert_eq!(drift.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&drift.stdout).contains("DRIFT"));
    assert!(String::from_utf8_lossy(&drift.stderr).contains("undeclared divergence"));
    let invalid = run("missing.json");
    assert_eq!(invalid.status.code(), Some(2));
    assert!(!invalid.stderr.is_empty());
}
