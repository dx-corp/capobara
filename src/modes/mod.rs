//! Projection modes. Each mode collects the entries and deletions that
//! `build::build_projection` folds into a receipt and a `Plan`.

pub mod copy_v1;
pub mod sdk_assembly;

use crate::tree::Entries;

/// Entries and their declared output boundaries, as produced by an
/// `sdk-assembly-v1` assembler. Task 10 supplies the real assembler (a port
/// of `assembleSdkProjection`); `build::build_projection`'s `assemble`
/// parameter returns this for now so the `sdk-assembly-v1` branch of the
/// shared build tail has a concrete type to work with.
#[derive(Debug)]
pub struct Assembled {
    pub entries: Entries,
    pub output_include: Vec<String>,
    pub output_managed: Vec<String>,
}
