//! Destination transport: the GitHub REST API client and publication state
//! (`github`), and the destination git checkout preparation/validation/
//! publication pipeline (`git`), used by `capobara publish`. Ports
//! `readPublicationState`, `publicationBody`, `assertDestinationCheckout`,
//! `prepareDestination`, `assertCandidateMatchesMainProjection`, and
//! `publishPreparedTree` from `scripts/projections/transport.mjs`.

pub mod git;
pub mod github;

pub use git::{
    Candidate, PUBLICATION_ENGINE, Prepared, Published, assert_candidate_matches_main_projection,
    assert_destination_checkout, changed_paths, prepare_destination, publish_prepared_tree,
    push_sync_branch,
};
#[cfg(any(test, feature = "recorded-api"))]
pub use github::RecordedApi;
pub use github::{
    GitHubApi, PublicationState, PullRequest, RestApi, create_or_update_pr, encode_uri_component,
    publication_body, read_publication_state,
};
