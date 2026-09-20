//! The JSON report a projection subcommand writes (`--report`), and the
//! input to the publication body.
use crate::build::Built;
use crate::receipt::Provenance;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub copied_paths: Vec<String>,
    pub deleted_paths: Vec<String>,
    pub copied_count: usize,
    pub deleted_count: usize,
    pub provenance: Provenance,
    pub source_file_count: usize,
    pub result: String,
}

impl From<&Built> for Report {
    fn from(built: &Built) -> Report {
        let copied_count = built.plan.copied_count();
        let deleted_count = built.plan.deleted_count();
        Report {
            copied_paths: built.plan.copied_paths.clone(),
            deleted_paths: built.plan.deleted_paths.clone(),
            copied_count,
            deleted_count,
            provenance: built.provenance.clone(),
            // Node: `entries.size - 1` (the receipt entry is not a source file).
            source_file_count: built.plan.entries.len().saturating_sub(1),
            result: if copied_count + deleted_count > 0 {
                "drift_detected".into()
            } else {
                "in_sync".into()
            },
        }
    }
}

use crate::definition::Definition;

/// Ports the markdown body written to `--markdown-output`: the exact Node
/// template from `main`'s `options["--markdown-output"]` branch in
/// `scripts/projections/project.mjs`, joined with `\n`. The final element
/// of Node's array is `""`, so the result ends with a trailing empty line
/// (a trailing `\n` once written to a file).
pub fn markdown_summary(
    definition: &Definition,
    source_sha: &str,
    prior_base: &str,
    built: &Built,
    report: &Report,
) -> String {
    let mut lines: Vec<String> = vec![
        format!("## Projection: {}", definition.name),
        String::new(),
        format!("- Source: {}@{source_sha}", definition.source_repository),
        format!("- Prior destination base: {prior_base}"),
        format!("- Content SHA-256: {}", built.provenance.content_digest),
        format!(
            "- Result: {}; {} changed, {} deleted",
            report.result, report.copied_count, report.deleted_count
        ),
        "- Destination-owned content is preserved. Destination CI is a separate health signal."
            .to_string(),
        String::new(),
    ];
    lines.extend(
        built
            .plan
            .copied_paths
            .iter()
            .take(20)
            .map(|p| format!("- copy/update {p}")),
    );
    lines.extend(
        built
            .plan
            .deleted_paths
            .iter()
            .take(20)
            .map(|p| format!("- delete {p}")),
    );
    lines.push(String::new());
    lines.join("\n")
}
