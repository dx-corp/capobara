//! `capobara vendor check` — verify a vendored upstream tree against its pin.
//!
//! Read-only by design. Capobara publishes outward and that stays true; this
//! subcommand only answers whether a vendored subtree still matches the
//! upstream commit it claims, and which divergences were declared.

use std::path::PathBuf;

use clap::Args;

use crate::vendor::{self, VendorReport};
use crate::{Error, Result};

#[derive(Args, Debug)]
pub struct VendorCliArgs {
    /// The vendor-import definition to check.
    #[arg(long)]
    pub definition: PathBuf,
    /// Mono checkout holding the vendored tree.
    #[arg(long)]
    pub root: PathBuf,
    /// A checkout of the upstream repository containing the pinned commit.
    #[arg(long)]
    pub upstream: PathBuf,
    /// Revision of Mono to read. Defaults to HEAD.
    #[arg(long, default_value = "HEAD")]
    pub rev: String,
}

pub fn check(args: VendorCliArgs) -> Result<VendorReport> {
    let definition = vendor::load(&args.definition)?;
    let report = vendor::check(&args.root, &args.rev, &args.upstream, &definition)?;
    println!("{}", report.summary());
    for divergence in &report.declared {
        println!("  declared {divergence:?}");
    }
    for divergence in &report.drift {
        println!("  DRIFT    {divergence:?}");
    }
    if report.clean() {
        Ok(report)
    } else {
        Err(Error::Contract(format!(
            "{}: {} undeclared divergence(s) from upstream {}",
            report.name,
            report.drift.len(),
            &report.upstream_commit[..12]
        )))
    }
}
