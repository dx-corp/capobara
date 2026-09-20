//! The projection receipt written into every projected tree, and its
//! verification. Ports `provenance` construction and `verifyProvenance` from
//! `scripts/projections/project.mjs`.

use serde::{Deserialize, Serialize};

use crate::{Result, contract};

/// The projection receipt. Field order matches Node's `provenance` object
/// literal in `buildProjection` exactly, which (together with
/// `#[serde(rename_all = "camelCase")]`) makes `to_receipt_bytes` byte-for-byte
/// compatible with Node's `JSON.stringify(provenance, null, 2) + "\n"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provenance {
    pub schema_version: u32,
    pub projection: String,
    pub projection_schema_version: u32,
    pub source_repository: String,
    pub source_sha: String,
    pub destination_repository: String,
    pub prior_projected_base: String,
    pub definition_digest: String,
    pub tool_digest: String,
    pub content_digest: String,
    pub publication_eligible: bool,
}

impl Provenance {
    /// The exact bytes written to the receipt file: two-space-indented JSON
    /// (`serde_json::to_string_pretty`, the same layout as
    /// `JSON.stringify(v, null, 2)`) plus a trailing newline.
    pub fn to_receipt_bytes(&self) -> Vec<u8> {
        let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
        text.push('\n');
        text.into_bytes()
    }

    /// Ports `verifyProvenance`: every field of `self` must equal the
    /// corresponding field of `stored`. Node additionally checks that
    /// `stored`'s key set matches exactly (`keys(actual, Object.keys(expected))`);
    /// that check has no analogue here because both sides are the same typed
    /// struct with `deny_unknown_fields`, so an extra or missing key can
    /// never reach this comparison. Reports the first differing field, in
    /// the struct's declared order, by its camelCase (receipt JSON) name to
    /// match Node's `Provenance mismatch: ${key}` messages.
    pub fn verify_against(&self, stored: &Provenance) -> Result<()> {
        contract(
            self.schema_version == stored.schema_version,
            "Provenance mismatch: schemaVersion",
        )?;
        contract(
            self.projection == stored.projection,
            "Provenance mismatch: projection",
        )?;
        contract(
            self.projection_schema_version == stored.projection_schema_version,
            "Provenance mismatch: projectionSchemaVersion",
        )?;
        contract(
            self.source_repository == stored.source_repository,
            "Provenance mismatch: sourceRepository",
        )?;
        contract(
            self.source_sha == stored.source_sha,
            "Provenance mismatch: sourceSha",
        )?;
        contract(
            self.destination_repository == stored.destination_repository,
            "Provenance mismatch: destinationRepository",
        )?;
        contract(
            self.prior_projected_base == stored.prior_projected_base,
            "Provenance mismatch: priorProjectedBase",
        )?;
        contract(
            self.definition_digest == stored.definition_digest,
            "Provenance mismatch: definitionDigest",
        )?;
        contract(
            self.tool_digest == stored.tool_digest,
            "Provenance mismatch: toolDigest",
        )?;
        contract(
            self.content_digest == stored.content_digest,
            "Provenance mismatch: contentDigest",
        )?;
        contract(
            self.publication_eligible == stored.publication_eligible,
            "Provenance mismatch: publicationEligible",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Provenance {
        Provenance {
            schema_version: 1,
            projection: "sample".into(),
            projection_schema_version: 1,
            source_repository: "dx-corp/mono".into(),
            source_sha: "1".repeat(40),
            destination_repository: "dx-corp/sample".into(),
            prior_projected_base: "2".repeat(40),
            definition_digest: "d".repeat(64),
            tool_digest: "3".repeat(64),
            content_digest: "c".repeat(64),
            publication_eligible: true,
        }
    }

    #[test]
    fn to_receipt_bytes_is_pretty_json_with_declared_field_order_and_trailing_newline() {
        let bytes = sample().to_receipt_bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.ends_with("}\n"));
        assert!(!text.ends_with("}\n\n"));
        let schema_at = text.find("\"schemaVersion\"").unwrap();
        let projection_at = text.find("\"projection\"").unwrap();
        let eligible_at = text.find("\"publicationEligible\"").unwrap();
        assert!(schema_at < projection_at);
        assert!(projection_at < eligible_at);
        assert!(text.starts_with("{\n  \""));
    }

    #[test]
    fn verify_against_reports_the_first_differing_field_in_declared_order() {
        let base = sample();
        let mut other = base.clone();
        other.projection_schema_version = 2;
        other.source_sha = "9".repeat(40);
        let err = base.verify_against(&other).unwrap_err().to_string();
        assert_eq!(err, "Provenance mismatch: projectionSchemaVersion");
        assert!(base.verify_against(&base).is_ok());
    }
}
