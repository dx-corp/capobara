use std::path::PathBuf;

use clap::{Parser, Subcommand};

use capobara::cli::ProjectCliArgs;
use capobara::cli::catalog::CatalogCommand;
use capobara::cli::project::ProjectCommand;
use capobara::cli::run::RunArgs;
use capobara::cli::transport::{PrepareArgs, ReportArgs};

#[derive(Parser)]
#[command(
    name = "capobara",
    version,
    about = "Publishes deterministic projections of Mono into standalone repositories."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate the catalog or print the publication matrix.
    Catalog {
        /// The Mono checkout to read config/projections/*.json from and
        /// run git against. Defaults to `git rev-parse --show-toplevel`
        /// from the current directory. `global = true` so it parses both
        /// before and after the `check`/`matrix` subcommand.
        #[arg(long, global = true)]
        root: Option<PathBuf>,
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Report the plan without changing the destination.
    Plan(ProjectCliArgs),
    /// Apply the projection to the destination checkout.
    Apply(ProjectCliArgs),
    /// Apply and require the stored receipt to match.
    Verify(ProjectCliArgs),
    /// Report drift between source and destination.
    Check(ProjectCliArgs),
    /// Clone-side preparation of the destination branch.
    Prepare(PrepareArgs),
    /// Recheck a prepared projection before publication.
    Preflight(ReportArgs),
    /// Commit, push, and open or update the pull request.
    Publish(ReportArgs),
    /// Prepare, apply, verify, preflight, publish, and prove in one process.
    #[command(
        long_about = "Prepare, apply, verify, preflight, publish, and prove in one process.\n\n\
        Validation policies are applied by the workflow's validate steps until they are \
        ported; `run` performs no distribution validation."
    )]
    Run(RunArgs),
}

// `plan`/`apply`/`verify`/`check` exit with the code `cli::project::run`
// returns on success, or print the error and exit 2 -- regardless of the
// `Error` variant. This differs from the other subcommands' `error.exit_code()`
// contract; see `cli::project::run`'s doc comment for why (Node's
// file-level `catch` sets exit code 2 unconditionally for this command
// family, including a `verify` provenance mismatch).
#[allow(clippy::exit)]
fn exit_project(result: capobara::Result<i32>) -> ! {
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

// `prepare`/`preflight`/`publish`/`run` return their own exit code (3 when
// a sync-hold stopped the work, else 0) and exit 1 on any error,
// regardless of the `Error` variant: both `scripts/projections/transport.mjs`
// and `scripts/projections/copybara-preflight.mjs` set
// `process.exitCode = 1` in their file-level `catch`, and set 3 only from
// the returned `result.held`. This differs from `exit_project` (always 2)
// and from `Error::exit_code()`.
#[allow(clippy::exit)]
fn exit_transport(result: capobara::Result<i32>) -> ! {
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

// `catalog check`/`catalog matrix` exit 1 on any error, regardless of the
// `Error` variant -- Node's file-level `catch` at the bottom of
// `catalog.mjs` unconditionally sets `process.exitCode = 1`. This differs
// from both `exit_transport` (always exits 1) and `exit_project`
// (always exits 2); see `cli::catalog::run`'s doc comment.
#[allow(clippy::exit)]
fn exit_catalog(result: capobara::Result<()>) -> ! {
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Plan(args) => {
            exit_project(capobara::cli::project::run(
                args.into_project_args(ProjectCommand::Plan),
            ));
        }
        Command::Apply(args) => {
            exit_project(capobara::cli::project::run(
                args.into_project_args(ProjectCommand::Apply),
            ));
        }
        Command::Verify(args) => {
            exit_project(capobara::cli::project::run(
                args.into_project_args(ProjectCommand::Verify),
            ));
        }
        Command::Check(args) => {
            exit_project(capobara::cli::project::run(
                args.into_project_args(ProjectCommand::Check),
            ));
        }
        Command::Catalog { root, command } => {
            exit_catalog(
                capobara::cli::catalog::resolve_root(root)
                    .and_then(|root| capobara::cli::catalog::run(&root, command)),
            );
        }
        Command::Prepare(args) => exit_transport(capobara::cli::transport::prepare(args)),
        Command::Preflight(args) => exit_transport(capobara::cli::transport::preflight(args)),
        Command::Publish(args) => exit_transport(capobara::cli::transport::publish(args)),
        Command::Run(args) => exit_transport(capobara::cli::run::run(args)),
    }
}
