use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::{Entries, assert_portable_paths, contained_path, read_entry, sort_js};
use crate::{Error, Result};

#[derive(Debug)]
pub struct Plan {
    pub entries: Entries,
    pub copied_paths: Vec<String>,
    pub deleted_paths: Vec<String>,
}

impl Plan {
    pub fn copied_count(&self) -> usize {
        self.copied_paths.len()
    }
    pub fn deleted_count(&self) -> usize {
        self.deleted_paths.len()
    }
}

pub fn plan_tree(target: &Path, entries: Entries, deletions: Vec<String>) -> Result<Plan> {
    assert_portable_paths(
        entries
            .keys()
            .map(String::as_str)
            .chain(deletions.iter().map(String::as_str)),
    )?;
    let mut copied_paths = Vec::new();
    let mut lower = HashSet::new();
    for (path, entry) in &entries {
        if !lower.insert(path.to_lowercase()) {
            return Err(Error::Contract(format!(
                "Case-colliding projection path: {path}"
            )));
        }
        let absolute = contained_path(target, path)?;
        if entry.mode != 0o644 && entry.mode != 0o755 {
            return Err(Error::Contract(format!("Invalid entry: {path}")));
        }
        let old = if absolute.exists() {
            Some(read_entry(target, path)?)
        } else {
            None
        };
        if old.as_ref() != Some(entry) {
            copied_paths.push(path.clone());
        }
    }
    for path in entries.keys() {
        let mut parts: Vec<&str> = path.split('/').collect();
        while parts.len() > 1 {
            parts.pop();
            if lower.contains(&parts.join("/").to_lowercase()) {
                return Err(Error::Contract(format!(
                    "Projection file/directory collision: {path}"
                )));
            }
        }
    }
    let mut deleted_paths: Vec<String> = deletions
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    sort_js(&mut deleted_paths);
    for path in &deleted_paths {
        let absolute = contained_path(target, path)?;
        if let Ok(meta) = fs::symlink_metadata(&absolute)
            && !meta.is_file()
        {
            return Err(Error::Contract(format!(
                "Cannot delete non-file projection path: {path}"
            )));
        }
        if entries.contains_key(path) {
            return Err(Error::Contract(format!(
                "Projection both writes and deletes {path}"
            )));
        }
    }
    sort_js(&mut copied_paths);
    Ok(Plan {
        entries,
        copied_paths,
        deleted_paths,
    })
}

pub fn apply_tree(target: &Path, plan: &Plan) -> Result<()> {
    for path in plan.copied_paths.iter().chain(&plan.deleted_paths) {
        contained_path(target, path)?;
    }
    for path in &plan.copied_paths {
        let absolute = contained_path(target, path)?;
        let entry = plan
            .entries
            .get(path)
            .ok_or_else(|| Error::Invalid(format!("Plan lost entry {path}")))?;
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&absolute, &entry.content)?;
        fs::set_permissions(&absolute, fs::Permissions::from_mode(entry.mode))?;
    }
    if !plan.deleted_paths.is_empty() {
        let target = target.canonicalize()?;
        for path in &plan.deleted_paths {
            let absolute = contained_path(&target, path)?;
            if absolute.exists() {
                fs::remove_file(&absolute)?;
            }
            let mut parent = absolute.parent().map(Path::to_path_buf);
            while let Some(dir) = parent {
                if dir == target || !dir.exists() || fs::read_dir(&dir)?.next().is_some() {
                    break;
                }
                fs::remove_dir(&dir)?;
                parent = dir.parent().map(Path::to_path_buf);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Entry;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn entry(bytes: &[u8], mode: u32) -> Entry {
        Entry {
            content: bytes.to_vec(),
            mode,
        }
    }

    #[test]
    fn deterministic_mapping_binary_bytes_executable_bits_stale_deletion() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("same"), b"same").unwrap();
        fs::write(dir.path().join("stale"), b"old").unwrap();
        fs::create_dir_all(dir.path().join("deep/dir")).unwrap();
        fs::write(dir.path().join("deep/dir/gone"), b"x").unwrap();
        let mut entries = Entries::new();
        entries.insert("same".into(), entry(b"same", 0o644));
        entries.insert("bin/tool".into(), entry(&[0, 255, 10, 13], 0o755));
        let plan = plan_tree(
            dir.path(),
            entries,
            vec!["deep/dir/gone".into(), "stale".into(), "stale".into()],
        )
        .unwrap();
        assert_eq!(plan.copied_paths, vec!["bin/tool"]);
        assert_eq!(plan.deleted_paths, vec!["deep/dir/gone", "stale"]);
        apply_tree(dir.path(), &plan).unwrap();
        assert_eq!(
            fs::read(dir.path().join("bin/tool")).unwrap(),
            vec![0, 255, 10, 13]
        );
        assert_eq!(
            fs::metadata(dir.path().join("bin/tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(!dir.path().join("stale").exists());
        assert!(!dir.path().join("deep").exists());
        let plan2 = plan_tree(dir.path(), plan.entries.clone(), vec![]).unwrap();
        assert!(plan2.copied_paths.is_empty());
    }

    #[test]
    fn file_directory_collisions_and_write_delete_conflicts_fail_before_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = Entries::new();
        entries.insert("a".into(), entry(b"", 0o644));
        entries.insert("a/b".into(), entry(b"", 0o644));
        assert!(plan_tree(dir.path(), entries, vec![]).is_err());
        let mut entries = Entries::new();
        entries.insert("x".into(), entry(b"", 0o644));
        assert!(plan_tree(dir.path(), entries, vec!["x".into()]).is_err());
        let mut entries = Entries::new();
        entries.insert("Y".into(), entry(b"", 0o644));
        entries.insert("y".into(), entry(b"", 0o644));
        assert!(plan_tree(dir.path(), entries, vec![]).is_err());
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn apply_tree_no_ops_on_empty_plan_without_touching_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing");
        let plan = plan_tree(&target, Entries::new(), vec![]).unwrap();
        assert!(plan.copied_paths.is_empty());
        assert!(plan.deleted_paths.is_empty());
        assert!(apply_tree(&target, &plan).is_ok());
        assert!(!target.exists());
    }
}
