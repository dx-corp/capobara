//! `prepare`, `preflight`, and `publish`: the transport CLI entry points.
//! Ports the `main()` dispatch at the bottom of
//! `scripts/projections/transport.mjs` (`prepare`/`publish`) and of
//! `scripts/projections/copybara-preflight.mjs` (`preflight`).
//!
//! All three print one line of JSON on stdout, exit 3 when the destination
//! PR is sync-held, and exit 1 on any error -- the latter regardless of the
//! `Error` variant, because both Node scripts' file-level `catch` sets
//! `process.exitCode = 1` unconditionally (`main.rs`'s `exit_transport`
//! mirrors that; contrast `exit_project`, which always exits 2).

use std::path::{Path, PathBuf};

use clap::Args;

use crate::catalog::read_catalog;
use crate::cli::project::sdk_inputs;
use crate::definition::LoadedDefinition;
use crate::preflight::preflight_publication;
use crate::transport::git::read_report;
use crate::transport::github::{GitHubApi, RestApi};
use crate::transport::{Published, prepare_destination, publish_prepared_tree};
use crate::{Error, Result, git};

/// `prepare <name> <target> <source-sha>`. Positional, in Node's argv
/// order.
#[derive(Args, Debug)]
pub struct PrepareArgs {
    /// The catalog projection name.
    pub name: String,
    /// The destination checkout.
    pub target: PathBuf,
    /// The immutable Mono revision being projected.
    pub source_sha: String,
}

/// `preflight <name> <target> <source-sha> <report>` and
/// `publish <name> <target> <source-sha> <report>`.
#[derive(Args, Debug)]
pub struct ReportArgs {
    /// The catalog projection name.
    pub name: String,
    /// The destination checkout.
    pub target: PathBuf,
    /// The immutable Mono revision being projected.
    pub source_sha: String,
    /// The report `capobara apply` wrote for this projection.
    pub report: PathBuf,
}

/// The Mono checkout `<name>` is resolved against. Node derives its `ROOT`
/// from the script's own location (`new URL("../../", import.meta.url)`);
/// a compiled binary has no script location, so the source root is the
/// process's current directory -- which is what the workflow's
/// `$GITHUB_WORKSPACE` already is for every projection step.
///
/// When the current directory holds no `config/projections/`, and only
/// then, this falls back to the enclosing checkout's top level the same
/// cwd-independent way `cli::catalog::resolve_root` does. That keeps the
/// brief's "current working directory as the source root" exact wherever
/// it resolves at all, and replaces an opaque `Invalid repository catalog`
/// with the obvious answer when these subcommands are run from a
/// subdirectory of Mono. A failed fallback is not itself an error: the
/// caller still reports the missing catalog against the cwd.
pub fn source_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(Error::Io)?;
    if cwd.join("config/projections").is_dir() {
        return Ok(cwd);
    }
    let toplevel = git::git(&cwd, &["rev-parse", "--show-toplevel"])
        .map(|toplevel| PathBuf::from(toplevel.trim()))
        .ok()
        .filter(|toplevel| toplevel.join("config/projections").is_dir());
    Ok(toplevel.unwrap_or(cwd))
}

/// Ports `readCatalog(ROOT).find((d) => d.name === name)` plus its
/// `Unknown projection: ${name}` guard.
pub fn resolve_definition(root: &Path, name: &str) -> Result<LoadedDefinition> {
    read_catalog(root, &sdk_inputs)?
        .into_iter()
        .find(|loaded| loaded.definition.name == name)
        .ok_or_else(|| Error::Contract(format!("Unknown projection: {name}")))
}

/// One recorded `(method, endpoint, response)` triple in a
/// `CAPOBARA_RECORDED_API` file.
#[cfg(all(debug_assertions, feature = "recorded-api"))]
#[derive(serde::Deserialize)]
struct RecordedCall {
    method: String,
    endpoint: String,
    response: serde_json::Value,
}

/// Debug builds with the `recorded-api` feature only: replay a recorded
/// GitHub conversation from the JSON file named by `CAPOBARA_RECORDED_API`
/// instead of calling api.github.com, so an integration test can drive
/// `run`/`prepare`/`preflight`/`publish` end to end as a child process with
/// no network and no token. Neither the variable nor `RecordedApi` itself
/// exists in a release build, so a production binary can never be talked
/// into replaying a canned GitHub response.
#[cfg(all(debug_assertions, feature = "recorded-api"))]
fn recorded_api(path: &str) -> Result<crate::transport::RecordedApi> {
    let bytes = std::fs::read(path).map_err(Error::Io)?;
    let calls: Vec<RecordedCall> = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Invalid(format!("Invalid recorded API file: {e}")))?;
    Ok(crate::transport::RecordedApi::new(
        calls
            .iter()
            .map(|call| {
                (
                    call.method.as_str(),
                    call.endpoint.as_str(),
                    call.response.clone(),
                )
            })
            .collect(),
    ))
}

/// The GitHub transport for this process: the real REST client, unless a
/// debug build has been pointed at a recorded conversation (see
/// `recorded_api`).
pub fn api_from_env() -> Result<Box<dyn GitHubApi>> {
    #[cfg(all(debug_assertions, feature = "recorded-api"))]
    if let Ok(path) = std::env::var("CAPOBARA_RECORDED_API") {
        return Ok(Box::new(recorded_api(&path)?));
    }
    Ok(Box::new(RestApi::from_env()?))
}

/// Node: `JSON.stringify(result)`.
fn to_line(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| Error::Invalid(format!("Failed to serialize result: {e}")))
}

/// Node: `console.log(JSON.stringify(result)); if (result.held)
/// process.exitCode = 3;`
fn print_line(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", to_line(value)?);
    Ok(())
}

/// `{"held":true}` / `{"held":false}` -- `prepare`'s exact Node shape
/// (`prepareDestination` returns only that one key).
#[derive(serde::Serialize)]
struct Held {
    held: bool,
}

/// `{"held":false,"unchanged":true}`, the converged branch of `publish`.
#[derive(serde::Serialize)]
struct Unchanged {
    held: bool,
    unchanged: bool,
}

/// `{"held":false,"pullRequest":...,"engine":...,"tree":...}`, the
/// published branch of `publish`, in Node's key order
/// (`transport.mjs:441-446`). A derived `Serialize` makes the struct's
/// declared field order the JSON key order, so this declaration *is* the
/// wire contract.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestResult {
    held: bool,
    pull_request: String,
    engine: String,
    tree: String,
}

pub fn prepare(args: PrepareArgs) -> Result<i32> {
    let root = source_root()?;
    let loaded = resolve_definition(&root, &args.name)?;
    let api = api_from_env()?;
    let prepared = prepare_destination(
        &loaded.definition,
        &loaded.text,
        &args.target,
        &args.source_sha,
        api.as_ref(),
    )?;
    print_line(&Held {
        held: prepared.held,
    })?;
    Ok(if prepared.held { 3 } else { 0 })
}

pub fn preflight(args: ReportArgs) -> Result<i32> {
    let root = source_root()?;
    let loaded = resolve_definition(&root, &args.name)?;
    let api = api_from_env()?;
    let result = preflight_publication(
        &loaded.definition,
        &root,
        &args.source_sha,
        &args.target,
        &args.report,
        api.as_ref(),
    )?;
    print_line(&result)?;
    Ok(if result.held { 3 } else { 0 })
}

pub fn publish(args: ReportArgs) -> Result<i32> {
    let root = source_root()?;
    let loaded = resolve_definition(&root, &args.name)?;
    let api = api_from_env()?;
    let report = read_report(&args.report)?;
    let published = publish_prepared_tree(
        &loaded.definition,
        &root,
        &args.source_sha,
        &args.target,
        &report,
        api.as_ref(),
    )?;
    print_published(&published)?;
    Ok(if matches!(published, Published::Held) {
        3
    } else {
        0
    })
}

/// The three `publishPreparedTree` return shapes, serialized exactly as
/// Node prints them -- including `engine` and `tree`, which
/// `transport::git::Published::PullRequest` now carries.
/// `cli::run` needs the bytes as a value, not only on stdout, because the
/// workflow also records them in `$GITHUB_STEP_SUMMARY` (yml:258).
pub fn published_json(published: &Published) -> Result<String> {
    match published {
        Published::Held => to_line(&Held { held: true }),
        Published::Unchanged => to_line(&Unchanged {
            held: false,
            unchanged: true,
        }),
        Published::PullRequest { url, engine, tree } => to_line(&PullRequestResult {
            held: false,
            pull_request: url.clone(),
            engine: engine.clone(),
            tree: tree.clone(),
        }),
    }
}

/// `published_json`, on stdout.
pub fn print_published(published: &Published) -> Result<()> {
    println!("{}", published_json(published)?);
    Ok(())
}
