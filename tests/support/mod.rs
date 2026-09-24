#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    pub fn init(remote_url: &str) -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["remote", "add", "origin", remote_url]);
        repo
    }
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
    pub fn git(&self, args: &[&str]) -> String {
        #[allow(
            clippy::disallowed_methods,
            reason = "integration tests drive a scratch git repository"
        )]
        let out = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    pub fn write(&self, path: &str, bytes: &[u8]) {
        let full = self.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }
    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }
    pub fn set_remote_main(&self, sha: &str) {
        self.git(&["update-ref", "refs/remotes/origin/main", sha]);
    }
    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }
    pub fn into_path(self) -> PathBuf {
        self.dir.keep()
    }
}

/// Writes every input file the reviewed `deixic-python` SDK assembly
/// policy names into `root`, with just enough content to satisfy that
/// policy's import-closure check: the BFS root (`console/v1/console_pb2.py`)
/// imports every other generated Python module directly.
///
/// The file *lists* come from `policies::PYTHON_SDK_FILES` and
/// `policies::PYTHON_GENERATED_FILES`, the same constants the policy is
/// built from, so this cannot drift from the policy it feeds.
///
/// `tests/sdk_assembly.rs` builds the same Python snapshot inline, as part
/// of a larger one covering all three policies. That duplication is
/// deliberate: folding its Python block into this helper would mean editing
/// a file another task is actively revising. See the task-13 fix-round
/// report.
pub fn populate_python_snapshot(root: &Path) {
    use capobara::modes::sdk_assembly::policies;

    const PYPROJECT_TOML: &str = concat!(
        "[project]\n",
        "name = \"deixic\"\n",
        "version = \"0.1.0\"\n",
        "dependencies = [\n",
        "  \"httpx>=0.27\",\n",
        "]\n",
        "\n",
        "[project.urls]\n",
        "Repository = \"https://github.com/dx-corp/mono\"\n",
    );

    fn write(root: &Path, path: &str, contents: &[u8]) {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, contents).unwrap();
    }

    for path in policies::PYTHON_SDK_FILES {
        let full = format!("sdk/deixic/python/{path}");
        if *path == "pyproject.toml" {
            write(root, &full, PYPROJECT_TOML.as_bytes());
        } else {
            write(root, &full, format!("# {path}\n").as_bytes());
        }
    }
    write(
        root,
        "sdk/deixic/python/src/deixicpublic/v1/sdk_pb2.py",
        b"# deixicpublic.v1\n",
    );
}

// ---------------------------------------------------------------------
// The crate's own projection definition, for tests that check the crate
// tree against it rather than a synthetic fixture (definition_coverage.rs,
// standalone_build.rs). Reads the real, committed
// `config/projections/capobara.json`, three directories above the crate
// (`rust/tools/capobara` -> `rust/tools` -> `rust` -> repo root), and
// reuses `capobara::definition::load_definition` -- the same validation
// and parsing the real tool uses -- rather than parsing the JSON by hand a
// second time.
// ---------------------------------------------------------------------

/// Loads and validates the real `config/projections/capobara.json`.
pub fn capobara_definition() -> capobara::definition::Definition {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/projections/capobara.json");
    capobara::definition::load_definition(&path, &|_| None)
        .unwrap_or_else(|e| panic!("load_definition({}): {e}", path.display()))
        .definition
}

/// The definition's mapping for the crate directory itself (as opposed to
/// the repo-root `LICENSE` mapping, whose `source` is `"."`).
pub fn crate_mapping(
    definition: &capobara::definition::Definition,
) -> &capobara::definition::Mapping {
    definition
        .mappings
        .iter()
        .find(|m| m.source == "rust/tools/capobara")
        .expect("capobara.json has no rust/tools/capobara mapping")
}

/// Whether `path` (relative to the crate root) is actually projected by
/// `mapping` -- the same `Matcher`-based predicate `modes::copy_v1::collect`
/// uses in production (`included.matches(&path) && !excluded.matches(&path)`),
/// not a reimplementation of it. `false` for a path no include pattern
/// names at all *and* for a path an include pattern names but an exclude
/// pattern deliberately removes (see `is_named_by_include`, which tells
/// those two `false` cases apart).
pub fn is_projected(mapping: &capobara::definition::Mapping, path: &str) -> bool {
    let included = capobara::tree::Matcher::new(&mapping.include).unwrap();
    let excluded = capobara::tree::Matcher::new(&mapping.exclude).unwrap();
    included.matches(path) && !excluded.matches(path)
}

/// Whether some pattern in `mapping.include` names `path` at all, regardless
/// of whether `mapping.exclude` subsequently removes it. A file the
/// definition's include patterns never mention (e.g. `scripts/**`, which
/// none of `Cargo.toml`/`Cargo.lock`/`README.md`/`build.rs`/`src/**`/
/// `tests/**` matches) is not "handled" by the definition at all and needs
/// an explicit `UNPROJECTED` allowlist entry; a file an include pattern
/// names but `exclude` then removes (e.g. `tests/fixtures/definitions/**`)
/// *is* handled -- excluding it is a decision the definition itself makes,
/// not an omission -- so it needs no allowlist entry even though
/// `is_projected` is `false` for it too.
pub fn is_named_by_include(mapping: &capobara::definition::Mapping, path: &str) -> bool {
    capobara::tree::Matcher::new(&mapping.include)
        .unwrap()
        .matches(path)
}

/// `git ls-files` under `dir`, relative to `dir`.
pub fn git_ls_files(dir: &Path) -> Vec<String> {
    #[allow(
        clippy::disallowed_methods,
        reason = "test enumerates the crate's own git-tracked files"
    )]
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-files"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git ls-files: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}
