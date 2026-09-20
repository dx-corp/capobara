//! The GitHub REST client (`RestApi`), a recorded double for tests
//! (`RecordedApi`), and publication state. Ports `github`,
//! `readPublicationState`, `publicationBody`, and the PR create/update calls
//! inside `publishPreparedTree` from `scripts/projections/transport.mjs`.

use std::time::Duration;

use serde_json::Value;

use crate::definition::Definition;
use crate::git::is_sha;
use crate::report::Report;
use crate::{Error, Result, contract};

/// The transport boundary for every GitHub REST call this crate makes.
/// `call` returns `None` for an empty response body (Node's
/// `response.trim() ? JSON.parse(response) : null`).
pub trait GitHubApi {
    fn call(&self, method: &str, endpoint: &str, body: Option<&Value>) -> Result<Option<Value>>;
}

/// A pull request as read back from `readPublicationState`. `html_url` is
/// captured opportunistically (as Node does implicitly by forwarding the raw
/// object) and is not itself part of the malformed-PR validation; a response
/// that omits it yields an empty string rather than a validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub head_sha: String,
    pub labels: Vec<String>,
    pub html_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationState {
    pub pr: Option<PullRequest>,
    pub held: bool,
}

/// Percent-encodes `input` with the same character set JavaScript's
/// `encodeURIComponent` leaves unescaped (`A-Z a-z 0-9 - _ . ! ~ * ' ( )`),
/// operating byte-by-byte over `input`'s UTF-8 encoding so a multi-byte
/// character is encoded as its constituent `%XX` bytes, matching
/// `encodeURIComponent`'s behavior on a UTF-16 string.
pub fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The REST endpoint listing the destination's open generated pull
/// requests: state `open`, base the declared default branch, head
/// `{owner}:{syncBranch}`. Factored out of `read_publication_state` so the
/// post-publication proof (`cli::run`) lists the same set of PRs the
/// publication path does -- the proof needs the raw array (it
/// discriminates "none", "exactly one", and "more than one" itself), not
/// `read_publication_state`'s validated at-most-one view.
pub fn open_sync_pr_endpoint(definition: &Definition) -> String {
    let repository = definition.destination.repository.as_str();
    let owner = repository.split('/').next().unwrap_or_default();
    format!(
        "repos/{repository}/pulls?state=open&base={}&head={}",
        encode_uri_component(&definition.destination.branch),
        encode_uri_component(&format!("{owner}:{}", definition.destination.sync_branch)),
    )
}

/// Ports `readPublicationState`. Confirms destination repository identity,
/// visibility, and archival/disabled state; confirms the calling token's
/// GitHub App installation is scoped to exactly the destination repository;
/// then reads the (at most one) open sync PR and validates its shape.
pub fn read_publication_state(
    definition: &Definition,
    api: &dyn GitHubApi,
) -> Result<PublicationState> {
    let repository = definition.destination.repository.as_str();

    let info = api
        .call("GET", &format!("repos/{repository}"), None)?
        .unwrap_or(Value::Null);
    let identity_ok = info.get("full_name").and_then(Value::as_str) == Some(repository)
        && info.get("archived").and_then(Value::as_bool) == Some(false)
        && info.get("disabled").and_then(Value::as_bool) == Some(false)
        && info.get("default_branch").and_then(Value::as_str)
            == Some(definition.destination.branch.as_str())
        && info.get("visibility").and_then(Value::as_str) == Some(definition.visibility.as_str());
    contract(
        identity_ok,
        format!("Destination identity or visibility mismatch: {repository}"),
    )?;

    // This workflow uses a repository-scoped GitHub App installation token.
    // Unlike user tokens, installation tokens do not expose a meaningful
    // repository.permissions.push claim. The token-mint action has already
    // required contents:write and pull_requests:write; prove here that the
    // resulting token is scoped to exactly the intended destination
    // repository.
    let installation = api
        .call("GET", "installation/repositories?per_page=100", None)?
        .unwrap_or(Value::Null);
    let scope_ok = installation.get("total_count").and_then(Value::as_i64) == Some(1)
        && installation
            .get("repositories")
            .and_then(Value::as_array)
            .is_some_and(|repos| {
                repos.len() == 1
                    && repos[0].get("full_name").and_then(Value::as_str) == Some(repository)
            });
    contract(
        scope_ok,
        format!("Destination App token scope mismatch: {repository}"),
    )?;

    let endpoint = open_sync_pr_endpoint(definition);
    let prs = api.call("GET", &endpoint, None)?.unwrap_or(Value::Null);
    let prs_array = prs.as_array();
    contract(
        prs_array.is_some_and(|prs| prs.len() <= 1),
        "Unreadable or ambiguous destination PR state",
    )?;
    let prs_array = prs_array.expect("checked by the contract() call above");

    let pr = match prs_array.first() {
        None => None,
        Some(pr) => Some(parse_pull_request(pr, repository, definition)?),
    };
    let held = pr.as_ref().is_some_and(|pr| {
        pr.labels
            .iter()
            .any(|l| l == &definition.destination.hold_label)
    });
    Ok(PublicationState { pr, held })
}

/// Validates and converts a single raw PR `Value` into a `PullRequest`.
/// Ports the `pr && (...)` malformed-shape check from `readPublicationState`.
fn parse_pull_request(
    pr: &Value,
    repository: &str,
    definition: &Definition,
) -> Result<PullRequest> {
    let number = pr.get("number").and_then(Value::as_u64);
    let labels = pr.get("labels").and_then(Value::as_array);
    let labels_ok = labels.is_some_and(|labels| {
        labels
            .iter()
            .all(|label| label.get("name").and_then(Value::as_str).is_some())
    });
    let head_repo_full_name = pr
        .get("head")
        .and_then(|head| head.get("repo"))
        .and_then(|repo| repo.get("full_name"))
        .and_then(Value::as_str);
    let head_ref = pr
        .get("head")
        .and_then(|head| head.get("ref"))
        .and_then(Value::as_str);
    let head_sha = pr
        .get("head")
        .and_then(|head| head.get("sha"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let base_ref = pr
        .get("base")
        .and_then(|base| base.get("ref"))
        .and_then(Value::as_str);

    let ok = number.is_some()
        && labels_ok
        && head_repo_full_name == Some(repository)
        && head_ref == Some(definition.destination.sync_branch.as_str())
        && is_sha(head_sha)
        && base_ref == Some(definition.destination.branch.as_str());
    contract(ok, "Malformed destination PR or sync-hold state")?;

    let labels = labels
        .expect("checked by the contract() call above")
        .iter()
        .filter_map(|label| label.get("name").and_then(Value::as_str).map(String::from))
        .collect();
    let html_url = pr
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(PullRequest {
        number: number.expect("checked by the contract() call above"),
        head_sha: head_sha.to_string(),
        labels,
        html_url,
    })
}

/// Ports `publicationBody` exactly: the same lines, in the same order,
/// joined by `"\n"` (the array's trailing `""` element gives the result a
/// trailing newline). The provenance JSON in the marker comment is compact
/// (`serde_json::to_string`), in `Provenance`'s declared field order, which
/// matches Node's object-literal order.
pub fn publication_body(definition: &Definition, report: &Report) -> String {
    let provenance_json = serde_json::to_string(&report.provenance).unwrap_or_default();
    let mut lines = vec![
        format!("<!-- repository-projection: {provenance_json} -->"),
        String::new(),
        format!("## {} projection", definition.name),
        String::new(),
        format!(
            "Mono source: {}@{}.",
            definition.source_repository, report.provenance.source_sha
        ),
        format!(
            "Prior destination base: {}.",
            report.provenance.prior_projected_base
        ),
        format!(
            "Projected content SHA-256: {}.",
            report.provenance.content_digest
        ),
        String::new(),
        format!(
            "{} changed; {} deleted.",
            report.copied_count, report.deleted_count
        ),
        String::new(),
    ];
    lines.extend(
        report
            .copied_paths
            .iter()
            .take(20)
            .map(|path| format!("- copy/update {path}")),
    );
    lines.extend(
        report
            .deleted_paths
            .iter()
            .take(20)
            .map(|path| format!("- delete {path}")),
    );
    lines.push(String::new());
    lines.push(
        "Mono owns projected source. Destination-owned CI and policy are preserved.".to_string(),
    );
    lines.push(
        "Apply the sync-hold label to this PR to suspend generated updates during intentional destination work."
            .to_string(),
    );
    lines.push(
        "Source-side verification does not establish destination CI health. Review destination checks before merging."
            .to_string(),
    );
    lines.push(String::new());
    lines.push("Change-Origin: generated".to_string());
    lines.push(String::new());
    lines.join("\n")
}

/// Ports the PR create/update calls inside `publishPreparedTree`. Returns
/// the confirmed PR's `html_url`.
pub fn create_or_update_pr(
    definition: &Definition,
    api: &dyn GitHubApi,
    existing: Option<&PullRequest>,
    body: &str,
) -> Result<String> {
    let repository = definition.destination.repository.as_str();
    let result = match existing {
        Some(pr) => {
            let endpoint = format!("repos/{repository}/pulls/{}", pr.number);
            api.call(
                "PATCH",
                &endpoint,
                Some(&serde_json::json!({ "body": body })),
            )?
        }
        None => {
            let endpoint = format!("repos/{repository}/pulls");
            let payload = serde_json::json!({
                "title": format!("chore: sync {} from Mono", definition.name),
                "body": body,
                "head": definition.destination.sync_branch,
                "base": definition.destination.branch,
            });
            api.call("POST", &endpoint, Some(&payload))?
        }
    }
    .unwrap_or(Value::Null);

    let number_ok = result.get("number").and_then(Value::as_i64).is_some();
    let html_url = result.get("html_url").and_then(Value::as_str);
    contract(
        number_ok && html_url.is_some(),
        "GitHub did not confirm the generated PR",
    )?;
    Ok(html_url
        .expect("checked by the contract() call above")
        .to_string())
}

/// The real GitHub REST client: `reqwest::blocking` against
/// `https://api.github.com/`, authenticated with a bearer token read from
/// `GH_TOKEN`.
pub struct RestApi {
    client: reqwest::blocking::Client,
    token: String,
}

const BASE_URL: &str = "https://api.github.com/";

impl RestApi {
    /// Reads the publication token from `GH_TOKEN`
    /// (`Invalid("Missing publication token")` when absent) and builds a
    /// client with a 30 second timeout.
    pub fn from_env() -> Result<RestApi> {
        let token = std::env::var("GH_TOKEN")
            .map_err(|_| Error::Invalid("Missing publication token".into()))?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| Error::Invalid(format!("Failed to build GitHub client: {error}")))?;
        Ok(RestApi { client, token })
    }
}

impl GitHubApi for RestApi {
    fn call(&self, method: &str, endpoint: &str, body: Option<&Value>) -> Result<Option<Value>> {
        let verb = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| Error::Invalid(format!("Invalid HTTP method: {method}")))?;
        let url = format!("{BASE_URL}{endpoint}");
        let mut request = self
            .client
            .request(verb, &url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.token),
            )
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(reqwest::header::USER_AGENT, "capobara");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|error| Error::Invalid(format!("GitHub {method} {endpoint}: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Invalid(format!(
                "GitHub {method} {endpoint}: {status}"
            )));
        }
        let text = response
            .text()
            .map_err(|error| Error::Invalid(format!("GitHub {method} {endpoint}: {error}")))?;
        if text.trim().is_empty() {
            Ok(None)
        } else {
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| Error::Invalid(format!("GitHub {method} {endpoint}: {error}")))?;
            Ok(Some(value))
        }
    }
}

/// A test double that replays a fixed sequence of `(method, endpoint)`
/// calls, each with a canned response. Panics on any call whose
/// `(method, endpoint)` does not match the next expected pair, or on a call
/// made once the sequence is exhausted, naming the endpoint either way.
#[cfg(any(test, feature = "recorded-api"))]
pub struct RecordedApi {
    expected: std::cell::RefCell<std::collections::VecDeque<(String, String, Value)>>,
    calls: std::cell::RefCell<Vec<(String, String, Option<Value>)>>,
}

#[cfg(any(test, feature = "recorded-api"))]
impl RecordedApi {
    pub fn new(expected: Vec<(&str, &str, Value)>) -> RecordedApi {
        RecordedApi {
            expected: std::cell::RefCell::new(
                expected
                    .into_iter()
                    .map(|(method, endpoint, response)| {
                        (method.to_string(), endpoint.to_string(), response)
                    })
                    .collect(),
            ),
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The `(method, endpoint, body)` calls made so far, in order.
    pub fn calls(&self) -> Vec<(String, String, Option<Value>)> {
        self.calls.borrow().clone()
    }
}

#[cfg(any(test, feature = "recorded-api"))]
impl GitHubApi for RecordedApi {
    fn call(&self, method: &str, endpoint: &str, body: Option<&Value>) -> Result<Option<Value>> {
        let next = self.expected.borrow_mut().pop_front();
        let Some((expected_method, expected_endpoint, response)) = next else {
            panic!("RecordedApi: unexpected call to {method} {endpoint} (no calls remain)");
        };
        if expected_method != method || expected_endpoint != endpoint {
            panic!(
                "RecordedApi: unexpected call to {method} {endpoint} (expected {expected_method} {expected_endpoint})"
            );
        }
        self.calls
            .borrow_mut()
            .push((method.to_string(), endpoint.to_string(), body.cloned()));
        Ok(if response.is_null() {
            None
        } else {
            Some(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_uri_component_matches_javascript_semantics() {
        assert_eq!(encode_uri_component(":"), "%3A");
        assert_eq!(encode_uri_component("/"), "%2F");
        assert_eq!(
            encode_uri_component("owner:sync/branch"),
            "owner%3Async%2Fbranch"
        );
        assert_eq!(
            encode_uri_component("A-Za-z0-9-_.!~*'()"),
            "A-Za-z0-9-_.!~*'()"
        );
        assert_eq!(encode_uri_component(" "), "%20");
    }
}
