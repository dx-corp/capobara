//! Integration tests for `capobara::transport::github`, using `RecordedApi`
//! (available here via the `recorded-api` feature). Ports, by name, three
//! tests from `scripts/projections/transport.test.mjs`:
//! `publication_checks_destination_identity_visibility_ownership_and_unreadable_hold_state`,
//! `publication_requires_an_app_token_scoped_to_exactly_the_destination`, and
//! `pr_body_carries_complete_provenance_and_bounded_changed_deleted_paths`.

use serde_json::{Value, json};

use capobara::definition::{Definition, definition_from_value};
use capobara::receipt::Provenance;
use capobara::report::Report;
use capobara::transport::{
    RecordedApi, create_or_update_pr, publication_body, read_publication_state,
};

fn no_sdk(_: &str) -> Option<Vec<String>> {
    None
}

fn base_definition_json() -> Value {
    json!({
        "schemaVersion": 1, "name": "fixture", "class": "source-tree", "mode": "copy-v1",
        "sourceRepository": "test/mono", "visibility": "public",
        "mappings": [
            {"source": "source", "destination": ".", "include": ["src/**", "README.md"], "exclude": []}
        ],
        "destination": {"repository": "test/public", "branch": "main", "syncBranch": "sync/fixture", "holdLabel": "sync-hold"},
        "destinationOwned": [".github/**", "SECURITY.md"],
        "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
        "outputManaged": ["src/**", "README.md"]
    })
}

fn definition() -> Definition {
    definition_from_value(base_definition_json(), &no_sdk)
        .unwrap()
        .definition
}

fn info() -> Value {
    json!({
        "full_name": "test/public",
        "archived": false,
        "disabled": false,
        "default_branch": "main",
        "visibility": "public",
    })
}

fn installation() -> Value {
    json!({"total_count": 1, "repositories": [{"full_name": "test/public"}]})
}

fn pr() -> Value {
    json!({
        "number": 1,
        "labels": [],
        "head": {
            "repo": {"full_name": "test/public"},
            "ref": "sync/fixture",
            "sha": "a".repeat(40),
        },
        "base": {"ref": "main"},
        "html_url": "https://github.com/test/public/pull/1",
    })
}

const REPOS_ENDPOINT: &str = "repos/test/public";
const INSTALLATION_ENDPOINT: &str = "installation/repositories?per_page=100";
const PULLS_ENDPOINT: &str =
    "repos/test/public/pulls?state=open&base=main&head=test%3Async%2Ffixture";

#[test]
fn publication_checks_destination_identity_visibility_ownership_and_unreadable_hold_state() {
    let d = definition();

    // No open PR: not held.
    let api = RecordedApi::new(vec![
        ("GET", REPOS_ENDPOINT, info()),
        ("GET", INSTALLATION_ENDPOINT, installation()),
        ("GET", PULLS_ENDPOINT, json!([])),
    ]);
    assert!(!read_publication_state(&d, &api).unwrap().held);

    // An open PR carrying the hold label: held.
    let mut held_pr = pr();
    held_pr["labels"] = json!([{"name": "sync-hold"}]);
    let api = RecordedApi::new(vec![
        ("GET", REPOS_ENDPOINT, info()),
        ("GET", INSTALLATION_ENDPOINT, installation()),
        ("GET", PULLS_ENDPOINT, json!([held_pr])),
    ]);
    assert!(read_publication_state(&d, &api).unwrap().held);

    // Unreadable or malformed PR-list responses each fail.
    let mut labels_null = pr();
    labels_null["labels"] = json!(null);
    let mut label_no_name = pr();
    label_no_name["labels"] = json!([{}]);
    let mut bad_sha = pr();
    bad_sha["head"]["sha"] = json!("bad");

    for payload in [
        json!(null),
        json!({}),
        json!([pr(), pr()]),
        json!([labels_null]),
        json!([label_no_name]),
        json!([bad_sha]),
    ] {
        let api = RecordedApi::new(vec![
            ("GET", REPOS_ENDPOINT, info()),
            ("GET", INSTALLATION_ENDPOINT, installation()),
            ("GET", PULLS_ENDPOINT, payload),
        ]);
        assert!(read_publication_state(&d, &api).is_err());
    }

    // Destination identity/visibility mismatches fail before the
    // installation-scope or PR-list calls are ever made.
    for (key, value) in [
        ("visibility", json!("private")),
        ("full_name", json!("another/repo")),
        ("archived", json!(true)),
    ] {
        let mut patched = info();
        patched[key] = value;
        let api = RecordedApi::new(vec![("GET", REPOS_ENDPOINT, patched)]);
        let err = read_publication_state(&d, &api).unwrap_err().to_string();
        assert_eq!(
            err,
            "Destination identity or visibility mismatch: test/public"
        );
    }
}

#[test]
fn publication_requires_an_app_token_scoped_to_exactly_the_destination() {
    let d = definition();
    for scope in [
        json!(null),
        json!({}),
        json!({"total_count": 0, "repositories": []}),
        json!({"total_count": 1, "repositories": [{"full_name": "test/other"}]}),
        json!({
            "total_count": 2,
            "repositories": [{"full_name": "test/public"}, {"full_name": "test/other"}],
        }),
    ] {
        let api = RecordedApi::new(vec![
            ("GET", REPOS_ENDPOINT, info()),
            ("GET", INSTALLATION_ENDPOINT, scope),
        ]);
        let err = read_publication_state(&d, &api).unwrap_err().to_string();
        assert_eq!(err, "Destination App token scope mismatch: test/public");
    }
}

fn pr_body_definition() -> Definition {
    let raw = json!({
        "schemaVersion": 1, "name": "sample", "class": "source-tree", "mode": "copy-v1",
        "sourceRepository": "dx-corp/mono", "visibility": "public",
        "mappings": [
            {"source": "pkg", "destination": ".", "include": ["src/**"], "exclude": []}
        ],
        "destination": {"repository": "dx-corp/sample", "branch": "main", "syncBranch": "sync/mono-projection", "holdLabel": "sync-hold"},
        "destinationOwned": [".github/**"],
        "deletion": "owned-paths", "provenance": ".repository-projection.json", "validation": "tree-v1",
        "outputManaged": ["src/**"]
    });
    definition_from_value(raw, &no_sdk).unwrap().definition
}

#[test]
fn pr_body_carries_complete_provenance_and_bounded_changed_deleted_paths() {
    let d = pr_body_definition();
    let provenance = Provenance {
        schema_version: 1,
        projection: "sample".into(),
        projection_schema_version: 1,
        source_repository: "dx-corp/mono".into(),
        source_sha: "1".repeat(40),
        destination_repository: "dx-corp/sample".into(),
        prior_projected_base: "2".repeat(40),
        definition_digest: "3".repeat(64),
        tool_digest: "4".repeat(64),
        content_digest: "5".repeat(64),
        publication_eligible: true,
    };
    let report = Report {
        copied_paths: (0..25).map(|i| format!("c{i}")).collect(),
        deleted_paths: (0..25).map(|i| format!("d{i}")).collect(),
        copied_count: 25,
        deleted_count: 25,
        provenance,
        source_file_count: 0,
        result: "drift_detected".into(),
    };

    let body = publication_body(&d, &report);
    // Verified byte-for-byte against Node's `publicationBody` output before
    // this snapshot was accepted; see the task-11 report for the recorded
    // command and its output.
    insta::assert_snapshot!(body);
}

#[test]
fn create_or_update_pr_posts_a_new_pr_and_returns_its_html_url() {
    let d = definition();
    let api = RecordedApi::new(vec![(
        "POST",
        "repos/test/public/pulls",
        json!({"number": 7, "html_url": "https://github.com/test/public/pull/7"}),
    )]);
    let url = create_or_update_pr(&d, &api, None, "body text").unwrap();
    assert_eq!(url, "https://github.com/test/public/pull/7");
    let calls = api.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].2,
        Some(json!({
            "title": "chore: sync fixture from Mono",
            "body": "body text",
            "head": "sync/fixture",
            "base": "main",
        }))
    );
}

#[test]
fn create_or_update_pr_patches_an_existing_pr() {
    let d = definition();
    let existing = capobara::transport::PullRequest {
        number: 7,
        head_sha: "a".repeat(40),
        labels: vec![],
        html_url: "https://github.com/test/public/pull/7".to_string(),
    };
    let api = RecordedApi::new(vec![(
        "PATCH",
        "repos/test/public/pulls/7",
        json!({"number": 7, "html_url": "https://github.com/test/public/pull/7"}),
    )]);
    let url = create_or_update_pr(&d, &api, Some(&existing), "updated body").unwrap();
    assert_eq!(url, "https://github.com/test/public/pull/7");
    let calls = api.calls();
    assert_eq!(calls[0].2, Some(json!({"body": "updated body"})));
}

#[test]
fn create_or_update_pr_rejects_an_unconfirmed_response() {
    let d = definition();
    for response in [json!({}), json!({"number": 7}), json!({"html_url": "x"})] {
        let api = RecordedApi::new(vec![("POST", "repos/test/public/pulls", response)]);
        let err = create_or_update_pr(&d, &api, None, "body")
            .unwrap_err()
            .to_string();
        assert_eq!(err, "GitHub did not confirm the generated PR");
    }
}
