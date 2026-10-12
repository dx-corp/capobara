//! Single integration-test binary for `capobara`.
//!
//! Cargo compiles and links one binary per `tests/*.rs` file. Folding the files
//! into modules of one target keeps every test while paying codegen and link
//! cost once; filters still work with a module prefix, for example
//! `cargo test -p capobara --test suite <module>::`.
//!
//! `transport_git`, `transport_github` and `run_cli` stay separate targets: they
//! need the `recorded-api` feature, `transport_github` owns an insta snapshot
//! named after its binary, and `transport_git` re-executes its own binary by
//! test name.

mod support;

#[path = "build.rs"]
mod build;

#[path = "catalog.rs"]
mod catalog;

#[path = "cli.rs"]
mod cli;

#[path = "equivalence.rs"]
mod equivalence;

#[path = "definition_coverage.rs"]
mod definition_coverage;

#[path = "project_cli.rs"]
mod project_cli;

#[path = "snapshot.rs"]
mod snapshot;

#[path = "sdk_assembly.rs"]
mod sdk_assembly;

#[path = "standalone_build.rs"]
mod standalone_build;
