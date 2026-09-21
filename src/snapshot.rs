use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::git::{git, git_bytes, is_sha};
use crate::tree::{PathOpts, assert_portable_paths, safe_path};
use crate::{Error, Result, invalid};

/// Upper bound on the size of a `git archive` payload we will buffer in
/// memory, matching Node's `maxBuffer` cap in `project.mjs::withSnapshot`.
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;

/// Materialize the committed content of `roots` at `sha` into a scratch
/// directory, invoke `f` with it, and remove the directory before
/// returning (on every path: success, an error from `f`, or an error from
/// this function itself).
pub fn with_snapshot<T>(
    root: &Path,
    sha: &str,
    roots: &[String],
    f: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    // Validate the shape of `sha` before it ever reaches a git argv: a
    // leading-dash value would otherwise be parsed by git as an option.
    invalid(is_sha(sha), "Invalid source revision")?;
    let resolved = git(root, &["rev-parse", &format!("{sha}^{{commit}}")]).unwrap_or_default();
    invalid(resolved.trim() == sha, "Invalid source revision")?;

    let paths: Vec<String> = roots
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for path in &paths {
        safe_path(path, PathOpts::default())?;
    }

    let mut args = vec!["ls-tree", "-rz", sha, "--"];
    args.extend(paths.iter().map(String::as_str));
    let listing = git(root, &args)?;
    let entries: Vec<&str> = listing.split('\0').filter(|s| !s.is_empty()).collect();
    invalid(
        !entries.iter().any(|e| e.starts_with("160000 ")),
        "Submodules are not projection inputs",
    )?;
    let present: Vec<&str> = entries
        .iter()
        .map(|e| e.split_once('\t').map(|(_, p)| p).unwrap_or(""))
        .collect();
    assert_portable_paths(present.iter().copied())?;

    let archive_roots: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|root| {
            present
                .iter()
                .any(|file| *file == *root || file.starts_with(&format!("{root}/")))
        })
        .collect();
    invalid(
        !archive_roots.is_empty(),
        "No committed projection inputs found",
    )?;

    let mut args = vec!["archive", "--format=tar", sha, "--"];
    args.extend(archive_roots);
    let archive = git_bytes(root, &args)?;
    invalid(
        archive.len() <= MAX_ARCHIVE_BYTES,
        "Projection archive exceeds 512 MiB",
    )?;

    let scratch = tempfile::Builder::new()
        .prefix("mono-projection-")
        .tempdir()?;
    extract_tar(&archive, scratch.path())?;
    f(scratch.path())
}

/// The reviewed process boundary for `tar`: the sole extraction call site
/// for materializing a git archive into a scratch directory.
///
/// `tar`'s stderr and exit status are always collected via
/// `wait_with_output`, even when writing the archive to its stdin fails
/// (for example because `tar` exited early on a malformed archive and
/// closed its stdin, producing a broken pipe on our write). A write error
/// is remembered rather than propagated immediately, so the child is
/// always reaped and a broken pipe never shadows `tar`'s real diagnostic.
fn extract_tar(archive: &[u8], dest: &Path) -> Result<()> {
    #[allow(
        clippy::disallowed_methods,
        reason = "tar extraction of a git archive is a reviewed process boundary"
    )]
    let mut tar = Command::new("tar")
        .args(["-xf", "-", "-C"])
        .arg(dest)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let write_result = tar
        .stdin
        .take()
        .ok_or_else(|| Error::Invalid("tar stdin unavailable".into()))
        .and_then(|mut stdin| stdin.write_all(archive).map_err(Error::Io));
    // Drop of `write_result`'s stdin handle (above, at the end of the
    // closure) happens before we wait, so tar sees EOF even if the write
    // was short.
    let output = tar.wait_with_output()?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "tar extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tar_reports_tar_failure_without_broken_pipe_noise() {
        let dest = tempfile::tempdir().unwrap();
        // Large enough to outrun tar's early failure (tar reads and
        // rejects the bogus header before we finish writing), so the
        // observable failure is tar's own diagnostic, not a broken pipe.
        let junk = vec![b'x'; 64 * 1024];
        let err = extract_tar(&junk, dest.path()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("tar extraction failed"),
            "unexpected message: {message}"
        );
        assert!(
            !message.contains("Broken pipe"),
            "broken pipe leaked into the error: {message}"
        );
    }
}
