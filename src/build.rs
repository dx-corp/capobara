//! Assembles a projection's entries and deletions into a planned `Plan` plus
//! its `Provenance` receipt. Ports the shared tail of `buildProjection` and
//! `checkPublicEntry` from `scripts/projections/project.mjs`; the
//! `maestro-public-tree-v1` branch of `buildProjection` is out of scope for
//! this crate (see `definition`'s module doc comment).

use std::path::Path;

use crate::definition::{Definition, Mode, definition_digest};
use crate::git::{is_sha, is_tree_id_or_digest};
use crate::modes::{Assembled, copy_v1};
use crate::receipt::Provenance;
use crate::tree::{Entry, Matcher, Plan, files_under, plan_tree, tree_digest};
use crate::{Result, contract};

pub struct BuildInput<'a> {
    pub definition: &'a Definition,
    pub definition_text: &'a str,
    pub source_root: &'a Path,
    pub target_root: &'a Path,
    pub source_sha: &'a str,
    pub prior_projected_base: &'a str,
    pub tool_digest: &'a str,
    pub publication_eligible: bool,
}

#[derive(Debug)]
pub struct Built {
    pub plan: Plan,
    pub provenance: Provenance,
}

/// Ports `checkPublicEntry`. `regex` has no lookahead, so the private-path
/// rule is implemented by walking `path`'s `/`-separated segments instead of
/// running one regex over the whole string; see `is_private_segment` for the
/// per-segment rule (including the `.env.example`-as-last-segment
/// exception, whose Node counterpart is a negative lookahead anchored on the
/// *whole path's* end, not the segment's).
pub fn check_public_entry(path: &str, entry: &Entry, definition: &Definition) -> Result<()> {
    let plugin_catalog = definition.name == "plugins"
        && definition.source_repository == "dx-corp/mono"
        && definition.destination.repository == "dx-corp/plugins"
        && path == ".agents/plugins/marketplace.json";
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    let private = segments
        .iter()
        .enumerate()
        .any(|(index, segment)| is_private_segment(segment, index == last));
    contract(
        plugin_catalog || !private,
        format!("Private path in public projection: {path}"),
    )?;

    contract(
        !starts_with_private_key_header(&entry.content),
        format!("Private key in public projection: {path}"),
    )?;
    Ok(())
}

/// True when `content` opens with a PEM private-key header: `-----BEGIN `,
/// an optional key-type tag (`RSA `, `EC `, or `OPENSSH `), then
/// `PRIVATE KEY-----`. Built from its fragments (rather than as four
/// complete, adjacent `-----BEGIN ... PRIVATE KEY-----` literals) so the
/// source text never itself reads as a stack of PEM key blocks.
fn starts_with_private_key_header(content: &[u8]) -> bool {
    let Some(rest) = content.strip_prefix(b"-----BEGIN ") else {
        return false;
    };
    [&b""[..], b"RSA ", b"EC ", b"OPENSSH "].iter().any(|tag| {
        rest.strip_prefix(*tag)
            .is_some_and(|after| after.starts_with(b"PRIVATE KEY-----"))
    })
}

/// True when a single path segment is private on its own. `is_last` says
/// whether this is the final segment of the whole path, needed only for the
/// `.env.example` exception: Node's negative lookahead `(?!example$)` tests
/// against the end of the *path*, so `.env.example` is allowed exclusively
/// as the last segment (`src/.env.example` is fine; `src/.env.example/x`,
/// where `.env.example` is a directory, is not).
fn is_private_segment(segment: &str, is_last: bool) -> bool {
    if matches!(
        segment,
        ".env" | "id_rsa" | "id_ed25519" | ".agents" | ".context"
    ) {
        return true;
    }
    if let Some(rest) = segment.strip_prefix("gha-creds-")
        && let Some(name) = rest.strip_suffix(".json")
        && !name.is_empty()
    {
        return true;
    }
    if segment.starts_with(".env.") {
        return !(is_last && segment == ".env.example");
    }
    false
}

/// Ports the shared tail of `buildProjection`: given the mode-specific
/// entries and deletion candidates, validates revision/digest inputs,
/// rejects any entry or deletion that would touch a destination-owned path,
/// rejects a source path that collides with the receipt's own path, checks
/// public-visibility entries, computes `contentDigest` over the entries
/// *before* the receipt is added, inserts the receipt, and plans the tree.
///
/// `assemble` is used only by the `sdk-assembly-v1` branch (Task 10 supplies
/// the real SDK assembler); `copy-v1` never calls it.
pub fn build_projection(
    input: BuildInput,
    assemble: &dyn Fn(&Path, &str) -> Result<Assembled>,
) -> Result<Built> {
    let BuildInput {
        definition,
        definition_text,
        source_root,
        target_root,
        source_sha,
        prior_projected_base,
        tool_digest,
        publication_eligible,
    } = input;

    contract(
        is_sha(source_sha) && is_sha(prior_projected_base),
        "Source revision and prior projected base must be full SHAs",
    )?;
    contract(
        is_tree_id_or_digest(tool_digest),
        "Missing projector implementation digest",
    )?;

    let owned = Matcher::new(&definition.destination_owned)?;

    let (mut entries, mut deletions) = match definition.mode {
        Mode::CopyV1 => copy_v1::collect(definition, source_root, target_root, &owned)?,
        Mode::SdkAssemblyV1 => {
            let assembled = assemble(source_root, &definition.name)?;
            let allowed = Matcher::new(&assembled.output_include)?;
            let managed = Matcher::new(&assembled.output_managed)?;
            for path in assembled.entries.keys() {
                contract(
                    allowed.matches(path) && managed.matches(path),
                    format!("SDK output outside reviewed policy: {path}"),
                )?;
            }
            let deletions: Vec<String> =
                files_under(target_root, &|path: &str| owned.matches(path))?
                    .into_iter()
                    .filter(|path| managed.matches(path))
                    .collect();
            (assembled.entries, deletions)
        }
    };

    for (path, entry) in entries.iter() {
        contract(
            !owned.matches(path),
            format!("Projection would overwrite destination-owned path: {path}"),
        )?;
        contract(
            path.as_str() != definition.provenance.as_str(),
            "Source collides with provenance",
        )?;
        if definition.visibility == "public" {
            check_public_entry(path, entry, definition)?;
        }
    }

    let content_digest = tree_digest(&entries);

    let provenance = Provenance {
        schema_version: 1,
        projection: definition.name.clone(),
        projection_schema_version: definition.schema_version,
        source_repository: definition.source_repository.clone(),
        source_sha: source_sha.to_string(),
        destination_repository: definition.destination.repository.clone(),
        prior_projected_base: prior_projected_base.to_string(),
        definition_digest: definition_digest(definition_text)?,
        tool_digest: tool_digest.to_string(),
        content_digest,
        publication_eligible,
    };

    entries.insert(
        definition.provenance.clone(),
        Entry {
            content: provenance.to_receipt_bytes(),
            mode: 0o644,
        },
    );

    deletions.retain(|path| !entries.contains_key(path));
    for path in &deletions {
        contract(
            !owned.matches(path),
            format!("Projection would delete destination-owned path: {path}"),
        )?;
    }

    let plan = plan_tree(target_root, entries, deletions)?;
    Ok(Built { plan, provenance })
}
