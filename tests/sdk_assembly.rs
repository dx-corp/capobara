//! Ports the four tests in `scripts/projections/sdk-assembly.test.mjs`
//! (Node, the source of record) plus one integration test proving
//! `definition::load_definition` accepts a real `sdk-assembly-v1` definition
//! when wired to `sdk_assembly::input_roots`.
//!
//! Node's tests read fixtures straight out of the live Mono tree (the
//! projection source itself). This crate has no such tree to read from, so
//! each test here builds a synthetic snapshot directory under a tempdir
//! containing every input file every policy names, with just enough content
//! to satisfy each policy's import-closure check: the Python closure's BFS
//! root (`console_pb2.py`) imports every other generated Python module
//! directly; the TypeScript closure's two roots (`index.ts`, `tasks.ts`)
//! re-export every shared and generated TypeScript module by a relative,
//! `.js`-suffixed specifier (mirroring the real compiled-output convention
//! the closure's extension swap expects); and the Go closure's `deixic/v1`
//! root imports every other generated Go package by its full module path.

mod support;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use capobara::modes::sdk_assembly::{self, policies};
use support::Repo;

// ---------------------------------------------------------------------
// Synthetic snapshot construction
// ---------------------------------------------------------------------

fn write(root: &Path, path: &str, contents: &[u8]) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, contents).unwrap();
}

fn make_executable(root: &Path, path: &str) {
    let full = root.join(path);
    let mut perms = std::fs::metadata(&full).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&full, perms).unwrap();
}

/// The relative, POSIX-style specifier `from_file` would use to import
/// `to_file` (both full repo-relative paths, `to_file` still carrying its
/// real extension) -- e.g. `sdk/deixic/typescript/src/index.ts` importing
/// `gen/ts/buf/validate/validate_pb.ts` yields
/// `../../../../gen/ts/buf/validate/validate_pb.ts`.
fn relative_specifier(from_file: &str, to_file: &str) -> String {
    let mut from_dir: Vec<&str> = from_file.split('/').collect();
    from_dir.pop();
    let mut to_parts: Vec<&str> = to_file.split('/').collect();
    let to_name = to_parts.pop().unwrap();
    let mut common = 0;
    while common < from_dir.len() && common < to_parts.len() && from_dir[common] == to_parts[common]
    {
        common += 1;
    }
    let ups = from_dir.len() - common;
    let mut segments: Vec<String> = (0..ups).map(|_| "..".to_string()).collect();
    segments.extend(to_parts[common..].iter().map(|s| s.to_string()));
    segments.push(to_name.to_string());
    segments.join("/")
}

/// Swaps a `.ts` specifier for the `.js` one the real compiled output (and
/// therefore the closure's own extension-swap resolution) expects.
fn js_specifier(ts_path: &str) -> String {
    match ts_path.strip_suffix(".ts") {
        Some(stem) => format!("{stem}.js"),
        None => ts_path.to_string(),
    }
}

fn python_package_and_module(generated_path: &str) -> (String, String) {
    let without_ext = generated_path.strip_suffix(".py").unwrap();
    match without_ext.rsplit_once('/') {
        Some((dir, module)) => (dir.replace('/', "."), module.to_string()),
        None => (String::new(), without_ext.to_string()),
    }
}

fn go_package_of(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx].to_string(),
        None => String::new(),
    }
}

const PYPROJECT_TOML: &str = concat!(
    "[project]\n",
    "name = \"deixic\"\n",
    "version = \"0.1.0\"\n",
    "dependencies = [\n",
    "  \"httpx>=0.27\",\n",
    "  \"evalops-sdk-core==1.2.3\",\n",
    "]\n",
    "\n",
    "[project.urls]\n",
    "Repository = \"https://github.com/dx-corp/mono\"\n",
);

const PACKAGE_JSON: &str = concat!(
    "{\n",
    "  \"name\": \"@dx-corp/deixic\",\n",
    "  \"version\": \"0.1.0\",\n",
    "  \"repository\": {\n",
    "    \"type\": \"git\",\n",
    "    \"url\": \"https://github.com/dx-corp/mono\"\n",
    "  }\n",
    "}\n",
);

const GO_MOD: &str = "module github.com/evalops/platform/gen/go\n\ngo 1.21\n";

/// Builds a tempdir snapshot containing every input every policy names,
/// shaped so all three closures pass. Reused by every test below except the
/// closure-escape test, which starts from this and mutates one file.
fn build_snapshot() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    populate_snapshot(dir.path());
    dir
}

/// Writes every input file every policy names into `root`. Split out of
/// `build_snapshot` so the end-to-end `apply` test below can populate a real
/// git working tree instead of a bare tempdir.
fn populate_snapshot(root: &Path) {
    // ---- Python ----
    for path in policies::PYTHON_SDK_FILES {
        let full = format!("sdk/deixic/python/{path}");
        if *path == "pyproject.toml" {
            write(root, &full, PYPROJECT_TOML.as_bytes());
        } else {
            write(root, &full, format!("# {path}\n").as_bytes());
        }
    }
    for path in policies::PYTHON_GENERATED_FILES {
        let full = format!("gen/python/{path}");
        if *path == "console/v1/console_pb2.py" {
            let mut content = String::from("# generated console module\n");
            for other in policies::PYTHON_GENERATED_FILES {
                if *other == *path {
                    continue;
                }
                let (package, module) = python_package_and_module(other);
                content.push_str(&format!("from {package} import {module}\n"));
            }
            write(root, &full, content.as_bytes());
        } else {
            write(root, &full, b"# generated\n");
        }
    }

    // ---- Node / TypeScript ----
    let index_path = "sdk/deixic/typescript/src/index.ts";
    for path in policies::NODE_PACKAGE_FILES {
        let full = format!("sdk/deixic/typescript/{path}");
        match *path {
            "package.json" => write(root, &full, PACKAGE_JSON.as_bytes()),
            "src/index.ts" => {} // written below, once all specifiers are known
            "src/tasks.ts" => write(root, &full, b"export {};\n"),
            "examples/account-brief.mjs" => {
                write(root, &full, b"#!/usr/bin/env node\nconsole.log(\"ok\");\n");
                make_executable(root, &full);
            }
            other => write(root, &full, format!("// {other}\n").as_bytes()),
        }
    }
    let mut index_content = String::new();
    for shared in policies::NODE_SHARED_FILES
        .iter()
        .filter(|path| path.ends_with(".ts"))
    {
        let spec = js_specifier(&relative_specifier(index_path, shared));
        index_content.push_str(&format!("export * from \"{spec}\";\n"));
    }
    for generated in policies::TYPESCRIPT_GENERATED_FILES {
        let full = format!("gen/ts/{generated}");
        let spec = js_specifier(&relative_specifier(index_path, &full));
        index_content.push_str(&format!("export * from \"{spec}\";\n"));
    }
    write(root, index_path, index_content.as_bytes());
    for path in policies::NODE_SHARED_FILES {
        write(root, path, format!("// {path}\n").as_bytes());
    }
    for path in policies::TYPESCRIPT_GENERATED_FILES {
        write(root, &format!("gen/ts/{path}"), b"// generated\n");
    }

    // ---- Go ----
    write(root, "sdk/deixic/go/README.md", b"# deixic-go\n");
    write(
        root,
        "sdk/deixic/go/deixic_connect_test.go.in",
        b"package deixicv1connect_test\n\nfunc TestProjection(t *testing.T) {}\n",
    );
    write(root, "gen/go/CHANGELOG.md", b"# changelog\n");
    write(root, "gen/go/go.mod", GO_MOD.as_bytes());
    write(root, "gen/go/go.sum", b"\n");

    let hub_package: &str = "deixic/v1";
    let mut other_packages: BTreeSet<String> = policies::GO_GENERATED_FILES
        .iter()
        .map(|path| go_package_of(path))
        .collect();
    other_packages.remove(hub_package);
    other_packages.remove("deixic/v1/deixicv1connect");
    let mut import_block = String::new();
    for package in &other_packages {
        import_block.push_str(&format!(
            "\t\"github.com/evalops/platform/gen/go/{package}\"\n"
        ));
    }
    let deixic_pb_go =
        format!("package deixicv1\n\nimport (\n{import_block})\n\ntype Placeholder struct{{}}\n");
    for path in policies::GO_GENERATED_FILES {
        let full = format!("gen/go/{path}");
        if *path == "deixic/v1/deixic.pb.go" {
            write(root, &full, deixic_pb_go.as_bytes());
        } else {
            write(root, &full, b"// generated\n");
        }
    }
}

fn fixture(name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn assert_unique(values: &[String]) {
    let unique: BTreeSet<&String> = values.iter().collect();
    assert_eq!(unique.len(), values.len(), "duplicates in {values:?}");
}

const POLICY_NAMES: [&str; 3] = ["deixic-python", "deixic-node", "deixic-go"];

// ---------------------------------------------------------------------
// Ported Node tests
// ---------------------------------------------------------------------

#[test]
fn sdk_policies_expose_reviewed_immutable_inputs_and_fixture_outputs() {
    let fixture = fixture("sdk-assembly.json");
    assert_eq!(fixture["schemaVersion"], 1);

    let mut policy_names: Vec<&str> = POLICY_NAMES.to_vec();
    policy_names.sort_unstable();
    let mut fixture_names: Vec<String> = fixture["outputs"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    fixture_names.sort();
    assert_eq!(policy_names, fixture_names);

    for name in POLICY_NAMES {
        let policy = sdk_assembly::policy(name).unwrap();
        let expected: Vec<String> = fixture["outputs"][name]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(policy.output_include, expected);
        assert_unique(&policy.input_roots);
        assert_unique(&policy.output_include);
        assert_unique(&policy.output_managed);
    }
}

#[test]
fn sdk_assembly_is_byte_deterministic_and_preserves_normalized_modes() {
    let snapshot = build_snapshot();
    let fixture = fixture("sdk-assembly.json");

    for name in POLICY_NAMES {
        let policy = sdk_assembly::policy(name).unwrap();
        let first = sdk_assembly::assemble(snapshot.path(), name).unwrap();
        let second = sdk_assembly::assemble(snapshot.path(), name).unwrap();
        assert_eq!(
            capobara::tree::tree_digest(&first.entries),
            capobara::tree::tree_digest(&second.entries),
        );
        let mut keys: Vec<String> = first.entries.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, policy.output_include);

        let mut executables: Vec<String> = Vec::new();
        for (path, entry) in &first.entries {
            assert!(
                entry.mode == 0o644 || entry.mode == 0o755,
                "unexpected mode {:o} for {path}",
                entry.mode
            );
            if entry.mode == 0o755 {
                executables.push(path.clone());
            }
        }
        executables.sort();
        let expected_executables: Vec<String> = fixture["executables"][name]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(executables, expected_executables, "policy {name}");
    }
}

#[test]
fn language_specific_assembly_removes_unpublished_and_mono_only_identities() {
    let snapshot = build_snapshot();

    let python = sdk_assembly::assemble(snapshot.path(), "deixic-python").unwrap();
    let pyproject = String::from_utf8_lossy(&python.entries["pyproject.toml"].content).into_owned();
    assert!(!pyproject.contains("evalops-sdk"));
    assert!(pyproject.contains("github.com/dx-corp/deixic-python"));

    let node = sdk_assembly::assemble(snapshot.path(), "deixic-node").unwrap();
    let package_json =
        String::from_utf8_lossy(&node.entries["sdk/deixic/typescript/package.json"].content)
            .into_owned();
    assert!(package_json.contains("github.com/dx-corp/deixic-node"));
    assert!(
        node.entries
            .keys()
            .filter(|path| path.starts_with("sdk/maestro/"))
            .all(|path| path.contains("/typescript/src/")
                || path.ends_with("verify-descriptor-sources.mjs"))
    );
    assert!(
        !node
            .entries
            .keys()
            .any(|path| path.starts_with("products/"))
    );

    let go = sdk_assembly::assemble(snapshot.path(), "deixic-go").unwrap();
    let policy = sdk_assembly::policy("deixic-go").unwrap();
    assert!(
        policy
            .input_roots
            .contains(&"sdk/deixic/go/deixic_connect_test.go.in".to_string())
    );
    let go_test_template = std::fs::read(
        snapshot
            .path()
            .join("sdk/deixic/go/deixic_connect_test.go.in"),
    )
    .unwrap();
    assert_eq!(
        go.entries["deixic/v1/deixicv1connect/projection_test.go"].content,
        go_test_template
    );
    let go_mod = String::from_utf8_lossy(&go.entries["go.mod"].content).into_owned();
    assert!(go_mod.starts_with("module github.com/dx-corp/deixic-go\n"));
    let leaked = regex::Regex::new(
        r#"(?m)^(\s*(?:[_A-Za-z][A-Za-z0-9_]*\s+)?")github\.com/evalops/platform/gen/go"#,
    )
    .unwrap();
    for (path, entry) in &go.entries {
        if path.ends_with(".go") {
            let text = String::from_utf8_lossy(&entry.content);
            assert!(!leaked.is_match(&text), "{path}");
        }
    }
}

#[test]
fn generated_dependency_closure_fails_closed_when_an_import_escapes() {
    let snapshot = build_snapshot();
    let console_path = snapshot.path().join("gen/python/console/v1/console_pb2.py");
    let mut content = std::fs::read_to_string(&console_path).unwrap();
    content.push_str("from private.v1 import staff_pb2\n");
    std::fs::write(&console_path, content).unwrap();

    let err = sdk_assembly::assemble(snapshot.path(), "deixic-python").unwrap_err();
    assert_eq!(
        err.to_string(),
        "Python generated import escapes reviewed closure: gen/python/private/v1/staff_pb2.py"
    );
}

// ---------------------------------------------------------------------
// definition.rs wiring: proves sdk_assembly::input_roots is a drop-in
// sdk_inputs closure for a real, reviewed definition. Gated because it
// reads a definition fixture whose input allowlist must match the real
// policy exactly; run with `--features mono-fixtures`.
// ---------------------------------------------------------------------

#[test]
#[cfg_attr(not(feature = "mono-fixtures"), ignore)]
fn deixic_python_definition_loads_against_the_real_sdk_assembly_policy() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/definitions/deixic-python.json");
    let loaded = capobara::definition::load_definition(&path, &sdk_assembly::input_roots).unwrap();
    assert_eq!(loaded.definition.name, "deixic-python");
    assert!(matches!(
        loaded.definition.mode,
        capobara::definition::Mode::SdkAssemblyV1
    ));
}

// ---------------------------------------------------------------------
// cli wiring: the production binary must assemble an `sdk-assembly-v1`
// projection through these reviewed policies.
//
// Regression. Between the task that added `plan`/`apply`/`verify`/`check`
// and the task that added the policies, `cli::project` carried a constant
// `None` policy lookup and an `unreachable!()` assembler. Nothing replaced
// them when the policies landed, so on `origin/main` every one of the three
// `sdk-assembly-v1` projections -- deixic-python, deixic-node, deixic-go --
// exited 2 with "Unknown SDK assembly policy" while Node's `project.mjs`
// applied them, and `capobara catalog check` failed outright where Node
// validated all ten entries. The equivalence harness found it; this test
// replays it against the real binary, the real reviewed policy, and the real
// committed definition, so the two holes cannot reopen independently: a
// stand-in lookup fails the definition load, and a stand-in assembler panics
// in the `sdk-assembly-v1` branch of `build_projection`.
// ---------------------------------------------------------------------

fn capobara() -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test executes the capobara binary"
    )]
    Command::new(env!("CARGO_BIN_EXE_capobara"))
}

// Debug-only, like `tests/project_cli.rs`: the synthetic source repository's
// `rust/tools/capobara` tree id is generated fresh here and can never match
// this build's embedded one, so the test supplies it through
// `CAPOBARA_TREE_ID_OVERRIDE`, which `tooldigest::embedded()` honours only
// under `cfg!(debug_assertions)`. Under `cargo test --release` this fails with
// "projector differs from source revision"; that is the override being
// compiled out of release builds on purpose, not a broken test.
#[test]
// Reads `tests/fixtures/definitions/deixic-python.json`, which
// `config/projections/capobara.json` deliberately EXCLUDES from the
// projection. The file therefore exists in Mono and not in
// `dx-corp/capobara`, so without this gate `cargo test` on a fresh clone of
// the published repository fails here with `No such file or directory` --
// the same defect class as the equivalence test's `EQUIVALENCE.md` read.
// `tests/standalone_build.rs` now runs the projected suite rather than only
// compiling it, which is what surfaced this.
#[cfg_attr(
    not(feature = "mono-fixtures"),
    ignore = "reads tests/fixtures/definitions/, which the projection excludes"
)]
fn apply_assembles_a_real_sdk_assembly_projection_through_the_reviewed_policy() {
    let definition_text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/definitions/deixic-python.json"),
    )
    .unwrap();

    let source = Repo::init("https://github.com/dx-corp/mono.git");
    populate_snapshot(source.path());
    source.write(
        "config/projections/deixic-python.json",
        definition_text.as_bytes(),
    );
    source.write(
        "rust/tools/capobara/src/lib.rs",
        b"// stand-in for the crate tree",
    );
    let sha = source.commit("source");
    let tree_id = source
        .git(&["rev-parse", "HEAD:rust/tools/capobara"])
        .trim()
        .to_owned();

    let target = Repo::init("https://github.com/dx-corp/deixic-python.git");
    target.write("SECURITY.md", b"destination owned");
    let base = target.commit("destination");
    target.set_remote_main(&base);

    let output = capobara()
        .env("CAPOBARA_TREE_ID_OVERRIDE", &tree_id)
        .args(["apply", "--definition"])
        .arg(source.path().join("config/projections/deixic-python.json"))
        .arg("--source")
        .arg(source.path())
        .arg("--source-sha")
        .arg(&sha)
        .arg("--target")
        .arg(target.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "apply failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The destination now holds exactly the reviewed policy's outputs, the
    // receipt, and the destination-owned file it started with -- nothing
    // else, and nothing missing.
    let policy = policies::policy("deixic-python").unwrap();
    let mut expected: Vec<String> = policy.output_include.clone();
    expected.push(".repository-projection.json".to_owned());
    expected.push("SECURITY.md".to_owned());
    expected.sort();
    let mut actual: Vec<String> = Vec::new();
    let mut stack = vec![target.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                actual.push(
                    path.strip_prefix(target.path())
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    actual.sort();
    assert_eq!(actual, expected);

    // A generated module is copied verbatim under `src/`, the destination
    // identity transform has run on `pyproject.toml`, and the
    // destination-owned file is untouched.
    assert_eq!(
        std::fs::read(target.path().join("src/meter/v1/meter_pb2.py")).unwrap(),
        std::fs::read(source.path().join("gen/python/meter/v1/meter_pb2.py")).unwrap(),
    );
    let pyproject = std::fs::read_to_string(target.path().join("pyproject.toml")).unwrap();
    assert!(!pyproject.contains("dx-corp/mono"), "{pyproject}");
    assert_eq!(
        std::fs::read(target.path().join("SECURITY.md")).unwrap(),
        b"destination owned",
    );

    // The receipt records the projection that actually ran.
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(target.path().join(".repository-projection.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["projection"], "deixic-python");
    assert_eq!(receipt["sourceSha"], sha);
    assert_eq!(receipt["destinationRepository"], "dx-corp/deixic-python");

    // Positive control: the same binary, same source, same destination, but
    // an unregistered policy name is still rejected -- the lookup is the
    // reviewed one, not "accept anything".
    let renamed = definition_text.replace("deixic-python", "deixic-perl");
    source.write("config/projections/deixic-perl.json", renamed.as_bytes());
    let renamed_sha = source.commit("unregistered policy");
    let renamed_target = Repo::init("https://github.com/dx-corp/deixic-perl.git");
    let renamed_base = renamed_target.commit("destination");
    renamed_target.set_remote_main(&renamed_base);
    let output = capobara()
        .env("CAPOBARA_TREE_ID_OVERRIDE", &tree_id)
        .args(["apply", "--definition"])
        .arg(source.path().join("config/projections/deixic-perl.json"))
        .arg("--source")
        .arg(source.path())
        .arg("--source-sha")
        .arg(&renamed_sha)
        .arg("--target")
        .arg(renamed_target.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "Unknown SDK assembly policy"
    );
}
