//! Cargo build script: embeds this crate's own git tree id into the binary
//! (as the `CAPOBARA_TREE_ID` compile-time environment variable) so
//! `tooldigest::embedded()` can prove, at verify/check time, that the
//! projector binary running is the one committed at the source revision
//! being projected (`cli::project::run` step 6).
//!
//! Precedence:
//! 1. `CAPOBARA_TREE_ID` in the *build* environment, if set and non-empty,
//!    wins outright. This is how the standalone public build (outside
//!    Mono, where there is no `rust/tools/capobara` git history to read)
//!    supplies a tree id directly. `scripts/equivalence.sh` deliberately
//!    does *not*: it builds with `env -u CAPOBARA_TREE_ID` inside a
//!    detached worktree at the revision under test, so the git lookup
//!    below is the only source of the value it then checks.
//! 2. Otherwise, `git rev-parse HEAD:rust/tools/capobara` from this
//!    crate's own directory (a `<rev>:<path>` object name is resolved
//!    relative to the repository root regardless of the invoking
//!    directory, so this works from the crate directory without a
//!    separate `--show-toplevel` lookup for that call). If `git status
//!    --porcelain -- rust/tools/capobara`, run from the repository root
//!    (a pathspec argument to `git status` *is* resolved relative to the
//!    invoking directory, unlike the `rev-parse` object name above, so
//!    this one call needs the toplevel as its `-C`), is non-empty, `-dirty`
//!    is appended: a binary built from an edited working tree can then
//!    never pass the "projector matches the committed revision" check,
//!    matching Node's equivalent fail-closed comparison against the files
//!    on disk.
//! 3. If neither is available (for example, building outside any git
//!    repository), the embedded value is empty and the runtime check
//!    (`build_projection` and stored-receipt validation) fails closed with
//!    "Projector tree id unavailable; build inside Mono or set
//!    CAPOBARA_TREE_ID".
use std::process::Command;

#[allow(
    clippy::disallowed_methods,
    reason = "build script queries git for the crate tree id"
)]
fn git(dir: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}

fn compute_tree_id() -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let id = git(&manifest_dir, &["rev-parse", "HEAD:rust/tools/capobara"])?;
    let root = git(&manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let dirty = git(
        &root,
        &["status", "--porcelain", "--", "rust/tools/capobara"],
    )
    .is_some_and(|s| !s.is_empty());
    Some(if dirty { format!("{id}-dirty") } else { id })
}

fn main() {
    println!("cargo:rerun-if-env-changed=CAPOBARA_TREE_ID");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let tree_id = std::env::var("CAPOBARA_TREE_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(compute_tree_id)
        .unwrap_or_default();

    println!("cargo:rustc-env=CAPOBARA_TREE_ID={tree_id}");
}
