mod support;
use capobara::catalog::{assert_main_authorized_revision, publication_matrix, read_catalog};
use support::Repo;

fn no_sdk(_: &str) -> Option<Vec<String>> {
    None
}

fn definition(name: &str) -> Vec<u8> {
    serde_json::json!({
        "schemaVersion": 1, "name": name, "class": "source-tree", "mode": "copy-v1",
        "sourceRepository": "dx-corp/mono", "visibility": "public",
        "mappings": [{"source": format!("pkgs/{name}"), "destination": ".", "include": ["src/**"], "exclude": []}],
        "destination": {"repository": format!("dx-corp/{name}"), "branch": "main", "syncBranch": "sync/mono-projection", "holdLabel": "sync-hold"},
        "destinationOwned": [".github/**", "SECURITY.md"],
        "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
        "outputManaged": ["src/**"]
    }).to_string().into_bytes()
}

#[test]
fn publication_matrix_selects_latest_relevant_revision_and_rejects_unauthorized_heads() {
    let repo = Repo::init("https://github.com/dx-corp/mono.git");
    repo.write(
        "config/projections/repositories.json",
        br#"{"schemaVersion":1,"sourceRepository":"dx-corp/mono","projections":["alpha","beta"]}"#,
    );
    repo.write("config/projections/alpha.json", &definition("alpha"));
    repo.write("config/projections/beta.json", &definition("beta"));
    repo.write("pkgs/alpha/src/a.rs", b"a");
    repo.write("pkgs/beta/src/b.rs", b"b");
    let first = repo.commit("both");
    repo.write("pkgs/beta/src/b.rs", b"b2");
    let second = repo.commit("beta only");
    repo.write("unrelated.txt", b"x");
    let third = repo.commit("unrelated");
    repo.set_remote_main(&third);
    assert_eq!(read_catalog(repo.path(), &no_sdk).unwrap().len(), 2);
    let matrix = publication_matrix(repo.path(), "all", &no_sdk).unwrap();
    let by_name = |n: &str| {
        matrix
            .include
            .iter()
            .find(|e| e.name == n)
            .unwrap()
            .source_sha
            .clone()
    };
    assert_eq!(by_name("alpha"), first);
    assert_eq!(by_name("beta"), second);

    // Pin the exact serialized JSON: key order, camelCase, compactness,
    // and entry order. `matrix.include`'s order is catalog-file order
    // (alpha, beta), matching Node's `definitions.filter(...).map(...)`.
    // Nothing else in this suite asserts on the wire bytes; a later
    // refactor (e.g. to `serde_json::Value`/`OrderedValue`, or an added
    // field) could otherwise silently reorder or rename keys.
    assert_eq!(
        serde_json::to_string(&matrix).unwrap(),
        format!(
            r#"{{"include":[{{"name":"alpha","repository":"dx-corp/alpha","sourceSha":"{first}"}},{{"name":"beta","repository":"dx-corp/beta","sourceSha":"{second}"}}]}}"#
        )
    );

    // `assert_main_authorized_revision` returns the authority SHA, not its
    // argument -- Node asserts this explicitly (catalog.test.mjs:82).
    assert_eq!(
        assert_main_authorized_revision(repo.path(), &first).unwrap(),
        third
    );

    // Match the exact message, not just "is an error": either assertion
    // below would also pass for an unrelated Error::Io (e.g. a typo'd
    // fixture path or a missing git binary).
    let unknown = publication_matrix(repo.path(), "gamma", &no_sdk).unwrap_err();
    assert_eq!(unknown.to_string(), "Unknown projection: gamma");

    // HEAD ahead of remote main is not authorized.
    repo.write("pkgs/alpha/src/a.rs", b"a2");
    repo.commit("unpublished");
    let unauthorized = publication_matrix(repo.path(), "all", &no_sdk).unwrap_err();
    assert!(
        unauthorized
            .to_string()
            .starts_with("Projection revision is not authorized by refs/remotes/origin/main: "),
        "{unauthorized}"
    );
}

/// The shared-source-revision-group algorithm (`examples`, `deixic-node`,
/// `deixic-python`) has no coverage in the brief's Step-1 test above,
/// whose two fixture names (`alpha`, `beta`) never form a group.
/// Transcribed from Node's "examples and both SDKs select one latest
/// relevant source snapshot" (`catalog.test.mjs:112-200`): a change to any
/// one coupled member's inputs selects that revision for all three
/// members; an uncoupled projection is unaffected; a group member absent
/// from the catalog fails with Node's exact `Missing coupled projection:
/// <name>` message (`catalog.mjs:34`).
#[test]
fn coupled_projections_share_one_source_revision_and_require_every_member_present() {
    let repo = Repo::init("https://github.com/dx-corp/mono.git");
    let names = ["examples", "deixic-node", "deixic-python", "other"];
    repo.write(
        "config/projections/repositories.json",
        br#"{"schemaVersion":1,"sourceRepository":"dx-corp/mono","projections":["examples","deixic-node","deixic-python","other"]}"#,
    );
    for name in names {
        repo.write(
            &format!("config/projections/{name}.json"),
            &definition(name),
        );
        repo.write(&format!("pkgs/{name}/src/lib.rs"), name.as_bytes());
    }
    let base = repo.commit("base");
    repo.set_remote_main(&base);

    // Only `examples`'s own input changes. `deixic-node` and
    // `deixic-python` did not change, but they select the same new
    // revision because `SHARED_SOURCE_REVISION_GROUPS` couples all three.
    // `other` is not in the group and keeps selecting `base`.
    repo.write("pkgs/examples/src/lib.rs", b"examples v2");
    let examples_change = repo.commit("examples input");
    repo.set_remote_main(&examples_change);

    let matrix = publication_matrix(repo.path(), "all", &no_sdk).unwrap();
    let sha = |n: &str| {
        matrix
            .include
            .iter()
            .find(|e| e.name == n)
            .unwrap()
            .source_sha
            .clone()
    };
    assert_eq!(sha("examples"), examples_change);
    assert_eq!(sha("deixic-node"), examples_change);
    assert_eq!(sha("deixic-python"), examples_change);
    assert_eq!(sha("other"), base);

    // Drop `deixic-node` from the catalog. It is still one of
    // `examples`'s coupled group members -- the group comes from
    // `SHARED_SOURCE_REVISION_GROUPS`, not from the catalog's current
    // membership -- so it is "missing", not merely "unpublished".
    repo.write(
        "config/projections/repositories.json",
        br#"{"schemaVersion":1,"sourceRepository":"dx-corp/mono","projections":["examples","deixic-python","other"]}"#,
    );
    let dropped = repo.commit("drop deixic-node from the catalog");
    repo.set_remote_main(&dropped);
    let err = publication_matrix(repo.path(), "examples", &no_sdk).unwrap_err();
    assert_eq!(err.to_string(), "Missing coupled projection: deixic-node");
}

#[allow(
    clippy::disallowed_methods,
    reason = "integration test executes the capobara binary"
)]
fn capobara() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_capobara"))
}

/// `catalog check`/`catalog matrix`'s `--root` resolution. Node's `ROOT`
/// (`catalog.mjs:6`) is derived from the script's own file location, so
/// it works from any current directory; this crate has no script location
/// to derive from, so it resolves the same way a human would from a
/// shell -- `git rev-parse --show-toplevel` -- unless `--root` is given
/// explicitly, and reports a clean message when neither is available.
#[test]
fn catalog_root_defaults_to_the_git_toplevel_and_can_be_overridden_or_fail_cleanly() {
    let repo = Repo::init("https://github.com/dx-corp/mono.git");
    repo.write(
        "config/projections/repositories.json",
        br#"{"schemaVersion":1,"sourceRepository":"dx-corp/mono","projections":["alpha"]}"#,
    );
    repo.write("config/projections/alpha.json", &definition("alpha"));
    repo.write("pkgs/alpha/src/a.rs", b"a");
    let sha = repo.commit("alpha");
    repo.set_remote_main(&sha);

    // Default: run from a subdirectory of the checkout, no --root.
    let subdir = repo.path().join("pkgs/alpha/src");
    let out = capobara()
        .args(["catalog", "check"])
        .current_dir(&subdir)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "1 repository projections validated"
    );

    // An explicit --root overrides the git lookup and works from an
    // unrelated current directory. `--root` precedes the subcommand here.
    let unrelated = tempfile::tempdir().unwrap();
    let out = capobara()
        .args(["catalog", "--root"])
        .arg(repo.path())
        .arg("check")
        .current_dir(unrelated.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `--root` is `global = true`, so it also parses trailing the
    // subcommand: `capobara catalog check --root <p>`.
    let out = capobara()
        .args(["catalog", "check", "--root"])
        .arg(repo.path())
        .current_dir(unrelated.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Neither --root nor a git checkout to fall back to: the ruling's
    // exact message, exit 1 (catalog's own exit-code convention).
    // `GIT_CEILING_DIRECTORIES` stops `git rev-parse --show-toplevel` from
    // walking past the tempdir into whatever git repository (if any)
    // happens to contain the host's temp directory, so this assertion
    // does not depend on the host's temp directory being outside any git
    // work tree.
    let out = capobara()
        .args(["catalog", "check"])
        .current_dir(unrelated.path())
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "Not inside a git repository; pass --root"
    );
}
