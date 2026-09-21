use std::path::Path;
use std::process::{Command, Stdio};

use crate::{Error, Result};

/// The reviewed process boundary for git: every other module reaches git
/// only through this function (or the helpers below that call it).
fn run(root: &Path, args: &[&str], environment: &[(&str, &str)]) -> Result<std::process::Output> {
    #[allow(
        clippy::disallowed_methods,
        reason = "git is the reviewed process boundary for capobara"
    )]
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(environment.iter().copied())
        .output()
        .map_err(Error::Io)
}

pub fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    checked_output(root, args, &[])
}

fn checked_output(root: &Path, args: &[&str], environment: &[(&str, &str)]) -> Result<Vec<u8>> {
    let out = run(root, args, environment)?;
    if !out.status.success() {
        return Err(Error::Invalid(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// Scratch repositories retain deterministic authors and ignore host Git config.
#[cfg(test)]
pub(crate) fn test_git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    checked_output(
        root,
        args,
        &[
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@example.invalid"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@example.invalid"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ],
    )
}

pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(git_bytes(root, args)?)
        .map_err(|_| Error::Invalid(format!("git {} produced non-UTF-8 output", args.join(" "))))
}

pub fn git_ok(root: &Path, args: &[&str]) -> bool {
    run(root, args, &[])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> bool {
    git_ok(root, &["merge-base", "--is-ancestor", ancestor, descendant])
}

pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Accepts either a 40-hex git tree id or a 64-hex digest. The projector
/// implementation digest (`toolDigest`) becomes a git tree id (40 hex) once
/// a later task computes it from the tool's own source tree; Node's
/// receipts, and this crate's tests until then, use a 64-hex SHA-256 digest.
pub fn is_tree_id_or_digest(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_tree_id_or_digest_accepts_40_or_64_lowercase_hex_only() {
        assert!(is_tree_id_or_digest(&"a".repeat(40)));
        assert!(is_tree_id_or_digest(&"a".repeat(64)));
        assert!(!is_tree_id_or_digest(&"a".repeat(41)));
        assert!(!is_tree_id_or_digest(&"A".repeat(40)));
        assert!(!is_tree_id_or_digest(&"g".repeat(40)));
        assert!(!is_tree_id_or_digest(""));
    }
}
