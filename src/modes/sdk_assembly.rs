//! `sdk-assembly-v1`: deterministic assembly of a standalone SDK repository
//! from an immutable Mono snapshot. Ports `assembleSdkProjection` and its
//! transforms and import-closure validators from
//! `scripts/projections/sdk-assembly.mjs` (Node, the source of record).
//!
//! The three reviewed policies (`deixic-python`, `deixic-node`, `deixic-go`)
//! live in `policies`; this module owns applying them: reading each copy's
//! source bytes from the snapshot, transforming destination-identity content
//! (`apply_transform`), checking that every generated source's import graph
//! stays inside the reviewed closure (`validate_closure`), and checking that
//! the actual set of inputs read and outputs produced matches the policy's
//! registered, sorted lists exactly (`require_same_set`) before handing back
//! an `Assembled`.

pub mod policies;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};

pub use policies::{Closure, Copy, Policy, Transform, policy};

use super::Assembled;
use crate::tree::{Entries, Entry, read_entry};
use crate::{Error, Result, contract};

/// The `sdk_inputs` closure `definition::load_definition`,
/// `definition::validate_definition`, and `definition::projection_input_roots`
/// take: the sorted, reviewed input paths for a named SDK assembly policy, or
/// `None` for an unknown name.
pub fn input_roots(name: &str) -> Option<Vec<String>> {
    policy(name).map(|p| p.input_roots.clone())
}

/// Assembles the named policy's output tree from `snapshot`. Fails closed
/// (`Error::Contract`) on a duplicate input/output, a missing snapshot input,
/// a transform whose precondition no longer holds, an import that escapes
/// the reviewed generated-dependency closure, or an actual input/output set
/// that has drifted from the policy's registered, reviewed lists.
pub fn assemble(snapshot: &Path, name: &str) -> Result<Assembled> {
    let selected = policy(name)
        .ok_or_else(|| Error::Invalid(format!("Unknown SDK assembly policy: {name}")))?;

    let mut inputs: Vec<String> = Vec::with_capacity(selected.copies.len());
    let mut source_entries: HashMap<String, Entry> = HashMap::with_capacity(selected.copies.len());
    let mut entries: Entries = Entries::new();

    for mapping in &selected.copies {
        contract(
            !source_entries.contains_key(&mapping.source),
            format!("Duplicate SDK input: {}", mapping.source),
        )?;
        contract(
            !entries.contains_key(&mapping.output),
            format!("Duplicate SDK output: {}", mapping.output),
        )?;
        let entry = read_entry(snapshot, &mapping.source)?;
        let content = apply_transform(mapping.transform, &entry.content)?;
        let mode = entry.mode;
        entries.insert(mapping.output.clone(), Entry { content, mode });
        inputs.push(mapping.source.clone());
        source_entries.insert(mapping.source.clone(), entry);
    }

    validate_closure(selected, &source_entries)?;
    require_same_set(
        &inputs,
        &selected.input_roots,
        &format!("{} input roots", selected.name),
    )?;
    let outputs: Vec<String> = entries.keys().cloned().collect();
    require_same_set(
        &outputs,
        &selected.output_include,
        &format!("{} output allowlist", selected.name),
    )?;

    Ok(Assembled {
        entries,
        output_include: selected.output_include.clone(),
        output_managed: selected.output_managed.clone(),
    })
}

fn apply_transform(transform: Option<Transform>, content: &[u8]) -> Result<Vec<u8>> {
    let Some(transform) = transform else {
        return Ok(content.to_vec());
    };
    let text = String::from_utf8_lossy(content).into_owned();
    match transform {
        Transform::PythonPyproject => {
            static DEPENDENCY: LazyLock<Regex> = LazyLock::new(|| {
                Regex::new(r#"(?m)^  "evalops-sdk[^"]+",\n"#).expect("static regex is valid")
            });
            contract(
                DEPENDENCY.find_iter(&text).count() == 1,
                "Python generated dependency declaration changed",
            )?;
            let without_dependency = DEPENDENCY.replace(&text, "");
            Ok(without_dependency
                .replace(
                    "https://github.com/dx-corp/mono",
                    "https://github.com/dx-corp/deixic-python",
                )
                .into_bytes())
        }
        Transform::NodePackage => {
            contract(
                text.contains("https://github.com/dx-corp/mono"),
                "Node repository metadata changed",
            )?;
            Ok(text
                .replace(
                    "https://github.com/dx-corp/mono",
                    "https://github.com/dx-corp/deixic-node",
                )
                .into_bytes())
        }
        Transform::GoModule => {
            const HEADER: &str = "module github.com/evalops/platform/gen/go\n";
            contract(
                text.starts_with(HEADER),
                "Go source module identity changed",
            )?;
            Ok(text
                .replacen(HEADER, "module github.com/dx-corp/deixic-go\n", 1)
                .into_bytes())
        }
        Transform::GoSource => Ok(transform_go_imports(&text).into_bytes()),
    }
}

fn transform_go_imports(text: &str) -> String {
    static IMPORT_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^import \(\n((?s:.)*?)^\)\n").expect("static regex is valid")
    });
    IMPORT_BLOCK
        .replacen(text, 1, |caps: &Captures<'_>| {
            let imports = caps[1].replace(
                "github.com/evalops/platform/gen/go",
                "github.com/dx-corp/deixic-go",
            );
            format!("import (\n{imports})\n")
        })
        .into_owned()
}

fn validate_closure(policy: &Policy, source_entries: &HashMap<String, Entry>) -> Result<()> {
    match policy.closure {
        Closure::PythonGeneratedImportsV1 => validate_python_closure(source_entries),
        Closure::TypescriptCompiledImportsV1 => validate_typescript_closure(source_entries),
        Closure::GoPackageImportsV1 => validate_go_closure(source_entries),
    }
}

fn validate_python_closure(source_entries: &HashMap<String, Entry>) -> Result<()> {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^from ([A-Za-z0-9_.]+) import ([A-Za-z0-9_]+_pb2)\b")
            .expect("static regex is valid")
    });
    const PREFIX: &str = "gen/python/";
    let allowed: HashSet<String> = policies::PYTHON_GENERATED_FILES
        .iter()
        .map(|path| format!("{PREFIX}{path}"))
        .collect();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![format!("{PREFIX}console/v1/console_pb2.py")];
    while let Some(path) = queue.pop() {
        if visited.contains(&path) {
            continue;
        }
        contract(
            allowed.contains(&path),
            format!("Python generated import escapes reviewed closure: {path}"),
        )?;
        visited.insert(path.clone());
        let text = source_text(source_entries, &path)?;
        for caps in IMPORT.captures_iter(&text) {
            let package_name = &caps[1];
            let module_name = &caps[2];
            if package_name.starts_with("google.protobuf") {
                continue;
            }
            let imported = format!(
                "{PREFIX}{}/{module_name}.py",
                package_name.replace('.', "/")
            );
            contract(
                allowed.contains(&imported),
                format!("Python generated import escapes reviewed closure: {imported}"),
            )?;
            queue.push(imported);
        }
    }
    require_same_set(
        &visited.into_iter().collect::<Vec<_>>(),
        &allowed.into_iter().collect::<Vec<_>>(),
        "Python generated dependency closure",
    )
}

fn validate_typescript_closure(source_entries: &HashMap<String, Entry>) -> Result<()> {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)(?:from\s+|import\s*\()["']([^"']+)["']"#).expect("static regex is valid")
    });
    let build_roots = [
        "sdk/deixic/typescript/src/index.ts".to_string(),
        "sdk/deixic/typescript/src/tasks.ts".to_string(),
    ];
    let mut allowed: HashSet<String> = build_roots.iter().cloned().collect();
    allowed.extend(
        policies::NODE_SHARED_FILES
            .iter()
            .filter(|path| path.ends_with(".ts"))
            .map(|path| (*path).to_string()),
    );
    allowed.extend(
        policies::TYPESCRIPT_GENERATED_FILES
            .iter()
            .map(|path| format!("gen/ts/{path}")),
    );
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = build_roots.to_vec();
    while let Some(path) = queue.pop() {
        if visited.contains(&path) {
            continue;
        }
        contract(
            allowed.contains(&path),
            format!("TypeScript import escapes reviewed closure: {path}"),
        )?;
        visited.insert(path.clone());
        let text = source_text(source_entries, &path)?;
        for caps in IMPORT.captures_iter(&text) {
            let specifier = &caps[1];
            if !specifier.starts_with('.') {
                continue;
            }
            let resolved_specifier = match specifier.strip_suffix(".js") {
                Some(stem) => format!("{stem}.ts"),
                None => specifier.to_string(),
            };
            let imported = posix_normalize(&posix_join(&posix_dirname(&path), &resolved_specifier));
            contract(
                !imported.starts_with("../"),
                format!("TypeScript import escapes projection: {path}"),
            )?;
            contract(
                allowed.contains(&imported),
                format!("TypeScript import escapes reviewed closure: {imported}"),
            )?;
            queue.push(imported);
        }
    }
    require_same_set(
        &visited.into_iter().collect::<Vec<_>>(),
        &allowed.into_iter().collect::<Vec<_>>(),
        "TypeScript compiled dependency closure",
    )
}

fn validate_go_closure(source_entries: &HashMap<String, Entry>) -> Result<()> {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)"(github\.com/evalops/platform/gen/go/[^"\s]+)""#)
            .expect("static regex is valid")
    });
    const PREFIX: &str = "gen/go/";
    const MODULE_PREFIX: &str = "github.com/evalops/platform/gen/go/";
    let allowed: HashSet<String> = policies::GO_GENERATED_FILES
        .iter()
        .map(|path| format!("{PREFIX}{path}"))
        .collect();
    let mut files_by_package: HashMap<String, Vec<String>> = HashMap::new();
    for path in &allowed {
        let package_path = posix_dirname(&path[PREFIX.len()..]);
        files_by_package
            .entry(package_path)
            .or_default()
            .push(path.clone());
    }
    let mut visited_files: HashSet<String> = HashSet::new();
    let mut visited_packages: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![
        "deixic/v1".to_string(),
        "deixic/v1/deixicv1connect".to_string(),
    ];
    while let Some(package_path) = queue.pop() {
        if visited_packages.contains(&package_path) {
            continue;
        }
        let files = files_by_package.get(&package_path);
        contract(
            files.is_some_and(|f| !f.is_empty()),
            format!("Go import escapes reviewed closure: {package_path}"),
        )?;
        visited_packages.insert(package_path.clone());
        for path in files.expect("checked by the contract() call above") {
            visited_files.insert(path.clone());
            let text = source_text(source_entries, path)?;
            for caps in IMPORT.captures_iter(&text) {
                let imported = &caps[1][MODULE_PREFIX.len()..];
                contract(
                    files_by_package.contains_key(imported),
                    format!("Go import escapes reviewed closure: {imported}"),
                )?;
                queue.push(imported.to_string());
            }
        }
    }
    require_same_set(
        &visited_files.into_iter().collect::<Vec<_>>(),
        &allowed.into_iter().collect::<Vec<_>>(),
        "Go generated dependency closure",
    )
}

fn source_text(source_entries: &HashMap<String, Entry>, path: &str) -> Result<String> {
    let entry = source_entries
        .get(path)
        .ok_or_else(|| Error::Contract(format!("Missing SDK assembly input: {path}")))?;
    Ok(String::from_utf8_lossy(&entry.content).into_owned())
}

/// Minimal POSIX path helpers matching Node's `node:path/posix` behavior for
/// the forward-slash, extension-swapped specifiers the TypeScript closure
/// resolves (`posix.dirname`, `posix.join`, `posix.normalize`).
fn posix_dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx].to_string(),
        None => ".".to_string(),
    }
}

fn posix_join(a: &str, b: &str) -> String {
    if a.is_empty() || a == "." {
        b.to_string()
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}/{b}")
    }
}

fn posix_normalize(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => match out.last() {
                Some(&last) if last != ".." => {
                    out.pop();
                }
                _ => {
                    if !is_absolute {
                        out.push("..");
                    }
                }
            },
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if is_absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

fn require_same_set(actual: &[String], expected: &[String], label: &str) -> Result<()> {
    let mut actual_sorted = actual.to_vec();
    actual_sorted.sort();
    let mut expected_sorted = expected.to_vec();
    expected_sorted.sort();
    contract(
        actual_sorted == expected_sorted,
        format!(
            "{label} changed: expected {}, received {}",
            serde_json::to_string(&expected_sorted).unwrap_or_default(),
            serde_json::to_string(&actual_sorted).unwrap_or_default(),
        ),
    )
}
