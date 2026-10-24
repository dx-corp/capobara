use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::tree::{Entries, contained_path, files_under, read_entry};
use crate::{Error, Result};

const CRATES: [(&str, &str); 2] = [
    ("dex-loop", "dex/loop"),
    ("managed-inference-contract", "managed-inference-contract"),
];

pub(super) fn collect(source: &Path, entries: &mut Entries) -> Result<()> {
    if !contained_path(source, "packages/dex-host-rs/Cargo.toml")?.exists() {
        return Ok(());
    }
    let mono = source
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Contract("Maestro source has no containing Rust workspace".into()))?;
    let rust_root = contained_path(mono, "rust")?;
    for (name, directory) in CRATES {
        let crate_root = contained_path(&rust_root, &format!("crates/{directory}"))?;
        for path in files_under(&crate_root, &|_| false)? {
            // Match the Node projection's prefix expression, including its
            // Cargo.toml/README.md prefix behavior.
            if !(path.starts_with("Cargo.toml")
                || path.starts_with("README.md")
                || path.starts_with("src/")
                || path.starts_with("tests/"))
            {
                continue;
            }
            let mut entry = read_entry(&crate_root, &path)?;
            if path == "Cargo.toml" {
                entry.content = public_rust_manifest(&rust_root, directory, name)?;
            } else if name == "dex-loop" && path == "README.md" {
                entry.content = public_dex_loop_readme(&entry.content);
            }
            entries.insert(format!("vendor/{name}/{path}"), entry);
        }
    }
    Ok(())
}

fn section<'a>(workspace: &'a str, name: &str) -> Result<&'a str> {
    let header = format!("[{name}]\n");
    let start = workspace
        .find(&header)
        .ok_or_else(|| Error::Contract(format!("Missing {name} in Rust workspace")))?;
    let remaining = &workspace[start + header.len()..];
    let end = remaining
        .char_indices()
        .find(|(index, value)| {
            *value == '[' && (*index == 0 || remaining.as_bytes()[*index - 1] == b'\n')
        })
        .map_or(remaining.len(), |(index, _)| index);
    Ok(&remaining[..end])
}

fn assignment<'a>(workspace: &'a str, table: &str, name: &str) -> Result<&'a str> {
    let prefix = format!("{name} = ");
    section(workspace, table)?
        .split('\n')
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or_else(|| Error::Contract(format!("Missing {name} in {table}")))
}

fn replace_fallible(
    regex: &Regex,
    input: &str,
    mut replacement: impl FnMut(&Captures<'_>) -> Result<String>,
) -> Result<String> {
    let mut result = String::new();
    let mut end = 0;
    for captures in regex.captures_iter(input) {
        let matched = captures.get(0).expect("complete regex match");
        result.push_str(&input[end..matched.start()]);
        result.push_str(&replacement(&captures)?);
        end = matched.end();
    }
    result.push_str(&input[end..]);
    Ok(result)
}

fn public_rust_manifest(rust_root: &Path, directory: &str, crate_name: &str) -> Result<Vec<u8>> {
    static INHERITED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^([A-Za-z0-9_-]+)\.workspace = true$").expect("inherited dependency regex")
    });
    static FEATURES: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^([A-Za-z0-9_-]+) = \{ workspace = true, features = (\[[^\n]+\]) \}$")
            .expect("inherited dependency features regex")
    });
    static VERSION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"version = ("[^"]+")"#).expect("dependency version regex"));
    static PRIVATE_PATH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"path = "(?:crates/|\.\./products/)"#).expect("private workspace path regex")
    });
    let workspace_entry = read_entry(rust_root, "Cargo.toml")?;
    let workspace = String::from_utf8_lossy(&workspace_entry.content);
    let manifest_entry = read_entry(rust_root, &format!("crates/{directory}/Cargo.toml"))?;
    let mut manifest = String::from_utf8_lossy(&manifest_entry.content).into_owned();
    for name in ["edition", "license", "publish", "rust-version"] {
        manifest = manifest.replacen(
            &format!("{name}.workspace = true"),
            &format!(
                "{name} = {}",
                assignment(&workspace, "workspace.package", name)?
            ),
            1,
        );
    }
    manifest = replace_fallible(&INHERITED, &manifest, |captures| {
        let name = &captures[1];
        Ok(format!(
            "{name} = {}",
            assignment(&workspace, "workspace.dependencies", name)?
        ))
    })?;
    manifest = replace_fallible(&FEATURES, &manifest, |captures| {
        let name = &captures[1];
        let dependency = assignment(&workspace, "workspace.dependencies", name)?;
        let version = VERSION
            .captures(dependency)
            .and_then(|capture| capture.get(1))
            .ok_or_else(|| {
                Error::Contract(format!("Missing explicit {name} version in Rust workspace"))
            })?;
        Ok(format!(
            "{name} = {{ version = {}, features = {} }}",
            version.as_str(),
            &captures[2]
        ))
    })?;
    let mut lints = Vec::new();
    for name in ["rust", "clippy"] {
        lints.push(format!(
            "[lints.{name}]\n{}",
            section(&workspace, &format!("workspace.lints.{name}"))?.trim_end()
        ));
    }
    manifest = manifest.replacen("[lints]\nworkspace = true", &lints.join("\n\n"), 1);
    if manifest.contains("workspace = true") || manifest.contains(".workspace = true") {
        return Err(Error::Contract(
            "Public dex-loop manifest still inherits the private Rust workspace".into(),
        ));
    }
    manifest = manifest.replacen(
        "agent-codemode = { path = \"../products/maestro/packages/codemode-rs\" }",
        "agent-codemode = { path = \"../../packages/codemode-rs\" }",
        1,
    );
    manifest = manifest.replacen(
        "managed-inference-contract = { path = \"crates/managed-inference-contract\" }",
        "managed-inference-contract = { path = \"../managed-inference-contract\" }",
        1,
    );
    if PRIVATE_PATH.is_match(&manifest) {
        return Err(Error::Contract(format!(
            "Public {crate_name} still references a private workspace path"
        )));
    }
    Ok(format!("{}\n\n[workspace]\n", manifest.trim_end()).into_bytes())
}

fn public_dex_loop_readme(content: &[u8]) -> Vec<u8> {
    static HOSTED_POLICY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\s+In the hosted `dex-tools` policy,[\s\S]*?\[policy checks\]\(\.\./tools/src/policy\.rs\)\.")
            .expect("hosted policy README regex")
    });
    let content = String::from_utf8_lossy(content);
    let mut content = HOSTED_POLICY.replace(&content, "").into_owned();
    if let Some(start) = content.find("The [filesystem host")
        && let Some(end) = content[start..].find("## Checks\n")
    {
        content.replace_range(start..start + end, concat!(
                "The [filesystem host tests](../../packages/dex-host-rs/tests/recovery_contract.rs)\n",
                "run these assertions against the public host's real storage adapters. The\n",
                "model and mutation are deterministic probes. These tests prove port recovery\n",
                "and persisted replay; they do not prove process-kill/fsync durability, live\n",
                "provider reconciliation, or cross-process locking for the single-process\n",
                "local effect ledger.\n\n",
                "Run from the public repository root after reviewing the\n",
                "[build guidance](../../docs/BUILD_TESTING.md):\n\n",
                "```bash\ncargo test --locked -p maestro-dex-host --test recovery_contract\n```\n\n"
            ));
    }
    content
        .replacen(
            "From the repository root, after the same build-capacity check:",
            "From the public repository root:",
            1,
        )
        .replace(
            "--manifest-path rust/Cargo.toml -p dex-loop",
            "--manifest-path vendor/dex-loop/Cargo.toml -p dex-loop",
        )
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    const WORKSPACE: &str = concat!(
        "[workspace.package]\nedition = \"2024\"\nlicense = \"MIT\"\npublish = false\nrust-version = \"1.95\"\n\n",
        "[workspace.dependencies]\nserde = \"1.0\"\n",
        "tokio = { version = \"1.44\", features = [\"full\"] }\n",
        "agent-codemode = { path = \"../products/maestro/packages/codemode-rs\" }\n",
        "managed-inference-contract = { path = \"crates/managed-inference-contract\" }\n\n",
        "[workspace.lints.rust]\nunsafe_code = \"forbid\"\n\n",
        "[workspace.lints.clippy]\ndbg_macro = \"deny\"\n"
    );
    const MANIFEST: &str = concat!(
        "[package]\nname = \"fixture\"\nedition.workspace = true\nlicense.workspace = true\n",
        "publish.workspace = true\nrust-version.workspace = true\n\n[dependencies]\n",
        "serde.workspace = true\ntokio = { workspace = true, features = [\"time\"] }\n",
        "agent-codemode.workspace = true\nmanaged-inference-contract.workspace = true\n\n",
        "[lints]\nworkspace = true\n"
    );

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("products/maestro");
        let rust = temp.path().join("rust");
        fs::create_dir_all(source.join("packages/dex-host-rs")).unwrap();
        fs::write(source.join("packages/dex-host-rs/Cargo.toml"), "").unwrap();
        for (_, directory) in CRATES {
            let root = rust.join("crates").join(directory);
            fs::create_dir_all(root.join("src")).unwrap();
            fs::create_dir_all(root.join("tests")).unwrap();
            fs::write(root.join("Cargo.toml"), MANIFEST).unwrap();
            fs::write(root.join("README.md"), "readme\n").unwrap();
            fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
            fs::write(root.join("tests/check.rs"), "// check\n").unwrap();
            fs::write(root.join("private.txt"), "private\n").unwrap();
        }
        fs::write(rust.join("Cargo.toml"), WORKSPACE).unwrap();
        (temp, source, rust)
    }

    #[test]
    fn manifest_resolves_workspace_and_rewrites_only_public_paths() {
        let (_temp, _source, rust) = fixture();
        let output =
            String::from_utf8(public_rust_manifest(&rust, "dex/loop", "dex-loop").unwrap())
                .unwrap();
        assert!(output.contains(
            "edition = \"2024\"\nlicense = \"MIT\"\npublish = false\nrust-version = \"1.95\""
        ));
        assert!(output.contains("serde = \"1.0\""));
        assert!(output.contains("tokio = { version = \"1.44\", features = [\"time\"] }"));
        assert!(output.contains("agent-codemode = { path = \"../../packages/codemode-rs\" }"));
        assert!(
            output.contains(
                "managed-inference-contract = { path = \"../managed-inference-contract\" }"
            )
        );
        assert!(output.ends_with("[lints.rust]\nunsafe_code = \"forbid\"\n\n[lints.clippy]\ndbg_macro = \"deny\"\n\n[workspace]\n"));
        fs::write(
            rust.join("Cargo.toml"),
            WORKSPACE.replace("serde = \"1.0\"", "serde = { path = \"crates/serde\" }"),
        )
        .unwrap();
        assert!(
            public_rust_manifest(&rust, "dex/loop", "dex-loop")
                .unwrap_err()
                .to_string()
                .contains("private workspace path")
        );
    }

    #[test]
    fn readme_removes_hosted_policy_and_projects_host_test_guidance() {
        let input = concat!(
            "Policy remains. In the hosted `dex-tools` policy, private text\n",
            "[policy checks](../tools/src/policy.rs).\n\n",
            "The [filesystem host tests](private) old guidance\n\n## Checks\n",
            "From the repository root, after the same build-capacity check:\n",
            "cargo test --manifest-path rust/Cargo.toml -p dex-loop\n",
            "cargo clippy --manifest-path rust/Cargo.toml -p dex-loop\n"
        );
        let output = String::from_utf8(public_dex_loop_readme(input.as_bytes())).unwrap();
        assert!(output.starts_with("Policy remains.\n\nThe [filesystem host tests](../../packages/dex-host-rs/tests/recovery_contract.rs)\n"));
        assert!(output.contains("cargo test --locked -p maestro-dex-host --test recovery_contract\n```\n\n## Checks\nFrom the public repository root:"));
        assert_eq!(
            output
                .matches("--manifest-path vendor/dex-loop/Cargo.toml -p dex-loop")
                .count(),
            2
        );
        assert!(!output.contains("private"));
    }

    #[test]
    fn collection_gates_vendoring_preserves_modes_and_rejects_symlinks() {
        let (_temp, source, rust) = fixture();
        let executable = rust.join("crates/dex/loop/tests/check.rs");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let mut entries = Entries::new();
        collect(&source, &mut entries).unwrap();
        assert_eq!(entries.len(), 8);
        assert_eq!(entries["vendor/dex-loop/tests/check.rs"].mode, 0o755);
        assert_eq!(
            entries["vendor/managed-inference-contract/src/lib.rs"].content,
            b"pub fn fixture() {}\n"
        );
        symlink("lib.rs", rust.join("crates/dex/loop/src/link.rs")).unwrap();
        assert!(collect(&source, &mut Entries::new()).is_err());
        fs::remove_file(source.join("packages/dex-host-rs/Cargo.toml")).unwrap();
        let mut absent = Entries::new();
        collect(&source, &mut absent).unwrap();
        assert!(absent.is_empty());
    }
}
