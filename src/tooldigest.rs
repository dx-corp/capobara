//! The projector implementation digest: proof that the running binary is
//! built from the same `rust/tools/capobara` tree as the source revision
//! being projected. Ports the `toolDigest` half of `main` in
//! `scripts/projections/project.mjs`, but not its mechanism: Node hashes the
//! concatenated contents of a fixed `TOOL_INPUTS` file list at run time;
//! this crate instead embeds its own git tree id at *compile* time (see
//! `build.rs`) and compares that fixed value against the tree id of
//! `rust/tools/capobara` at the revision being projected. Either shape is a
//! valid "the tool that ran this matches the tool committed at this
//! revision" proof; `git::is_tree_id_or_digest` accepts both a 40-hex tree
//! id (this crate) and a 64-hex digest (Node, and any receipt Node wrote
//! before the cutover) wherever a `toolDigest` is validated.

use std::path::Path;
use std::sync::OnceLock;

use crate::Result;
use crate::git;

/// The cached override value, shared by both debug-only override
/// mechanisms below: `Some(id)` once either one has run, `None` once
/// `embedded()` has checked the environment and found nothing set. A plain
/// (non-`cfg`-gated) static so it always exists as an item -- `embedded()`
/// only ever touches it inside a `cfg!(debug_assertions)`-guarded branch
/// that is dead code (and eligible for removal by the optimizer) in a
/// release build, exactly as before this override was added.
static OVERRIDE: OnceLock<Option<String>> = OnceLock::new();

/// This crate's own compiled-in tree id (or digest), as embedded by
/// `build.rs` into the `CAPOBARA_TREE_ID` compile-time environment
/// variable (empty if unavailable, `<id>-dirty` if the working tree had
/// uncommitted changes under `rust/tools/capobara` -- see `build.rs`'s doc
/// comment for the full precedence).
///
/// In debug builds only, two mechanisms can override that value, both
/// backed by the same process-wide `OnceLock` above (so whichever runs
/// first for a given process wins, and stays pinned for that process's
/// whole lifetime):
/// - The *process* environment variable `CAPOBARA_TREE_ID_OVERRIDE`, read
///   lazily on first use. This is what lets a spawned child process --
///   for example the compiled binary `tests/project_cli.rs` and
///   `tests/transport_git.rs`'s `run_capobara` helper invoke -- run
///   against a synthetic source repository: the test sets the variable on
///   that one child's environment only, and the child's own fresh
///   `OnceLock` picks it up the first time it reads a digest.
/// - `override_for_tests`, called explicitly and in-process, for a test
///   that calls `cli::project::run` (or anything built on it, such as
///   `transport::git::assert_candidate_matches_main_projection` or
///   `publish_prepared_tree`) directly in the *same* process, where there
///   is no child `Command` to attach an environment variable to.
///
/// Release builds compile neither seam: `override_for_tests` does not
/// exist at all outside `#[cfg(debug_assertions)]`, and this function's
/// only source of truth is `env!("CAPOBARA_TREE_ID")`, so a production
/// binary can never be talked into skipping the "projector differs from
/// source revision" check via an environment variable or a library call at
/// runtime.
pub fn embedded() -> &'static str {
    if cfg!(debug_assertions) {
        let over = OVERRIDE.get_or_init(|| std::env::var("CAPOBARA_TREE_ID_OVERRIDE").ok());
        if let Some(value) = over {
            return value;
        }
    }
    env!("CAPOBARA_TREE_ID")
}

/// Debug builds only: pin the tree id this process reports, for tests that
/// call `cli::project::run` in-process against a synthetic source
/// repository. Must be called before the first digest read; a second call
/// with a different value panics so two fixtures cannot silently disagree.
///
/// Two distinct causes can leave `OVERRIDE` already initialized when this
/// runs, and the panic names which one happened, with the stored value in
/// either case: (a) a second, disagreeing call to `override_for_tests`
/// itself, or (b) `embedded()` already ran once with
/// `CAPOBARA_TREE_ID_OVERRIDE` unset, caching `None` -- meaning a digest
/// was read before this function ever got a chance to install its value,
/// which is a call-order bug in the caller, not a value conflict.
#[cfg(debug_assertions)]
pub fn override_for_tests(tree_id: &str) {
    if OVERRIDE.set(Some(tree_id.to_owned())).is_err() {
        match OVERRIDE.get() {
            Some(Some(existing)) if existing == tree_id => {}
            Some(Some(existing)) => panic!(
                "tooldigest::override_for_tests: already set to a different tree id (stored {existing:?}, requested {tree_id:?})"
            ),
            Some(None) => panic!(
                "tooldigest::override_for_tests: a digest was already read with no override installed (stored value: None), so this call is too late; call override_for_tests before the first digest read. Requested tree id: {tree_id:?}"
            ),
            None => {
                unreachable!("OVERRIDE.set just failed, so OVERRIDE.get() must return Some(..)")
            }
        }
    }
}

/// `git rev-parse {sha}:rust/tools/capobara` in `source_root`'s repository:
/// the tree id of this crate's directory as committed at `sha`, for
/// comparison against `embedded()`.
pub fn at_revision(source_root: &Path, sha: &str) -> Result<String> {
    let id = git::git(
        source_root,
        &["rev-parse", &format!("{sha}:rust/tools/capobara")],
    )?;
    Ok(id.trim().to_string())
}
