use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{PathOpts, js_cmp, safe_path, sort_js};
use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub content: Vec<u8>,
    pub mode: u32,
}

pub type Entries = BTreeMap<String, Entry>;

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn contained_path(root: &Path, path: &str) -> Result<PathBuf> {
    safe_path(path, PathOpts::default())?;
    let base = root.symlink_metadata()?;
    if base.file_type().is_symlink() || !base.is_dir() {
        return Err(Error::Contract(
            "Projection root must be a real directory".into(),
        ));
    }
    let mut current = root.to_path_buf();
    for part in path.split('/') {
        match fs::read_dir(&current) {
            Ok(entries) => {
                for entry in entries {
                    let name = entry?.file_name();
                    let name = name.to_string_lossy();
                    if name != part && name.eq_ignore_ascii_case(part) {
                        return Err(Error::Contract(format!(
                            "Case-colliding filesystem path: {path}"
                        )));
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        current.push(part);
        match fs::symlink_metadata(&current) {
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(Error::Contract(format!(
                        "Symlink at projection boundary: {path}"
                    )));
                }
                if !meta.is_dir() && !meta.is_file() {
                    return Err(Error::Contract(format!(
                        "Special file at projection boundary: {path}"
                    )));
                }
            }
        }
    }
    Ok(current)
}

pub fn files_under(root: &Path, skip: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
    fn visit(
        root: &Path,
        prefix: &str,
        skip: &dyn Fn(&str) -> bool,
        out: &mut Vec<String>,
    ) -> Result<()> {
        let dir = if prefix.is_empty() {
            root.to_path_buf()
        } else {
            contained_path(root, prefix)?
        };
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if name == ".git" || skip(&path) {
                continue;
            }
            contained_path(root, &path)?;
            let kind = entry.file_type()?;
            // `!kind.is_dir()` would silently accept device/socket/fifo entries;
            // this contract intentionally rejects anything but a regular file.
            #[allow(clippy::filetype_is_file)]
            let is_regular_file = kind.is_file();
            if kind.is_dir() {
                visit(root, &path, skip, out)?;
            } else if is_regular_file {
                out.push(path);
            } else {
                return Err(Error::Contract(format!(
                    "Unsupported projection entry: {path}"
                )));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(root, "", skip, &mut out)?;
    sort_js(&mut out);
    Ok(out)
}

pub fn read_entry(root: &Path, path: &str) -> Result<Entry> {
    let absolute = contained_path(root, path)?;
    let meta = fs::symlink_metadata(&absolute)?;
    if !meta.is_file() {
        return Err(Error::Contract(format!(
            "Expected regular projection file: {path}"
        )));
    }
    let mode = if meta.permissions().mode() & 0o111 != 0 {
        0o755
    } else {
        0o644
    };
    Ok(Entry {
        content: fs::read(&absolute)?,
        mode,
    })
}

pub fn tree_digest(entries: &Entries) -> String {
    let mut hash = Sha256::new();
    let mut ordered: Vec<(&String, &Entry)> = entries.iter().collect();
    ordered.sort_by(|(a, _), (b, _)| js_cmp(a, b));
    for (path, entry) in ordered {
        let line = serde_json::to_string(&(path, entry.mode, sha256_hex(&entry.content)))
            .unwrap_or_default();
        hash.update(line.as_bytes());
        hash.update(b"\n");
    }
    hex::encode(hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn digest_matches_node_format() {
        let mut entries = Entries::new();
        entries.insert(
            "b.txt".into(),
            Entry {
                content: b"hi\n".to_vec(),
                mode: 0o755,
            },
        );
        entries.insert(
            "a.txt".into(),
            Entry {
                content: b"".to_vec(),
                mode: 0o644,
            },
        );
        // Computed with: node -e 'const {treeDigest}=await import("./scripts/projections/tree.mjs"); ...'
        // Recorded once from the Node implementation and frozen here.
        insta::assert_snapshot!(tree_digest(&entries));
    }

    #[test]
    fn digest_orders_entries_like_javascript() {
        let mut entries = Entries::new();
        entries.insert(
            "\u{FF01}.md".into(),
            Entry {
                content: b"a".to_vec(),
                mode: 0o644,
            },
        );
        entries.insert(
            "\u{1F389}.md".into(),
            Entry {
                content: b"b".to_vec(),
                mode: 0o644,
            },
        );
        // Recorded from Node with the one-liner below; the emoji entry is hashed first.
        insta::assert_snapshot!(tree_digest(&entries));
    }

    #[test]
    fn symlinks_and_special_files_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/real"), b"x").unwrap();
        symlink("real", dir.path().join("sub/link")).unwrap();
        symlink("sub", dir.path().join("dirlink")).unwrap();
        assert!(contained_path(dir.path(), "sub/link").is_err());
        assert!(contained_path(dir.path(), "dirlink/real").is_err());
        assert!(contained_path(dir.path(), "sub/missing/deeper").is_ok());
        assert!(files_under(dir.path(), &|_| false).is_err());
    }

    #[test]
    fn case_aliases_are_rejected_on_read() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("README.md"), b"x").unwrap();
        assert!(contained_path(dir.path(), "readme.md").is_err());
    }

    #[test]
    fn read_entry_normalizes_modes_and_lists_sorted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("z"), b"1").unwrap();
        fs::write(dir.path().join("a"), b"2").unwrap();
        fs::set_permissions(dir.path().join("a"), fs::Permissions::from_mode(0o710)).unwrap();
        assert_eq!(read_entry(dir.path(), "a").unwrap().mode, 0o755);
        assert_eq!(read_entry(dir.path(), "z").unwrap().mode, 0o644);
        assert_eq!(files_under(dir.path(), &|_| false).unwrap(), vec!["a", "z"]);
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), b"ref").unwrap();
        assert_eq!(files_under(dir.path(), &|_| false).unwrap(), vec!["a", "z"]);
    }
}
