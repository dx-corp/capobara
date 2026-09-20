mod support;
use capobara::snapshot::with_snapshot;
use support::Repo;

#[test]
fn git_snapshot_ignores_untracked_and_dirty_content_and_rejects_unknown_revisions() {
    let repo = Repo::init("https://github.com/dx-corp/mono.git");
    repo.write("proto/a.proto", b"syntax = \"proto3\";\n");
    repo.write("LICENSE", b"BUSL\n");
    let sha = repo.commit("initial");
    repo.write("proto/a.proto", b"dirty\n");
    repo.write("proto/untracked.proto", b"new\n");
    let seen = with_snapshot(
        repo.path(),
        &sha,
        &["proto".into(), "LICENSE".into(), "missing".into()],
        |dir| {
            assert_eq!(
                std::fs::read(dir.join("proto/a.proto")).unwrap(),
                b"syntax = \"proto3\";\n"
            );
            assert!(!dir.join("proto/untracked.proto").exists());
            assert!(dir.join("LICENSE").exists());
            Ok(dir.to_path_buf())
        },
    )
    .unwrap();
    assert!(!seen.exists(), "snapshot directory must be removed");
    let bogus = "0".repeat(40);
    let unknown_revision_err =
        with_snapshot(repo.path(), &bogus, &["proto".into()], |_| Ok(())).unwrap_err();
    assert_eq!(unknown_revision_err.to_string(), "Invalid source revision");
    let no_inputs_err =
        with_snapshot(repo.path(), &sha, &["nowhere".into()], |_| Ok(())).unwrap_err();
    assert_eq!(
        no_inputs_err.to_string(),
        "No committed projection inputs found"
    );
}

#[test]
fn snapshot_rejects_dash_prefixed_revision_before_touching_git() {
    // `root` is a plain tempdir, not a git repository at all: any call to
    // git here would fail differently (e.g. "not a git repository"), so an
    // exact "Invalid source revision" proves is_sha() short-circuited
    // before `--exec-path` ever reached a git argv.
    let not_a_repo = tempfile::tempdir().unwrap();
    let err = with_snapshot(not_a_repo.path(), "--exec-path", &["proto".into()], |_| {
        Ok(())
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "Invalid source revision");
}

#[test]
fn git_snapshot_rejects_submodules() {
    let repo = Repo::init("https://github.com/dx-corp/mono.git");
    repo.write("vendor/keep.txt", b"placeholder\n");
    let commit_sha = repo.commit("initial");
    // Register a gitlink (mode 160000) pointing at an arbitrary commit,
    // without needing a real nested repository: stage it directly with
    // update-index, then commit the index as-is (not via Repo::commit,
    // which runs `git add -A` and would try to reconcile the working tree
    // against the now-missing vendor/sub directory).
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{commit_sha},vendor/sub"),
    ]);
    repo.git(&["commit", "-q", "-m", "add gitlink"]);
    let head = repo.head();
    let err = with_snapshot(repo.path(), &head, &["vendor".into()], |_| Ok(())).unwrap_err();
    assert_eq!(err.to_string(), "Submodules are not projection inputs");
}
