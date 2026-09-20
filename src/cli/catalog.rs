//! `catalog check` and `catalog matrix [name|all]`. Ports the
//! `["matrix", "check"]` branch of `main` at the bottom of
//! `scripts/projections/catalog.mjs`. Node's file-level `catch` there sets
//! `process.exitCode = 1` for any thrown error -- unlike
//! `scripts/projections/project.mjs`'s `plan`/`apply`/`verify`/`check`,
//! which exit 2 (see `cli::project::run`'s doc comment); `main.rs`'s
//! `exit_catalog` mirrors that here.

use std::path::{Path, PathBuf};

use clap::Subcommand;

use crate::catalog::{publication_matrix, read_catalog};
use crate::cli::project::sdk_inputs;
use crate::{Error, Result, git};

#[derive(Subcommand, Debug)]
pub enum CatalogCommand {
    /// Validate every catalog entry and print how many were checked.
    Check,
    /// Print the publication matrix (`{"include":[...]}`) for one
    /// projection name, or every projection when omitted.
    Matrix {
        #[arg(default_value = "all")]
        requested: String,
    },
}

/// Resolves the Mono checkout `catalog` reads `config/projections/*.json`
/// from and runs git against. `catalog.mjs`'s `ROOT` is derived from the
/// script's own file location (`fileURLToPath(new URL("../../",
/// import.meta.url))`), so it is the Mono checkout containing the script
/// regardless of the process's current directory. This crate has no
/// script location to derive from, so an explicit `--root` is used as-is;
/// otherwise the checkout is found the same cwd-independent way a human
/// would from a shell: `git rev-parse --show-toplevel`, run through the
/// crate's `git` module so `Command::new` stays confined to `git.rs::run`.
pub fn resolve_root(root: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = root {
        return Ok(root);
    }
    let cwd = std::env::current_dir().map_err(Error::Io)?;
    git::git(&cwd, &["rev-parse", "--show-toplevel"])
        .map(|toplevel| PathBuf::from(toplevel.trim()))
        .map_err(|_| Error::Invalid("Not inside a git repository; pass --root".into()))
}

/// Runs `catalog check` or `catalog matrix [name|all]` against `root`.
/// `root` is the Mono checkout to read `config/projections/*.json` and run
/// git commands against; the `sdk-assembly-v1` entries resolve their input
/// roots through `cli::project::sdk_inputs`, the same reviewed policy lookup
/// `plan`/`apply`/`verify`/`check` use.
pub fn run(root: &Path, command: CatalogCommand) -> Result<()> {
    match command {
        CatalogCommand::Check => {
            let count = read_catalog(root, &sdk_inputs)?.len();
            println!("{count} repository projections validated");
        }
        CatalogCommand::Matrix { requested } => {
            let matrix = publication_matrix(root, &requested, &sdk_inputs)?;
            let json = serde_json::to_string(&matrix).map_err(|e| {
                Error::Invalid(format!("Failed to serialize publication matrix: {e}"))
            })?;
            println!("{json}");
        }
    }
    Ok(())
}
