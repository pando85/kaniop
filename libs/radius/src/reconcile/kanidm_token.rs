//! Kanidm token executors: list, mint, verify and revoke.
//!
//! These perform the effects the pure planner ([`super::token`]) decides on.
//! They never choose *what* to do: minting happens only for a
//! `CurrentToken::Mint` plan, revoking only for ids in a plan computed from a
//! fresh observation (see the planner's module docs).
//!
//! Bearer tokens are write-only and shown once by Kanidm. [`Bearer`] holds
//! one without `Display`, `Serialize` or a revealing `Debug`, so it cannot
//! reach a log line or event by accident; call [`Bearer::expose`] only to
//! write the config Secret or authenticate a verification session.

use std::fmt;
use std::time::Duration;

use kanidm_client::KanidmClient;
use kanidm_proto::internal::ApiToken;
use kaniop_operator::metrics::{
    ControllerMetrics, KANIDM_OP_DESTROY_API_TOKEN, KANIDM_OP_GENERATE_API_TOKEN,
    KANIDM_OP_LIST_API_TOKENS, KANIDM_OP_WHOAMI, KANIDM_OUTCOME_CHANGED, KANIDM_OUTCOME_UNCHANGED,
};
use time::OffsetDateTime;
use tracing::debug;
use uuid::Uuid;

use crate::reconcile::error::{ExecError, ExecResult, kanidm_error};
use crate::reconcile::identity::is_not_found;
use crate::reconcile::sdk;

const TOKEN_LABEL_PREFIX: &str = "kaniop-radius-";

/// A Kanidm bearer token. Deliberately opaque: see the module docs.
#[derive(Clone, PartialEq, Eq)]
pub struct Bearer(String);

impl Bearer {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The token text. Never log or put it in status, events or errors.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Bearer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Bearer(<redacted>)")
    }
}

/// A freshly minted, not yet verified or stored token.
#[derive(Debug)]
pub struct MintedToken {
    pub token_id: Uuid,
    pub bearer: Bearer,
}

/// A unique label for one mint. The account is dedicated to one CR UID, so
/// the label is informational (it names the issuer in Kanidm's UI) and the
/// uniqueness only serves to find the new token's id after the call.
pub fn new_token_label() -> String {
    format!("{TOKEN_LABEL_PREFIX}{}", Uuid::new_v4().simple())
}

/// Find the one token carrying `label` that was not in `before`.
///
/// `generate_api_token` returns only the bearer, so the token id comes from a
/// second listing. Anything other than exactly one new match is ambiguous and
/// the token is left alone (the planner revokes unused orphans after the
/// grace period).
pub fn find_minted_token(before: &[ApiToken], after: &[ApiToken], label: &str) -> Option<Uuid> {
    let mut matching = after
        .iter()
        .filter(|token| token.label == label)
        .filter(|token| before.iter().all(|old| old.token_id != token.token_id));
    let found = matching.next()?;
    matching.next().is_none().then_some(found.token_id)
}

/// Tokens currently on the account.
pub async fn list_tokens(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    account: &str,
) -> ExecResult<Vec<ApiToken>> {
    sdk::call(
        metrics,
        KANIDM_OP_LIST_API_TOKENS,
        KANIDM_OUTCOME_UNCHANGED,
        client.idm_service_account_list_api_token(account),
    )
    .await
    .map_err(|e| kanidm_error("list API tokens of", "RADIUS service account", e))
}

/// Mint a read-only token on the (already ownership-verified) account.
///
/// `expiry` is `TokenPolicy::new_token_expiry(now)`: set when rotation is
/// enabled, absent otherwise. The bearer is not stored anywhere yet: the
/// caller must verify it and persist it in the config Secret in the same
/// reconcile. If this call fails after Kanidm accepted the request, the
/// token is an orphan the planner revokes once past the grace period.
pub async fn mint_token(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    account: &str,
    expiry: Option<OffsetDateTime>,
) -> ExecResult<MintedToken> {
    let label = new_token_label();
    let before = list_tokens(client, metrics, account).await?;
    let bearer = sdk::call(
        metrics,
        KANIDM_OP_GENERATE_API_TOKEN,
        KANIDM_OUTCOME_CHANGED,
        client.idm_service_account_generate_api_token(
            account, &label, expiry, false, // read-only is mandatory for RADIUS
            false, // not compact
        ),
    )
    .await
    .map_err(|e| kanidm_error("generate API token for", "RADIUS service account", e))?;
    let after = list_tokens(client, metrics, account).await?;
    let token_id = find_minted_token(&before, &after, &label).ok_or_else(|| {
        ExecError::MintedTokenNotFound {
            account: account.to_string(),
        }
    })?;
    Ok(MintedToken {
        token_id,
        bearer: Bearer::new(bearer),
    })
}

/// How [`verify_token`] retries. Right after a mint, a Kanidm replica other
/// than the one that issued the token may not know it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyRetry {
    pub attempts: u32,
    pub delay: Duration,
}

impl Default for VerifyRetry {
    fn default() -> Self {
        Self {
            attempts: 5,
            delay: Duration::from_secs(1),
        }
    }
}

/// Promotion gate: authenticate to Kanidm with the candidate token
/// alone. `KanidmClient::new_session` copies the cached client's address and
/// TLS settings but not its bearer, so a success proves the candidate itself.
///
/// A 401 (`whoami` returns `None`) is retried like a transient error, then
/// reported as [`ExecError::CandidateRejected`]. Transport and server errors
/// that persist through the retries are returned with their source.
pub async fn verify_token(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    account: &str,
    bearer: &Bearer,
    retry: VerifyRetry,
) -> ExecResult<()> {
    let session = client
        .new_session()
        .map_err(|e| kanidm_error("open session for", "token verification", e))?;
    session.set_token(bearer.expose().to_string()).await;
    let attempts = retry.attempts.max(1);
    let mut last: Option<ExecError> = None;
    for attempt in 1..=attempts {
        if attempt > 1 {
            tokio::time::sleep(retry.delay).await;
        }
        match sdk::call(
            metrics,
            KANIDM_OP_WHOAMI,
            KANIDM_OUTCOME_UNCHANGED,
            session.whoami(),
        )
        .await
        {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {
                last = Some(ExecError::CandidateRejected {
                    account: account.to_string(),
                });
            }
            Err(error) => {
                last = Some(kanidm_error(
                    "verify token of",
                    "RADIUS service account",
                    error,
                ))
            }
        }
        debug!(attempt, attempts, "candidate token not accepted yet");
    }
    Err(last.unwrap_or_else(|| ExecError::CandidateRejected {
        account: account.to_string(),
    }))
}

/// What [`revoke_token`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevokeOutcome {
    Revoked,
    /// Kanidm no longer has the token (or account): the goal is met.
    AlreadyAbsent,
}

/// Revoke one token by id. A token that is already gone is success, so the
/// call is idempotent across retries and concurrent reconciles.
pub async fn revoke_token(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    account: &str,
    token_id: Uuid,
) -> ExecResult<RevokeOutcome> {
    match sdk::call(
        metrics,
        KANIDM_OP_DESTROY_API_TOKEN,
        KANIDM_OUTCOME_CHANGED,
        client.idm_service_account_destroy_api_token(account, token_id),
    )
    .await
    {
        Ok(()) => Ok(RevokeOutcome::Revoked),
        Err(error) if is_not_found(&error) => Ok(RevokeOutcome::AlreadyAbsent),
        Err(error) => Err(kanidm_error(
            "revoke API token of",
            "RADIUS service account",
            error,
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use http::StatusCode;
    use kanidm_client::ClientError;
    use serde_json::{Value, json};

    use super::*;
    use crate::reconcile::kanidm_mock::{self, Request};

    const ACCOUNT: &str = "kaniop_radius_d10066c345994487972d26bb42587a32";
    const TOKEN_A: &str = "9ad74e68-147f-49e8-96db-dc21c920ff3b";
    const TOKEN_B: &str = "31b30b65-037a-418f-b7ca-0c376da2db28";

    fn token(id: &str, label: &str) -> ApiToken {
        serde_json::from_value(json!({
            "account_id": "00000000-0000-0000-0000-000000000001",
            "token_id": id,
            "label": label,
            "expiry": null,
            "issued_at": 1_700_000_000,
            "purpose": "readonly"
        }))
        .unwrap()
    }

    fn token_json(t: &ApiToken) -> Value {
        serde_json::to_value(t).unwrap()
    }

    #[test]
    fn label_is_unique_and_well_formed() {
        let a = new_token_label();
        let b = new_token_label();
        assert_ne!(a, b);
        assert!(a.starts_with("kaniop-radius-") && a.len() == 14 + 32);
    }

    #[test]
    fn finds_exactly_one_new_token_by_label() {
        let old = token(TOKEN_A, "kaniop-radius-old");
        let new = token(TOKEN_B, "kaniop-radius-new");
        let before = [old.clone()];
        let after = [old.clone(), new];
        assert_eq!(
            find_minted_token(&before, &after, "kaniop-radius-new"),
            Some(uuid(TOKEN_B))
        );
        // No such label.
        assert_eq!(find_minted_token(&before, &after, "other"), None);
        // The label existed before: that is not our new token.
        assert_eq!(find_minted_token(&after, &after, "kaniop-radius-new"), None);
        // Two new tokens share the label: ambiguous, so none.
        let dup = [
            old.clone(),
            token(TOKEN_B, "x"),
            token(TOKEN_A.replace('9', "8").as_str(), "x"),
        ];
        assert_eq!(find_minted_token(&before, &dup, "x"), None);
    }

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    #[test]
    fn bearer_never_shows_in_debug() {
        let minted = MintedToken {
            token_id: uuid(TOKEN_A),
            bearer: Bearer::new("eyJ-secret-bearer".into()),
        };
        let text = format!("{minted:?} {:?}", minted.bearer);
        assert!(!text.contains("eyJ-secret-bearer"), "{text}");
        assert_eq!(minted.bearer.expose(), "eyJ-secret-bearer");
    }

    #[test]
    fn not_found_detection() {
        let http =
            |code: u16| ClientError::Http(StatusCode::from_u16(code).unwrap(), None, String::new());
        assert!(is_not_found(&http(404)));
        assert!(!is_not_found(&http(500)));
        assert!(!is_not_found(&http(403)));
        assert!(!is_not_found(&ClientError::Unauthorized));
        let no_match = ClientError::Http(
            StatusCode::BAD_REQUEST,
            Some(kanidm_proto::internal::OperationError::NoMatchingEntries),
            String::new(),
        );
        assert!(is_not_found(&no_match));
    }

    #[tokio::test]
    async fn mint_requests_a_read_only_token_with_expiry_and_finds_its_id() {
        let listed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let label = Arc::new(Mutex::new(None::<String>));
        let (l, lb) = (listed.clone(), label.clone());
        let mock = kanidm_mock::start(move |req: &Request| {
            let list_path = format!("/v1/service_account/{ACCOUNT}/_api_token");
            match (req.method.as_str(), req.path == list_path) {
                ("GET", true) => (200, serde_json::to_string(&*l.lock().unwrap()).unwrap()),
                ("POST", true) => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    let new_label = body["label"].as_str().unwrap().to_string();
                    l.lock()
                        .unwrap()
                        .push(token_json(&token(TOKEN_B, &new_label)));
                    *lb.lock().unwrap() = Some(new_label);
                    (200, "\"bearer-jws-value\"".into())
                }
                _ => (500, "{}".into()),
            }
        })
        .await;
        let expiry = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let minted = mint_token(&mock.client, &mock.metrics, ACCOUNT, Some(expiry))
            .await
            .unwrap();
        assert_eq!(minted.token_id, uuid(TOKEN_B));
        assert_eq!(minted.bearer.expose(), "bearer-jws-value");

        let post = mock.writes().pop().unwrap();
        let body: Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(body["read_write"], false, "RADIUS tokens are read-only");
        assert_eq!(body["compact"], false);
        assert!(!body["expiry"].is_null(), "expiry requested: {body}");
        assert_eq!(body["label"], label.lock().unwrap().clone().unwrap());
    }

    #[tokio::test]
    async fn mint_without_rotation_requests_no_expiry() {
        let mock = kanidm_mock::start(|req: &Request| match req.method.as_str() {
            "POST" => (200, "\"b\"".into()),
            _ => (200, "[]".into()),
        })
        .await;
        // The after-listing is empty, so the id cannot be found; the request
        // shape is what this checks.
        let error = mint_token(&mock.client, &mock.metrics, ACCOUNT, None)
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::MintedTokenNotFound { .. }));
        let body: Value = serde_json::from_str(&mock.writes()[0].body).unwrap();
        assert!(body["expiry"].is_null());
        assert!(!error.to_string().contains("\"b\""));
    }

    #[tokio::test]
    async fn mint_failure_keeps_source_and_creates_nothing_else() {
        let mock = kanidm_mock::start(|req: &Request| match req.method.as_str() {
            "POST" => (500, "{}".into()),
            _ => (200, "[]".into()),
        })
        .await;
        let error = mint_token(&mock.client, &mock.metrics, ACCOUNT, None)
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::Api(_)));
        assert_eq!(mock.writes().len(), 1);
    }

    fn whoami_ok() -> (u16, String) {
        (
            200,
            json!({"youare": {"attrs": {"name": [ACCOUNT]}}}).to_string(),
        )
    }

    const FAST: VerifyRetry = VerifyRetry {
        attempts: 3,
        delay: Duration::from_millis(0),
    };

    #[tokio::test]
    async fn verify_uses_only_the_candidate_token() {
        let mock = kanidm_mock::start(|_| whoami_ok()).await;
        // The shared client carries the operator's own credential.
        mock.client.set_token("operator-admin-token".into()).await;
        verify_token(
            &mock.client,
            &mock.metrics,
            ACCOUNT,
            &Bearer::new("candidate-token".into()),
            FAST,
        )
        .await
        .unwrap();
        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/v1/self");
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer candidate-token")
        );
        // The shared client's credential is untouched.
        assert_eq!(
            mock.client.get_token().await.as_deref(),
            Some("operator-admin-token")
        );
    }

    #[tokio::test]
    async fn verify_rejects_a_token_kanidm_does_not_accept() {
        let mock = kanidm_mock::start(|_| (401, "null".into())).await;
        let error = verify_token(
            &mock.client,
            &mock.metrics,
            ACCOUNT,
            &Bearer::new("candidate-token".into()),
            FAST,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ExecError::CandidateRejected { .. }));
        assert!(!error.to_string().contains("candidate-token"));
        assert_eq!(mock.requests().len(), 3, "retried for replication lag");
    }

    #[tokio::test]
    async fn verify_succeeds_once_a_lagging_replica_catches_up() {
        let calls = Arc::new(Mutex::new(0));
        let c = calls.clone();
        let mock = kanidm_mock::start(move |_| {
            let mut n = c.lock().unwrap();
            *n += 1;
            if *n < 3 {
                (401, "null".into())
            } else {
                whoami_ok()
            }
        })
        .await;
        verify_token(
            &mock.client,
            &mock.metrics,
            ACCOUNT,
            &Bearer::new("t".into()),
            FAST,
        )
        .await
        .unwrap();
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn verify_reports_persistent_server_errors_with_their_source() {
        let mock = kanidm_mock::start(|_| (503, "{}".into())).await;
        let error = verify_token(
            &mock.client,
            &mock.metrics,
            ACCOUNT,
            &Bearer::new("t".into()),
            FAST,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ExecError::Api(_)));
    }

    #[tokio::test]
    async fn revoke_by_id_and_already_absent_is_success() {
        let mock = kanidm_mock::start(|req: &Request| {
            if req.path.ends_with(TOKEN_A) {
                (200, "null".into())
            } else if req.path.ends_with(TOKEN_B) {
                (404, json!("NoMatchingEntries").to_string())
            } else {
                (500, "{}".into())
            }
        })
        .await;
        assert_eq!(
            revoke_token(&mock.client, &mock.metrics, ACCOUNT, uuid(TOKEN_A))
                .await
                .unwrap(),
            RevokeOutcome::Revoked
        );
        assert_eq!(
            revoke_token(&mock.client, &mock.metrics, ACCOUNT, uuid(TOKEN_B))
                .await
                .unwrap(),
            RevokeOutcome::AlreadyAbsent
        );
        let writes = mock.writes();
        assert_eq!(writes[0].method, "DELETE");
        assert_eq!(
            writes[0].path,
            format!("/v1/service_account/{ACCOUNT}/_api_token/{TOKEN_A}")
        );
    }

    #[tokio::test]
    async fn revoke_failure_is_an_error() {
        let mock = kanidm_mock::start(|_| (500, "{}".into())).await;
        let error = revoke_token(&mock.client, &mock.metrics, ACCOUNT, uuid(TOKEN_A))
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::Api(_)));
    }
}
