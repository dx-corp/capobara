//! Native public Maestro projection, including its reviewed sanitization boundary.

mod boundary;
mod vendor;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;
use serde::Serialize;

use crate::definition::Definition;
use crate::ordered::OrderedValue as Json;
use crate::tree::{
    Entries, Matcher, PathOpts, contained_path, files_under, read_entry, safe_path, sort_js,
};
use crate::{Error, Result, contract};
use boundary::*;

fn parse_json(bytes: &[u8]) -> Result<Json> {
    serde_json::from_slice(bytes)
        .map_err(|e| Error::Contract(format!("Invalid public projection JSON: {e}")))
}

fn json_bytes(value: &Json, indent: &[u8]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
    value
        .serialize(&mut serializer)
        .map_err(|e| Error::Contract(e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    match value {
        Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}
fn set(value: &mut Json, key: &str, replacement: Json) {
    if let Json::Object(fields) = value {
        if let Some((_, old)) = fields.iter_mut().find(|(k, _)| k == key) {
            *old = replacement;
        } else {
            fields.push((key.into(), replacement));
        }
    }
}
fn remove(value: &mut Json, key: &str) {
    if let Json::Object(fields) = value {
        fields.retain(|(k, _)| k != key);
    }
}
fn string(value: &Json) -> Option<&str> {
    if let Json::String(s) = value {
        Some(s)
    } else {
        None
    }
}

fn resolve_package(source: &Path) -> Result<(Vec<u8>, String)> {
    let mut pkg = parse_json(&read_entry(source, "package.json")?.content)?;
    contract(
        matches!(pkg, Json::Object(_)),
        "Source package.json must contain a JSON object",
    )?;
    let name = get(&pkg, "maestro")
        .and_then(|m| get(m, "canonicalPackageName"))
        .and_then(string)
        .or_else(|| get(&pkg, "name").and_then(string))
        .unwrap_or("")
        .to_owned();
    contract(
        !name.trim().is_empty(),
        "Could not resolve public package name; pass --package-name or set package.json maestro.canonicalPackageName",
    )?;
    set(&mut pkg, "name", Json::String(name.clone()));
    let mut maestro = get(&pkg, "maestro")
        .filter(|v| matches!(v, Json::Object(_)))
        .cloned()
        .unwrap_or(Json::Object(Vec::new()));
    set(
        &mut maestro,
        "canonicalPackageName",
        Json::String(name.clone()),
    );
    let mut aliases = vec![Json::String(name.clone())];
    if let Some(Json::Array(existing)) = get(&maestro, "packageAliases") {
        for alias in existing {
            if string(alias).is_some_and(|s| !s.trim().is_empty()) && !aliases.contains(alias) {
                aliases.push(alias.clone());
            }
        }
    }
    set(&mut maestro, "packageAliases", Json::Array(aliases));
    set(&mut pkg, "maestro", maestro);
    let mut scripts = get(&pkg, "scripts")
        .filter(|v| matches!(v, Json::Object(_)))
        .cloned()
        .unwrap_or(Json::Object(Vec::new()));
    for (key, value) in [
        (
            "check:hosted-orb-delegation",
            "node --check scripts/hosted-orb-delegation-acceptance.mjs && node --test scripts/hosted-orb-delegation-acceptance.test.mjs",
        ),
        (
            "release:verify:published",
            "node scripts/smoke-registry-install.js",
        ),
        (
            "release:verify:published:e2e",
            "node scripts/smoke-published-replay-e2e.js",
        ),
        (
            "release:verify:published:evidence",
            "node scripts/verify-published-replay-evidence.js",
        ),
        ("release:deprecate", "node scripts/deprecate-release.js"),
    ] {
        set(&mut scripts, key, Json::String(value.into()));
    }
    set(&mut pkg, "scripts", scripts);
    Ok((json_bytes(&pkg, b"  ")?, name))
}

/// Resolves the receipt's package coordinate from the product source root.
pub(crate) fn public_package_name(source: &Path) -> Result<String> {
    resolve_package(source).map(|(_, name)| name)
}

fn is_public_doc(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    PUBLIC_DOCUMENTATION_PATHS
        .iter()
        .any(|p| normalized == *p || normalized.starts_with(&format!("{p}/")))
}
fn is_override(path: &str) -> bool {
    path == ".env.example" || path.starts_with(".env.example/") || is_public_doc(path)
}
fn override_descendant(path: &str) -> bool {
    PUBLIC_DOCUMENTATION_PATHS
        .iter()
        .copied()
        .chain([".env.example"])
        .any(|p| p.starts_with(&format!("{path}/")))
}

// Legacy exclusion globs permit `.git/**` and trailing `/`, which the public
// projection-path matcher deliberately rejects. Keep those parser semantics here.
struct Exclusions {
    exact: BTreeSet<String>,
    prefixes: Vec<String>,
    regexes: Vec<Regex>,
}
impl Exclusions {
    fn new(patterns: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut result = Self {
            exact: BTreeSet::new(),
            prefixes: Vec::new(),
            regexes: Vec::new(),
        };
        for pattern in patterns {
            let pattern = pattern
                .strip_prefix("./")
                .or_else(|| pattern.strip_prefix('/'))
                .unwrap_or(&pattern)
                .to_owned();
            if let Some(prefix) = pattern.strip_suffix("/**") {
                result.prefixes.push(prefix.into());
            }
            if !pattern.contains('*') {
                if pattern.ends_with('/') {
                    result.prefixes.push(pattern.trim_end_matches('/').into());
                }
                result.exact.insert(pattern);
                continue;
            }
            let mut regex = String::from("^");
            let mut chars = pattern.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'*') {
                    chars.next();
                    regex.push_str(".*");
                } else if c == '*' {
                    regex.push_str("[^/]*");
                } else {
                    regex.push_str(&regex::escape(&c.to_string()));
                }
            }
            regex.push('$');
            result
                .regexes
                .push(Regex::new(&regex).map_err(|e| Error::Contract(e.to_string()))?);
        }
        Ok(result)
    }
    fn matches(&self, path: &str) -> bool {
        if self.exact.contains(path) {
            return true;
        }
        if is_override(path) {
            return false;
        }
        self.prefixes
            .iter()
            .any(|p| path == p || path.starts_with(&format!("{p}/")))
            || self.regexes.iter().any(|r| r.is_match(path))
    }
}
fn walk(root: &Path, exclusions: &Exclusions) -> Result<Vec<String>> {
    Ok(
        files_under(root, &|p| exclusions.matches(p) && !override_descendant(p))?
            .into_iter()
            .filter(|p| !exclusions.matches(p))
            .collect(),
    )
}

fn replace_regex(text: &str, pattern: &str, replacement: &str) -> Result<String> {
    Ok(Regex::new(pattern)
        .map_err(|e| Error::Contract(e.to_string()))?
        .replace(text, replacement)
        .into_owned())
}
fn remove_lock_package(content: &str, name: &str) -> String {
    let marker = format!("[[package]]\nname = \"{name}\"\n");
    let Some(start) = content.find(&marker) else {
        return content.into();
    };
    if let Some(end) = content[start..].find("\n[[package]]\n") {
        format!("{}{}", &content[..start], &content[start + end + 1..])
    } else {
        format!("{}\n", content[..start].trim_end())
    }
}
fn private_cerebro_trace(content: &str) -> bool {
    let text = content.to_lowercase();
    text.contains("cerebro") || text.contains("platform-connect-http")
}
fn sanitize_cargo(path: &str, content: &str) -> Result<String> {
    let output = match path {
        "packages/dex-host-rs/Cargo.toml" => {
            let external = "dex-loop = { path = \"../../../../rust/crates/dex/loop\"";
            contract(
                content.contains(external),
                "Maestro dex-loop path changed; update the public projection",
            )?;
            content.replace(external, "dex-loop = { path = \"../../vendor/dex-loop\"")
        }
        "Cargo.toml" => {
            let clean = content.replacen("    \"packages/cerebro-host-rs\",\n", "", 1);
            contract(
                !private_cerebro_trace(&clean),
                "Cargo.toml still names Cerebro after sanitization; update the public projection",
            )?;
            replace_regex(&clean, r#"(?m)^test-world = \{ git = "https://github\.com/evalops/test-world", rev = "[^"]+" \}\r?\n"#, "")?
                .replacen("\"vendor/*\"]", "\"vendor/*\", \"vendor/dex-loop\"]", 1)
        }
        "packages/ambient-agent-rs/Cargo.toml" => {
            replace_regex(content, r"(?m)^test-world\.workspace = true\r?\n", "")?
        }
        "Cargo.lock" => {
            let clean = [
                "cerebro-agent-tools",
                "cerebro-client",
                "cerebro-handle",
                "maestro-cerebro-host",
                "platform-connect-http",
            ]
            .into_iter()
            .fold(content.to_owned(), |text, name| {
                remove_lock_package(&text, name)
            });
            contract(
                !private_cerebro_trace(&clean),
                "Cargo.lock still names a private Cerebro package; update the public projection",
            )?;
            let clean = replace_regex(&clean, r#"(?m)^\s*"test-world",\r?\n"#, "")?;
            // Rust regex has no look-ahead; preserve the following package marker.
            replace_regex(
                &clean,
                r#"\n\[\[package\]\]\nname = "test-world"\nversion = "[^"]+"\nsource = "git\+https://github\.com/evalops/test-world\?rev=[^"]+"(\n\n\[\[package\]\])"#,
                "$1",
            )?
        }
        "deny.toml" => replace_regex(
            content,
            r#"(?m)^allow-git = \["https://github\.com/evalops/test-world"\]\r?\n"#,
            "",
        )?,
        "packages/ambient-agent-rs/src/platform_event_bus.rs" => {
            let text = replace_regex(
                content,
                r"(?m)^#\[cfg\(test\)\]\r?\nuse test_world::\{OrgOverrides, TestWorld, WorkspaceOverrides\};\r?\n\r?\n",
                "",
            )?;
            let text = replace_regex(
                &text,
                r#"(?m)^        let world = TestWorld::new\(\);\r?\n        let org = world\.org_with\(OrgOverrides::new\(\)\.slug\("org_test"\)\);\r?\n        let workspace =\r?\n            world\.workspace_with\(WorkspaceOverrides::new\(\)\.org\(&org\)\.slug\("workspace_test"\)\);\r?\n\r?\n"#,
                "",
            )?;
            replace_regex(
                &text,
                r"(?m)^            tenant_id: Some\(org\.record\(\)\.slug\),\r?\n            workspace_id: workspace\.record\(\)\.slug,",
                "            tenant_id: Some(\"org_test\".to_string()),\n            workspace_id: \"workspace_test\".to_string(),",
            )?
        }
        _ => content.into(),
    };
    contract(
        !Regex::new("(?i)test[-_]world")
            .expect("static regex")
            .is_match(&output),
        format!(
            "Sanitization left a private test-dependency reference in {path}; a rewrite rule in sanitizePublicCargoContent no longer matches the source."
        ),
    )?;
    Ok(output)
}

fn sanitize_dockerfile(content: &str) -> Result<String> {
    let start = content.find("COPY --from=dex-loop-workspace ");
    let end = start.and_then(|s| {
        content[s..]
            .find("COPY vendor/rquickjs-core-0.13.0 ")
            .map(|e| e + s)
    });
    contract(
        start.is_some() && end.is_some() && content.contains("COPY --from=planner /rust /rust"),
        "Maestro Dockerfile dependency context changed; update its public projection",
    )?;
    let public_copy = "COPY vendor/dex-loop ./vendor/dex-loop\nCOPY vendor/managed-inference-contract ./vendor/managed-inference-contract\n";
    let output = format!(
        "{}{}{}",
        &content[..start.expect("checked")],
        public_copy,
        &content[end.expect("checked")..]
    )
    .replacen("COPY --from=planner /rust /rust\n", public_copy, 1);
    let crate_copy = "COPY packages/cerebro-host-rs ./packages/cerebro-host-rs\n";
    let platform_copy = "COPY --from=planner /platform /platform\n";
    contract(
        output.matches(crate_copy).count() == 2 && output.matches(platform_copy).count() == 1,
        "Maestro Dockerfile Cerebro lines changed; update the public projection",
    )?;
    let output = output
        .replace(crate_copy, "")
        .replacen(platform_copy, "", 1);
    contract(
        !output.to_lowercase().contains("cerebro"),
        "Public Dockerfile still mentions Cerebro; update the public projection",
    )?;
    Ok(output)
}

fn replacement_path(path: &str) -> Option<&'static str> {
    match path {
        "AGENTS.md" => Some(".github/public-repo/AGENTS.md"),
        "scripts/hosted-orb-delegation-acceptance.mjs" => {
            Some(".github/public-repo/scripts/hosted-orb-delegation-acceptance.mjs")
        }
        "scripts/hosted-orb-delegation-acceptance.test.mjs" => {
            Some(".github/public-repo/scripts/hosted-orb-delegation-acceptance.test.mjs")
        }
        "scripts/check-public-surface-boundary.mjs" => {
            Some(".github/public-repo/scripts/check-public-surface-boundary.mjs")
        }
        "packages/local-host-rs/src/evalops_cli/ci_auth.rs" => {
            Some(".github/public-repo/packages/local-host-rs/src/evalops_cli/ci_auth.rs")
        }
        _ => None,
    }
}

fn public_content(source: &Path, path: &str, original: &[u8], package: &[u8]) -> Result<Vec<u8>> {
    if path == "package.json" {
        return Ok(package.to_vec());
    }
    if let Some(replacement) = replacement_path(path) {
        return Ok(read_entry(source, replacement)?.content);
    }
    if path == "vendor/tls-provenance.json" {
        let Json::Array(mut records) = parse_json(original)? else {
            return Err(Error::Contract(
                "TLS provenance consumer schema changed; update the public projection".into(),
            ));
        };
        for record in &mut records {
            contract(
                matches!(record, Json::Object(_))
                    && matches!(get(record, "workspaces"), Some(Json::Array(items)) if items.iter().all(|v| matches!(v, Json::String(_)))),
                "TLS provenance consumer schema changed; update the public projection",
            )?;
            remove(record, "workspaces");
        }
        return json_bytes(&Json::Array(records), b"  ");
    }
    if path == "Dockerfile" {
        return Ok(sanitize_dockerfile(&String::from_utf8_lossy(original))?.into_bytes());
    }
    if path == "docs/doc-path-allowlist.json" {
        let Json::Array(mut records) = parse_json(original)? else {
            return Err(Error::Contract(
                "docs/doc-path-allowlist.json must contain a JSON array".into(),
            ));
        };
        records.retain(|record| {
            get(record, "source")
                .and_then(string)
                .is_some_and(|p| is_public_doc(p) && source.join(p).exists())
        });
        return json_bytes(&Json::Array(records), b"\t");
    }
    if matches!(
        path,
        "docs/protocols/hosted-orb-delegation-acceptance-v1.json"
            | "products/maestro/docs/protocols/hosted-orb-delegation-acceptance-v1.json"
    ) {
        let mut document = parse_json(original)?;
        remove(&mut document, "contract_sources");
        remove(&mut document, "live_gate");
        if let Some(mut launch) = get(&document, "atomic_launch").cloned() {
            remove(&mut launch, "current_origin_status");
            set(&mut document, "atomic_launch", launch);
        }
        let bytes = json_bytes(&document, b"\t")?;
        return Ok(String::from_utf8_lossy(&bytes)
            .replace(
                "https://github.com/dx-corp/mono.git",
                "https://github.com/evalops/maestro.git",
            )
            .into_bytes());
    }
    if matches!(
        path,
        "packages/local-host-rs/src/evalops_cli.rs"
            | "products/maestro/packages/local-host-rs/src/evalops_cli.rs"
    ) {
        let text = String::from_utf8_lossy(original);
        let private = "  maestro evalops ci-auth exec ...        Run one child with a short-lived, exact-bound CI credential\\n";
        contract(
            text.contains(private),
            format!("Public text sanitization no longer matches {path}"),
        )?;
        return Ok(text.replacen(private, "", 1).into_bytes());
    }
    if PUBLIC_SANITIZED_PATHS.contains(&path) {
        return Ok(sanitize_cargo(path, &String::from_utf8_lossy(original))?.into_bytes());
    }
    Ok(original.to_vec())
}

fn assert_cerebro_boundary(entries: &Entries) -> Result<()> {
    let known = [
        (".env.example", 12),
        ("CHANGELOG.md", 3),
        ("Makefile", 74),
        ("docs/MCP_GUIDE.md", 10),
        ("docs/protocols/a2a-fleet-delegation.md", 1),
        ("docs/protocols/headless.md", 8),
        ("packages/local-host-rs/src/init_cli.rs", 1),
        ("packages/ui-rs/src/translations.rs", 5),
        ("scripts/check-cerebro-e2e.mjs", 129),
        ("skills/incident-triage/SKILL.md", 24),
        ("skills/incident-triage/reference/triage.md", 23),
        ("vendor/dex-loop/src/context/context_pocket.rs", 1),
    ];
    let mut problems = Vec::new();
    for (path, entry) in entries {
        let text = if entry.content.contains(&0) {
            String::new()
        } else {
            String::from_utf8_lossy(&entry.content).to_lowercase()
        };
        let count =
            path.to_lowercase().matches("cerebro").count() + text.matches("cerebro").count();
        let allowed = known
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, count)| *count)
            .unwrap_or(0);
        if count > allowed {
            problems.push(format!("{path}: {count} mention(s), {allowed} allowed"));
        }
    }
    sort_js(&mut problems);
    contract(
        problems.is_empty(),
        format!(
            "The public projection names Cerebro:\n{}",
            problems.join("\n")
        ),
    )
}

/// Collect the sanitized product tree and destination deletion candidates.
/// Shared build validation performs the final destination-ownership check.
pub(crate) fn collect(
    definition: &Definition,
    source_root: &Path,
    target_root: &Path,
    owned: &Matcher,
) -> Result<(Entries, Vec<String>)> {
    let mapping = definition
        .mappings
        .first()
        .ok_or_else(|| Error::Contract("Missing Maestro source mapping".into()))?;
    let source = contained_path(source_root, &mapping.source)?;
    let mut patterns: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| (*s).into()).collect();
    let exclude_file = contained_path(&source, ".github/public-release-mirror.exclude")?;
    if exclude_file.exists() {
        patterns.extend(
            fs::read_to_string(exclude_file)?
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.starts_with('#'))
                .map(str::to_owned),
        );
    }
    if let Ok(relative) = target_root.strip_prefix(&source)
        && !relative.as_os_str().is_empty()
    {
        patterns.push(format!("{}/**", relative.to_string_lossy()));
    }
    let excluded = Exclusions::new(patterns)?;
    let stale = Exclusions::new(
        STALE_PUBLIC_TARGET_DELETE_PATTERNS
            .iter()
            .map(|p| (*p).into()),
    )?;
    let source_files = walk(&source, &excluded)?;
    let target_files = walk(target_root, &excluded)?;
    let (package, _) = resolve_package(&source)?;
    let mut entries = Entries::new();
    for path in source_files {
        let mut original = read_entry(&source, &path)?;
        original.content = public_content(&source, &path, &original.content, &package)?;
        entries.insert(path, original);
    }
    vendor::collect(&source, &mut entries)?;
    assert_cerebro_boundary(&entries)?;
    let allowed = Matcher::new(&mapping.include)?;
    for path in entries.keys() {
        contract(
            allowed.matches(path),
            format!("Code output outside catalog allowlist: {path}"),
        )?;
    }
    let mut deletions: BTreeSet<String> = target_files
        .into_iter()
        .filter(|p| !entries.contains_key(p))
        .collect();
    for path in STALE_PUBLIC_TARGET_DELETES {
        if contained_path(target_root, path)?.exists() {
            deletions.insert((*path).into());
        }
    }
    for path in files_under(target_root, &|_| false)? {
        if stale.matches(&path) {
            deletions.insert(path);
        }
    }
    let credential = Regex::new(r"^gha-creds-.*\.json$").expect("static regex");
    for path in files_under(target_root, &|p| {
        ["coverage", "dist", "node_modules", "target", "tmp"]
            .contains(&p.rsplit('/').next().unwrap_or(p))
    })? {
        if credential.is_match(path.rsplit('/').next().unwrap_or(&path)) {
            deletions.insert(path);
        }
    }
    let release =
        parse_json(&read_entry(&source, ".github/release-mirror-manifest.json")?.content)?;
    contract(
        matches!(&release, Json::Object(_)),
        "Invalid release manifest",
    )?;
    contract(
        matches!(&release, Json::Object(fields) if fields.len() == 1 && fields[0].0 == "files"),
        "Unknown or missing release manifest fields",
    )?;
    let Some(Json::Array(release_files)) = get(&release, "files") else {
        return Err(Error::Contract("Invalid release files".into()));
    };
    contract(!release_files.is_empty(), "Invalid release files")?;
    let mut seen = BTreeSet::new();
    let mut release_paths = Vec::with_capacity(release_files.len());
    for file in release_files {
        let path = string(file).ok_or_else(|| Error::Contract("Invalid release files".into()))?;
        safe_path(
            path,
            PathOpts {
                pattern: true,
                root: false,
            },
        )?;
        release_paths.push(path);
        seen.insert(path);
    }
    contract(seen.len() == release_paths.len(), "Duplicate release files")?;
    for path in release_paths {
        safe_path(path, PathOpts::default())?;
        contract(
            allowed.matches(path) && !owned.matches(path),
            format!("Release helper crosses ownership: {path}"),
        )?;
        entries.insert(path.into(), read_entry(&source, path)?);
    }
    for mapping in definition.mappings.iter().skip(1) {
        let source = if mapping.source == "." {
            source_root.to_path_buf()
        } else {
            contained_path(source_root, &mapping.source)?
        };
        for path in &mapping.include {
            let target = if mapping.destination == "." {
                path.clone()
            } else {
                format!("{}/{path}", mapping.destination)
            };
            contract(
                !entries.contains_key(&target),
                format!("Overlapping mappings: {target}"),
            )?;
            entries.insert(target, read_entry(&source, path)?);
        }
    }
    let mut deletions: Vec<_> = deletions.into_iter().collect();
    sort_js(&mut deletions);
    Ok((entries, deletions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Entry;

    #[test]
    fn documentation_overrides_descend_through_excluded_parents() {
        let excluded = Exclusions::new(DEFAULT_EXCLUDES.iter().map(|p| (*p).into())).unwrap();
        assert!(excluded.matches("docs"));
        assert!(override_descendant("docs/design"));
        assert!(!excluded.matches("docs/BUILD_TESTING.md"));
        assert!(!excluded.matches("docs/cookbook/example.md"));
        assert!(excluded.matches("docs/private.md"));
        assert!(!excluded.matches(".env.example"));
        assert!(excluded.matches(".env.local"));
        let explicit = Exclusions::new(["docs/BUILD_TESTING.md".into(), "docs/**".into()]).unwrap();
        assert!(explicit.matches("docs/BUILD_TESTING.md"));
    }

    #[test]
    fn json_rewrites_preserve_order_indentation_and_newline() {
        let source = tempfile::tempdir().unwrap();
        let bytes = br#"{"z":1,"contract_sources":[],"a":2,"atomic_launch":{"z":3,"current_origin_status":"private","a":4},"repo":"https://github.com/dx-corp/mono.git"}"#;
        let output = public_content(
            source.path(),
            "docs/protocols/hosted-orb-delegation-acceptance-v1.json",
            bytes,
            b"",
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n\t\"z\": 1,\n\t\"a\": 2,\n\t\"atomic_launch\": {\n\t\t\"z\": 3,\n\t\t\"a\": 4\n\t},\n\t\"repo\": \"https://github.com/evalops/maestro.git\"\n}\n"
        );
    }

    #[test]
    fn ordinary_binary_content_survives_without_utf8_decoding() {
        let source = tempfile::tempdir().unwrap();
        let bytes = [0, 0xff, 0xfe, b'\n'];
        assert_eq!(
            public_content(source.path(), "image.png", &bytes, b"").unwrap(),
            bytes
        );
        let mut entries = Entries::new();
        entries.insert(
            "image.png".into(),
            Entry {
                content: b"\0cerebro".to_vec(),
                mode: 0o644,
            },
        );
        assert!(assert_cerebro_boundary(&entries).is_ok());
        entries.insert(
            "new-cerebro.txt".into(),
            Entry {
                content: vec![0],
                mode: 0o644,
            },
        );
        assert!(assert_cerebro_boundary(&entries).is_err());
    }

    #[test]
    fn removed_deny_manifest_mention_cannot_be_reintroduced() {
        let mut entries = Entries::new();
        entries.insert(
            "deny.toml".into(),
            Entry {
                content: b"# public dependencies\n".to_vec(),
                mode: 0o644,
            },
        );
        assert!(assert_cerebro_boundary(&entries).is_ok());
        entries.get_mut("deny.toml").unwrap().content = b"# Cerebro\n".to_vec();
        let error = assert_cerebro_boundary(&entries).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deny.toml: 1 mention(s), 0 allowed"),
            "{error}"
        );
    }

    #[test]
    fn cargo_sanitizer_removes_private_inputs_and_rejects_drift() {
        let manifest = "[workspace]\nmembers = [\n    \"packages/cerebro-host-rs\",\n]\nexclude = [\"vendor/*\"]\ntest-world = { git = \"https://github.com/evalops/test-world\", rev = \"abc\" }\n";
        let sanitized = sanitize_cargo("Cargo.toml", manifest).unwrap();
        assert!(!sanitized.contains("cerebro"));
        assert!(!sanitized.contains("test-world"));
        assert!(sanitized.contains("\"vendor/*\", \"vendor/dex-loop\"]"));
        assert!(sanitize_cargo("Cargo.toml", "test-world = { changed = true }\n").is_err());
        assert!(sanitize_cargo("packages/dex-host-rs/Cargo.toml", "changed").is_err());
    }

    #[test]
    fn lock_package_removal_keeps_adjacent_blocks_and_final_newline() {
        let lock = "header\n\n[[package]]\nname = \"private\"\nversion = \"1\"\n\n[[package]]\nname = \"public\"\n";
        assert_eq!(
            remove_lock_package(lock, "private"),
            "header\n\n[[package]]\nname = \"public\"\n"
        );
        assert_eq!(
            remove_lock_package(lock, "public"),
            "header\n\n[[package]]\nname = \"private\"\nversion = \"1\"\n"
        );
    }

    #[test]
    fn tls_provenance_removes_only_consumer_coordinates() {
        let source = tempfile::tempdir().unwrap();
        let bytes = br#"[{"sha":"abc","workspaces":["private"],"archive":"tls"}]"#;
        let output =
            public_content(source.path(), "vendor/tls-provenance.json", bytes, b"").unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[\n  {\n    \"sha\": \"abc\",\n    \"archive\": \"tls\"\n  }\n]\n"
        );
        assert!(
            public_content(
                source.path(),
                "vendor/tls-provenance.json",
                br#"[{"workspaces":42}]"#,
                b""
            )
            .is_err()
        );
    }

    #[test]
    fn collect_preserves_owned_paths_and_overlays_reviewed_release_and_root_helpers() {
        use std::os::unix::fs::PermissionsExt;
        let mono = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let source = mono.path().join("products/maestro");
        let write = |root: &Path, path: &str, bytes: &[u8]| {
            let absolute = root.join(path);
            fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            fs::write(absolute, bytes).unwrap();
        };
        write(&source, "package.json", br#"{"name":"fixture","maestro":{"canonicalPackageName":"public-fixture","packageAliases":["fixture","public-fixture","",42]}}"#);
        write(
            &source,
            ".github/release-mirror-manifest.json",
            br#"{"files":["scripts/deprecate-release.js"]}"#,
        );
        write(&source, "scripts/deprecate-release.js", b"release helper\n");
        write(&source, "AGENTS.md", b"private instructions\n");
        write(
            &source,
            ".github/public-repo/AGENTS.md",
            b"public instructions\n",
        );
        fs::set_permissions(source.join("AGENTS.md"), fs::Permissions::from_mode(0o755)).unwrap();
        write(&source, "docs/BUILD_TESTING.md", b"public build guide\n");
        write(&source, "docs/planning/private.md", b"private plan\n");
        write(&source, "asset.bin", &[0, 0xff, 0xfe]);
        write(mono.path(), "scripts/ci/helper.sh", b"root helper\n");
        write(
            target.path(),
            ".github/workflows/owned.yml",
            b"owned workflow\n",
        );
        write(
            target.path(),
            "docs/planning/stale.md",
            b"stale private plan\n",
        );
        write(target.path(), "removed.txt", b"removed\n");
        write(
            target.path(),
            "nested/gha-creds-fixture.json",
            b"credential artifact\n",
        );
        let definition: Definition = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1, "name": "deixic-code", "class": "source-tree",
            "mode": "maestro-public-tree-v1", "sourceRepository": "dx-corp/mono", "visibility": "public",
            "mappings": [
                {"source":"products/maestro", "destination":".", "include":["**"], "exclude":[]},
                {"source":"scripts/ci", "destination":"scripts/ci", "include":["helper.sh"], "exclude":[]}
            ],
            "destination": {"repository":"dx-corp/code", "branch":"main", "syncBranch":"sync/code", "holdLabel":"sync-hold"},
            "destinationOwned":[".github/workflows/**"], "deletion":"mirror-boundary",
            "provenance":".repository-projection.json", "validation":"maestro-prepared-tree-v1"
        })).unwrap();
        let owned = Matcher::new(&definition.destination_owned).unwrap();
        let (entries, deletions) =
            collect(&definition, mono.path(), target.path(), &owned).unwrap();
        assert_eq!(entries["AGENTS.md"].content, b"public instructions\n");
        assert_eq!(entries["AGENTS.md"].mode, 0o755);
        assert_eq!(entries["asset.bin"].content, [0, 0xff, 0xfe]);
        assert_eq!(
            entries["scripts/deprecate-release.js"].content,
            b"release helper\n"
        );
        assert_eq!(entries["scripts/ci/helper.sh"].content, b"root helper\n");
        assert!(entries.contains_key("docs/BUILD_TESTING.md"));
        assert!(!entries.contains_key("docs/planning/private.md"));
        assert!(!entries.contains_key(".github/release-mirror-manifest.json"));
        assert!(!entries.keys().any(|p| owned.matches(p)));
        assert_eq!(
            deletions,
            [
                "docs/planning/stale.md",
                "nested/gha-creds-fixture.json",
                "removed.txt"
            ]
        );
        assert_eq!(public_package_name(&source).unwrap(), "public-fixture");
        let package = parse_json(&entries["package.json"].content).unwrap();
        assert_eq!(
            get(get(&package, "maestro").unwrap(), "packageAliases"),
            Some(&Json::Array(vec![
                Json::String("public-fixture".into()),
                Json::String("fixture".into())
            ]))
        );
    }
}
