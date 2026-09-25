//! The three reviewed SDK assembly policies (python, node, go), transcribed
//! verbatim (file lists and copy shapes) from
//! `scripts/projections/sdk-assembly.mjs` in Mono. `super` (`sdk_assembly.rs`)
//! owns `assemble`, the transforms, and the closure validators; this module
//! only owns the reviewed, immutable shape of each policy.

use std::sync::LazyLock;

// The reviewed Deixic Python SDK's own files, copied verbatim under their
// own names (mirrors `PYTHON_SDK_FILES` in sdk-assembly.mjs).
pub const PYTHON_SDK_FILES: &[&str] = &[
    "CHANGELOG.md",
    "LICENSE",
    "README.md",
    "pyproject.toml",
    "src/deixic/__init__.py",
    "src/deixic/auth.py",
    "src/deixic/client.py",
    "src/deixic/errors.py",
    "src/deixic/federation.py",
    "src/deixic/examples/__init__.py",
    "src/deixic/examples/account_brief.py",
    "src/deixic/examples/account_brief_result.py",
    "src/deixic/examples/task_result.py",
    "src/deixic/examples/verify_test_journey.py",
    "src/deixic/py.typed",
    "src/deixic/protocol.py",
    "src/deixicpublic/__init__.py",
    "src/deixicpublic/v1/__init__.py",
    "src/deixicpublic/v1/sdk_pb2.py",
    "src/deixic/tasks.py",
    "src/deixic/transport.py",
    "tests/test_account_brief.py",
    "tests/test_account_brief_application.py",
    "tests/test_client.py",
    "tests/test_http_journey.py",
    "tests/test_real_journey.py",
    "tests/test_recovery.py",
    "tests/test_tasks.py",
    "tests/test_test_journey.py",
    "tests/test_workload_federation.py",
];

// Generated protobuf/gRPC Python modules under `gen/python/`, copied into
// `src/` (mirrors `PYTHON_GENERATED_FILES`).
// The reviewed Deixic Node package's own files (mirrors `NODE_PACKAGE_FILES`).
pub const NODE_PACKAGE_FILES: &[&str] = &[
    "CHANGELOG.md",
    "LICENSE",
    "README.md",
    "examples/account-brief-result.d.mts",
    "examples/account-brief-result.mjs",
    "examples/account-brief.mjs",
    "package-lock.json",
    "package.json",
    "scripts/check-package-exports.mjs",
    "scripts/smoke-packed-package.mjs",
    "src/index.ts",
    "src/tasks.ts",
    "src/client.ts",
    "src/errors.ts",
    "src/accepted-turn.ts",
    "src/app-context.ts",
    "src/protocol.ts",
    "test/account-brief-result.test.mjs",
    "test/account-brief.test.mjs",
    "test/client.test.mjs",
    "test/tasks.test.mjs",
    "tsconfig.json",
];

pub const GO_GENERATED_FILES: &[&str] = &[
    "deixicpublic/v1/sdk.pb.go",
    "deixicpublic/v1/deixicpublicv1connect/sdk.connect.go",
];

/// A byte-for-byte transform applied to one copy's content, keyed by the
/// destination identity it must rewrite. See `sdk_assembly::apply_transform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    PythonPyproject,
    NodePackage,
    GoModule,
    GoSource,
}

/// The reviewed import-closure check a policy's generated sources must
/// satisfy. See `sdk_assembly::validate_closure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closure {
    PythonGeneratedImportsV1,
    TypescriptCompiledImportsV1,
    GoPackageImportsV1,
}

/// One reviewed input -> output mapping within a policy (mirrors the `copy()`
/// helper in sdk-assembly.mjs).
#[derive(Debug, Clone)]
pub struct Copy {
    pub source: String,
    pub output: String,
    pub transform: Option<Transform>,
}

/// A named, reviewed SDK assembly policy: the exact set of source files it
/// reads, the exact set of output files it produces, the destination
/// repository's managed boundary, and the import-closure check its generated
/// sources must pass. Built once by `policy` and never mutated afterward.
#[derive(Debug)]
pub struct Policy {
    pub name: &'static str,
    pub input_roots: Vec<String>,
    pub output_include: Vec<String>,
    pub output_managed: Vec<String>,
    pub closure: Closure,
    pub copies: Vec<Copy>,
}

fn copy(
    source: impl Into<String>,
    output: impl Into<String>,
    transform: Option<Transform>,
) -> Copy {
    Copy {
        source: source.into(),
        output: output.into(),
        transform,
    }
}

fn python_copies() -> Vec<Copy> {
    let copies: Vec<Copy> = PYTHON_SDK_FILES
        .iter()
        .map(|path| {
            copy(
                format!("sdk/deixic/python/{path}"),
                (*path).to_string(),
                (*path == "pyproject.toml").then_some(Transform::PythonPyproject),
            )
        })
        .collect();
    copies
}

fn node_copies() -> Vec<Copy> {
    let copies: Vec<Copy> = NODE_PACKAGE_FILES
        .iter()
        .map(|path| {
            let full = format!("sdk/deixic/typescript/{path}");
            copy(
                full.clone(),
                full,
                (*path == "package.json").then_some(Transform::NodePackage),
            )
        })
        .collect();
    copies
}

fn go_copies() -> Vec<Copy> {
    let mut copies = vec![
        copy("sdk/deixic/go/README.md", "README.md", None),
        copy("sdk/deixic/python/LICENSE", "LICENSE", None),
        copy(
            "sdk/deixic/go/deixic_connect_test.go.in",
            "deixicpublic/v1/deixicpublicv1connect/projection_test.go",
            None,
        ),
        copy("sdk/deixic/go/go.mod", "go.mod", None),
        copy("sdk/deixic/go/go.sum", "go.sum", None),
    ];
    copies.extend(GO_GENERATED_FILES.iter().map(|path| {
        copy(
            format!("gen/go/{path}"),
            (*path).to_string(),
            Some(Transform::GoSource),
        )
    }));
    copies
}

fn make_policy(
    name: &'static str,
    copies: Vec<Copy>,
    closure: Closure,
    output_managed: &[&str],
) -> Policy {
    let mut input_roots: Vec<String> = copies.iter().map(|c| c.source.clone()).collect();
    input_roots.sort();
    let mut output_include: Vec<String> = copies.iter().map(|c| c.output.clone()).collect();
    output_include.sort();
    let mut output_managed: Vec<String> = output_managed.iter().map(|s| (*s).to_string()).collect();
    output_managed.sort();
    Policy {
        name,
        input_roots,
        output_include,
        output_managed,
        closure,
        copies,
    }
}

static POLICIES: LazyLock<Vec<Policy>> = LazyLock::new(|| {
    vec![
        make_policy(
            "deixic-python",
            python_copies(),
            Closure::PythonGeneratedImportsV1,
            &[
                "CHANGELOG.md",
                "LICENSE",
                "README.md",
                "pyproject.toml",
                "src/**",
                "tests/**",
            ],
        ),
        make_policy(
            "deixic-node",
            node_copies(),
            Closure::TypescriptCompiledImportsV1,
            &["sdk/deixic/typescript/**"],
        ),
        make_policy(
            "deixic-go",
            go_copies(),
            Closure::GoPackageImportsV1,
            &[
                "CHANGELOG.md",
                "LICENSE",
                "README.md",
                "deixicpublic/**",
                // These were copied by the former broad Go projection. Keep
                // ownership until Capobara deletes them from the public repo.
                "agentruntime/**",
                "agents/**",
                "codex/**",
                "common/**",
                "connectors/**",
                "console/**",
                "deixic/**",
                "memory/**",
                "meter/**",
                "objectives/**",
                "orbcontrol/**",
                "platform/**",
                "remoterunner/**",
                "toolexecution/**",
                "traces/**",
                "vfs/**",
                "go.mod",
                "go.sum",
            ],
        ),
    ]
});

/// Looks up one of the three reviewed, immutable SDK assembly policies by
/// name. `None` for any other name.
pub fn policy(name: &str) -> Option<&'static Policy> {
    POLICIES.iter().find(|p| p.name == name)
}
