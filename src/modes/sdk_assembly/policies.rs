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
    "src/deixic/examples/__init__.py",
    "src/deixic/examples/account_brief.py",
    "src/deixic/examples/account_brief_result.py",
    "src/deixic/examples/task_result.py",
    "src/deixic/examples/verify_test_journey.py",
    "src/deixic/py.typed",
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
];

// Generated protobuf/gRPC Python modules under `gen/python/`, copied into
// `src/` (mirrors `PYTHON_GENERATED_FILES`).
pub const PYTHON_GENERATED_FILES: &[&str] = &[
    "agentruntime/v1/runtime_pb2.py",
    "agents/v1/agents_pb2.py",
    "buf/validate/validate_pb2.py",
    "codex/v1/codex_pb2.py",
    "common/v1/analytics_pb2.py",
    "common/v1/authz_pb2.py",
    "common/v1/classification_pb2.py",
    "common/v1/delivery_pb2.py",
    "common/v1/entity_pb2.py",
    "common/v1/risk_pb2.py",
    "common/v1/surface_pb2.py",
    "connectors/v1/connectors_pb2.py",
    "console/v1/console_pb2.py",
    "evalops_platform/v1/platform_pb2.py",
    "google/api/annotations_pb2.py",
    "google/api/http_pb2.py",
    "memory/v1/memory_pb2.py",
    "meter/v1/meter_pb2.py",
    "objectives/v1/objectives_pb2.py",
    "orbcontrol/v1/orb_control_pb2.py",
    "remoterunner/v1/remoterunner_pb2.py",
    "toolexecution/v1/toolexecution_pb2.py",
    "traces/v1/traces_pb2.py",
    "vfs/v1/filesystem_pb2.py",
];

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
    "test/account-brief-result.test.mjs",
    "test/account-brief.test.mjs",
    "test/client.test.mjs",
    "test/tasks.test.mjs",
    "tsconfig.json",
];

// index.ts is intentionally absent: the Deixic package imports this smaller
// reviewed helper closure directly instead of projecting the Maestro SDK.
pub const NODE_SHARED_FILES: &[&str] = &[
    "sdk/maestro/typescript/scripts/verify-descriptor-sources.mjs",
    "sdk/maestro/typescript/src/accepted-turn.ts",
    "sdk/maestro/typescript/src/app-context.ts",
    "sdk/maestro/typescript/src/client.ts",
    "sdk/maestro/typescript/src/errors.ts",
];

// Generated protobuf/gRPC TypeScript modules under `gen/ts/`, copied into
// place under the same relative path (mirrors `TYPESCRIPT_GENERATED_FILES`).
pub const TYPESCRIPT_GENERATED_FILES: &[&str] = &[
    "agentruntime/v1/runtime_pb.ts",
    "agents/v1/agents_pb.ts",
    "buf/validate/validate_pb.ts",
    "codex/v1/codex_pb.ts",
    "common/v1/analytics_pb.ts",
    "common/v1/authz_pb.ts",
    "common/v1/classification_pb.ts",
    "common/v1/delivery_pb.ts",
    "common/v1/entity_pb.ts",
    "common/v1/risk_pb.ts",
    "common/v1/surface_pb.ts",
    "connectors/v1/connectors_pb.ts",
    "console/v1/console_pb.ts",
    "deixic/v1/deixic_pb.ts",
    "google/api/annotations_pb.ts",
    "google/api/http_pb.ts",
    "memory/v1/memory_pb.ts",
    "meter/v1/meter_pb.ts",
    "objectives/v1/objectives_pb.ts",
    "orbcontrol/v1/orb_control_pb.ts",
    "platform/v1/platform_pb.ts",
    "remoterunner/v1/remoterunner_pb.ts",
    "toolexecution/v1/toolexecution_pb.ts",
    "traces/v1/traces_pb.ts",
    "vfs/v1/filesystem_pb.ts",
];

// Generated protobuf/gRPC Go modules under `gen/go/`, copied into place under
// the same relative path (mirrors `GO_GENERATED_FILES`).
pub const GO_GENERATED_FILES: &[&str] = &[
    "agentruntime/v1/runtime.pb.go",
    "agents/v1/agents.pb.go",
    "codex/v1/codex.pb.go",
    "common/v1/analytics.pb.go",
    "common/v1/authz.pb.go",
    "common/v1/classification.pb.go",
    "common/v1/delivery.pb.go",
    "common/v1/entity.pb.go",
    "common/v1/risk.pb.go",
    "common/v1/surface.pb.go",
    "connectors/v1/connectors.pb.go",
    "console/v1/console.pb.go",
    "deixic/v1/deixic.pb.go",
    "deixic/v1/deixicv1connect/deixic.connect.go",
    "memory/v1/memory.pb.go",
    "meter/v1/meter.pb.go",
    "objectives/v1/objectives.pb.go",
    "orbcontrol/v1/orb_control.pb.go",
    "platform/v1/platform.pb.go",
    "remoterunner/v1/remoterunner.pb.go",
    "toolexecution/v1/toolexecution.pb.go",
    "traces/v1/traces.pb.go",
    "vfs/v1/filesystem.pb.go",
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
    let mut copies: Vec<Copy> = PYTHON_SDK_FILES
        .iter()
        .map(|path| {
            copy(
                format!("sdk/deixic/python/{path}"),
                (*path).to_string(),
                (*path == "pyproject.toml").then_some(Transform::PythonPyproject),
            )
        })
        .collect();
    copies.extend(
        PYTHON_GENERATED_FILES
            .iter()
            .map(|path| copy(format!("gen/python/{path}"), format!("src/{path}"), None)),
    );
    copies
}

fn node_copies() -> Vec<Copy> {
    let mut copies: Vec<Copy> = NODE_PACKAGE_FILES
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
    copies.extend(
        NODE_SHARED_FILES
            .iter()
            .map(|path| copy((*path).to_string(), (*path).to_string(), None)),
    );
    copies.extend(TYPESCRIPT_GENERATED_FILES.iter().map(|path| {
        let full = format!("gen/ts/{path}");
        copy(full.clone(), full, None)
    }));
    copies
}

fn go_copies() -> Vec<Copy> {
    let mut copies = vec![
        copy("sdk/deixic/go/README.md", "README.md", None),
        copy("sdk/deixic/python/LICENSE", "LICENSE", None),
        copy(
            "sdk/deixic/go/deixic_connect_test.go.in",
            "deixic/v1/deixicv1connect/projection_test.go",
            None,
        ),
        copy("gen/go/CHANGELOG.md", "CHANGELOG.md", None),
        copy("gen/go/go.mod", "go.mod", Some(Transform::GoModule)),
        copy("gen/go/go.sum", "go.sum", None),
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
            &[
                "gen/ts/**",
                "sdk/deixic/typescript/**",
                "sdk/maestro/typescript/scripts/verify-descriptor-sources.mjs",
                "sdk/maestro/typescript/src/**",
            ],
        ),
        make_policy(
            "deixic-go",
            go_copies(),
            Closure::GoPackageImportsV1,
            &[
                "CHANGELOG.md",
                "LICENSE",
                "README.md",
                "agentruntime/**",
                "agents/**",
                "codex/**",
                "common/**",
                "connectors/**",
                "console/**",
                "deixic/**",
                "go.mod",
                "go.sum",
                "memory/**",
                "meter/**",
                "objectives/**",
                "orbcontrol/**",
                "platform/**",
                "remoterunner/**",
                "toolexecution/**",
                "traces/**",
                "vfs/**",
            ],
        ),
    ]
});

/// Looks up one of the three reviewed, immutable SDK assembly policies by
/// name. `None` for any other name.
pub fn policy(name: &str) -> Option<&'static Policy> {
    POLICIES.iter().find(|p| p.name == name)
}
