//! CLI-facing argument types. `main.rs` (the binary) parses these with
//! `clap`; `project::run` (the library function Tasks 12 and 13 also call
//! in-process) takes the plain `project::ProjectArgs` struct built from
//! them, not a `clap`-derived type, so callers that already hold the
//! individual fields never need to round-trip through argv.

pub mod catalog;
pub mod project;
pub mod run;
pub mod transport;

use std::path::PathBuf;

use clap::Args;

use project::{ProjectArgs, ProjectCommand};

/// Flags shared by `plan`, `apply`, `verify`, and `check`. Ports the
/// `--definition`/`--source`/`--source-sha`/`--target`/`--report`/
/// `--status-output`/`--markdown-output`/`--draft` options parsed by
/// `main`'s hand-rolled option loop in `scripts/projections/project.mjs`.
#[derive(Args, Debug)]
pub struct ProjectCliArgs {
    #[arg(long)]
    pub definition: PathBuf,
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long = "source-sha")]
    pub source_sha: String,
    #[arg(long)]
    pub target: PathBuf,
    #[arg(long)]
    pub report: Option<PathBuf>,
    #[arg(long = "status-output")]
    pub status_output: Option<PathBuf>,
    #[arg(long = "markdown-output")]
    pub markdown_output: Option<PathBuf>,
    #[arg(long)]
    pub draft: bool,
}

impl ProjectCliArgs {
    pub fn into_project_args(self, command: ProjectCommand) -> ProjectArgs {
        ProjectArgs {
            command,
            definition: self.definition,
            source: self.source,
            source_sha: self.source_sha,
            target: self.target,
            report: self.report,
            status_output: self.status_output,
            markdown_output: self.markdown_output,
            draft: self.draft,
        }
    }
}
