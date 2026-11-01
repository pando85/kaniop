//! Deterministic Kanidm identity for individual RADIUS custom resources, and
//! the executors that ensure and delete it.
//!
//! A Kubernetes object name is reusable after deletion. Kanidm accounts live
//! outside Kubernetes garbage collection, so using only namespace/name would
//! accidentally adopt a previous or unrelated account after recreation.
//! Instead the immutable Kubernetes UID identifies the account. Always inspect
//! existing account ownership metadata before updating it; a matching computed
//! name alone must never authorize adoption.
//!
//! Each CR owns exactly one account, joined to the built-in
//! `idm_radius_servers` group *additively*: only this account's membership
//! edge is ever added or removed, never `group_set_members`, which could
//! revoke unrelated RADIUS instances. Deleting the account removes every
//! token on it. Call the executors only after namespace authorization, the
//! restore guard and input validation.

use kanidm_client::{ClientError, KanidmClient};
use kanidm_proto::internal::OperationError;
use kanidm_proto::v1::Entry;
use kaniop_k8s_util::types::normalize_spn;
use kaniop_operator::metrics::{
    ControllerMetrics, KANIDM_OP_ADD_MEMBERS, KANIDM_OP_CREATE, KANIDM_OP_DELETE, KANIDM_OP_GET,
    KANIDM_OP_REMOVE_MEMBERS, KANIDM_OUTCOME_CHANGED, KANIDM_OUTCOME_UNCHANGED,
};
use kube::ResourceExt;
use thiserror::Error;
use uuid::Uuid;

use crate::crd::KanidmRadiusServer;
use crate::reconcile::error::{ExecError, ExecResult, kanidm_error};
use crate::reconcile::sdk;

/// Built-in Kanidm group whose members may act as RADIUS servers.
pub const RADIUS_GROUP: &str = "idm_radius_servers";
const ENTRY_MANAGER: &str = "idm_admin";

const RADIUS_ACCOUNT_PREFIX: &str = "kaniop_radius_";
const DISPLAY_NAME_PREFIX: &str = "Kaniop RADIUS: ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadiusIdentity {
    /// Stable across controller restarts, distinct across CR re-creations.
    pub account_name: String,
    /// A human-readable ownership marker for the Kanidm display name.
    pub display_name: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusIdentityError {
    #[error("RADIUS resource requires a valid Kubernetes UID")]
    InvalidUid,
    #[error("RADIUS resource requires a namespace and a name")]
    MissingObjectIdentity,
}

impl RadiusIdentity {
    pub fn new(namespace: &str, name: &str, uid: &str) -> Result<Self, RadiusIdentityError> {
        if namespace.is_empty() || name.is_empty() {
            return Err(RadiusIdentityError::MissingObjectIdentity);
        }
        let uid = Uuid::parse_str(uid).map_err(|_| RadiusIdentityError::InvalidUid)?;
        // Kanidm account names permit ASCII underscores. The UID's 128 bits
        // avoid name collision between independent RADIUS resources without
        // requiring a shared lock or incrementing counter.
        let account_name = format!("{RADIUS_ACCOUNT_PREFIX}{}", uid.simple());
        let display_name = format!(
            "{DISPLAY_NAME_PREFIX}{namespace}/{name} [{}]",
            uid.hyphenated()
        );
        Ok(Self {
            account_name,
            display_name,
        })
    }

    /// Do not silently adopt an account if an operator restart finds an
    /// already-existing Kanidm entry. The caller must inspect the account's
    /// display name and return an ownership conflict when this is false.
    pub fn matches_existing_display_name(&self, display_name: &str) -> bool {
        self.display_name == display_name
    }

    /// Identity of a CR, from its namespace, name and immutable UID.
    pub fn for_resource(radius: &KanidmRadiusServer) -> Result<Self, RadiusIdentityError> {
        let namespace = radius
            .namespace()
            .ok_or(RadiusIdentityError::MissingObjectIdentity)?;
        let uid = radius
            .metadata
            .uid
            .as_deref()
            .ok_or(RadiusIdentityError::InvalidUid)?;
        Self::new(&namespace, &radius.name_any(), uid)
    }
}

/// What exists in Kanidm under this identity's account name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountState {
    Absent,
    /// The display-name marker names this CR (namespace, name and UID).
    Owned,
    /// An account with this name exists without our marker.
    Foreign,
}

/// Classify a fetched account. Only the exact marker authorizes use; never
/// update an existing account to *make it match* (silent adoption).
pub fn account_state(entry: Option<&Entry>, identity: &RadiusIdentity) -> AccountState {
    match entry {
        None => AccountState::Absent,
        Some(entry)
            if entry.attrs.get("displayname").is_some_and(|names| {
                names.len() == 1 && identity.matches_existing_display_name(&names[0])
            }) =>
        {
            AccountState::Owned
        }
        Some(_) => AccountState::Foreign,
    }
}

/// Is `account` in a `member` attribute list (names or SPNs)?
pub fn is_member(members: &[String], account: &str) -> bool {
    let wanted = normalize_spn(account);
    members.iter().any(|member| normalize_spn(member) == wanted)
}

fn is_conflict(error: &ClientError) -> bool {
    matches!(error, ClientError::Http(status, _, _) if status.as_u16() == 409)
}

/// A delete of something that is already gone.
pub(crate) fn is_not_found(error: &ClientError) -> bool {
    matches!(error, ClientError::Http(status, _, _) if status.as_u16() == 404)
        || matches!(
            error,
            ClientError::Http(_, Some(OperationError::NoMatchingEntries), _)
        )
}

/// What [`ensure_identity`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityOutcome {
    pub account_created: bool,
    pub membership_added: bool,
}

async fn get_account(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    identity: &RadiusIdentity,
) -> ExecResult<AccountState> {
    let entry = sdk::call(
        metrics,
        KANIDM_OP_GET,
        KANIDM_OUTCOME_UNCHANGED,
        client.idm_service_account_get(&identity.account_name),
    )
    .await
    .map_err(|e| kanidm_error("get", "RADIUS service account", e))?;
    Ok(account_state(entry.as_ref(), identity))
}

async fn members(client: &KanidmClient, metrics: &ControllerMetrics) -> ExecResult<Vec<String>> {
    Ok(sdk::call(
        metrics,
        KANIDM_OP_GET,
        KANIDM_OUTCOME_UNCHANGED,
        client.idm_group_get_members(RADIUS_GROUP),
    )
    .await
    .map_err(|e| kanidm_error("get members of", RADIUS_GROUP, e))?
    .unwrap_or_default())
}

/// Ensure the per-CR account (created, never adopted) and its own membership
/// edge in `idm_radius_servers`. Mints, rotates and revokes nothing.
///
/// A same-named account without our display-name marker is
/// [`ExecError::AccountNotOwned`].
pub async fn ensure_identity(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    identity: &RadiusIdentity,
) -> ExecResult<IdentityOutcome> {
    let name = &identity.account_name;
    let mut account_created = false;
    match get_account(client, metrics, identity).await? {
        AccountState::Owned => {}
        AccountState::Foreign => return Err(not_owned(identity)),
        AccountState::Absent => {
            match sdk::call(
                metrics,
                KANIDM_OP_CREATE,
                KANIDM_OUTCOME_CHANGED,
                client.idm_service_account_create(name, &identity.display_name, ENTRY_MANAGER),
            )
            .await
            {
                Ok(()) => account_created = true,
                // A concurrent reconcile may have created it; verify the
                // marker instead of adopting.
                Err(error) if is_conflict(&error) => {
                    if get_account(client, metrics, identity).await? != AccountState::Owned {
                        return Err(not_owned(identity));
                    }
                }
                Err(error) => {
                    return Err(kanidm_error("create", "RADIUS service account", error));
                }
            }
        }
    }

    let membership_added = if is_member(&members(client, metrics).await?, name) {
        false
    } else {
        sdk::call(
            metrics,
            KANIDM_OP_ADD_MEMBERS,
            KANIDM_OUTCOME_CHANGED,
            client.idm_group_add_members(RADIUS_GROUP, &[name.as_str()]),
        )
        .await
        .map_err(|e| kanidm_error("add member to", RADIUS_GROUP, e))?;
        true
    };
    Ok(IdentityOutcome {
        account_created,
        membership_added,
    })
}

fn not_owned(identity: &RadiusIdentity) -> ExecError {
    ExecError::AccountNotOwned {
        account: identity.account_name.clone(),
    }
}

/// What [`delete_identity`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// The account was already gone.
    AlreadyAbsent,
    /// Membership edge removed (if present) and account deleted. Every token
    /// on the account went with it.
    Deleted,
}

/// Remove exactly this CR's membership edge, then delete its account. Call
/// only after the workload has drained. An account without our marker is
/// [`ExecError::AccountNotOwned`] and is left untouched (the finalizer turns
/// that into a skip with a Warning event).
pub async fn delete_identity(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    identity: &RadiusIdentity,
) -> ExecResult<DeleteOutcome> {
    let name = &identity.account_name;
    match get_account(client, metrics, identity).await? {
        AccountState::Absent => return Ok(DeleteOutcome::AlreadyAbsent),
        AccountState::Foreign => return Err(not_owned(identity)),
        AccountState::Owned => {}
    }
    if is_member(&members(client, metrics).await?, name) {
        // Only this UID-scoped edge; never the group's other members.
        sdk::call(
            metrics,
            KANIDM_OP_REMOVE_MEMBERS,
            KANIDM_OUTCOME_CHANGED,
            client.idm_group_remove_members(RADIUS_GROUP, &[name.as_str()]),
        )
        .await
        .map_err(|e| kanidm_error("remove member from", RADIUS_GROUP, e))?;
    }
    match sdk::call(
        metrics,
        KANIDM_OP_DELETE,
        KANIDM_OUTCOME_CHANGED,
        client.idm_service_account_delete(name),
    )
    .await
    {
        Ok(()) => Ok(DeleteOutcome::Deleted),
        Err(error) if is_not_found(&error) => Ok(DeleteOutcome::AlreadyAbsent),
        Err(error) => Err(kanidm_error("delete", "RADIUS service account", error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";
    const SECOND_UID: &str = "ad8cf3d5-b2a9-44d6-b257-058554ab09d5";

    #[test]
    fn identity_is_stable_across_reconciliation() {
        let a = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        let b = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.account_name,
            "kaniop_radius_d10066c345994487972d26bb42587a32"
        );
        assert!(a.matches_existing_display_name(&a.display_name));
    }

    #[test]
    fn same_name_recreated_with_new_uid_does_not_adopt_previous_account() {
        let old = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        let new = RadiusIdentity::new("network", "radius", SECOND_UID).unwrap();
        assert_ne!(old.account_name, new.account_name);
        assert!(!new.matches_existing_display_name(&old.display_name));
    }

    #[test]
    fn different_resources_cannot_share_the_same_uid_identity_marker() {
        // An unexpectedly reused UID would not be sufficient to authorize an
        // existing account with different namespace/name metadata.
        let a = RadiusIdentity::new("network-a", "radius", FIRST_UID).unwrap();
        let b = RadiusIdentity::new("network-b", "radius", FIRST_UID).unwrap();
        assert_eq!(a.account_name, b.account_name);
        assert!(!b.matches_existing_display_name(&a.display_name));
    }

    #[test]
    fn rejects_absent_or_non_uuid_uid() {
        assert_eq!(
            RadiusIdentity::new("network", "radius", ""),
            Err(RadiusIdentityError::InvalidUid)
        );
        assert_eq!(
            RadiusIdentity::new("network", "radius", "wrong-uid"),
            Err(RadiusIdentityError::InvalidUid)
        );
        assert_eq!(
            RadiusIdentity::new("", "radius", FIRST_UID),
            Err(RadiusIdentityError::MissingObjectIdentity)
        );
    }

    #[test]
    fn forged_display_name_causes_ownership_conflict() {
        let identity = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        assert!(!identity.matches_existing_display_name("Someone else's service account"));
    }
}

#[cfg(test)]
mod executor_tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    use super::*;
    use crate::reconcile::kanidm_mock::{self, Request};

    const UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";
    const ACCOUNT: &str = "kaniop_radius_d10066c345994487972d26bb42587a32";
    const ACCOUNT_PATH: &str = "/v1/service_account/kaniop_radius_d10066c345994487972d26bb42587a32";
    const MEMBERS_PATH: &str = "/v1/group/idm_radius_servers/_attr/member";

    fn identity() -> RadiusIdentity {
        RadiusIdentity::new("network", "campus", UID).unwrap()
    }

    fn entry_json(display_name: &str) -> String {
        json!({"attrs": {"displayname": [display_name]}}).to_string()
    }

    /// Fake Kanidm state: the account (display name) and group members.
    #[derive(Default)]
    struct World {
        account: Option<String>,
        members: Vec<String>,
        create_conflicts: bool,
    }

    fn handler(world: Arc<Mutex<World>>) -> impl Fn(&Request) -> (u16, String) + Send + Sync {
        move |req| {
            let mut w = world.lock().unwrap();
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", ACCOUNT_PATH) => match &w.account {
                    Some(name) => (200, entry_json(name)),
                    None => (200, "null".into()),
                },
                ("GET", MEMBERS_PATH) => (200, serde_json::to_string(&w.members).unwrap()),
                ("POST", "/v1/service_account") => {
                    if w.create_conflicts {
                        return (409, "{}".into());
                    }
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    w.account = Some(body["attrs"]["displayname"][0].as_str().unwrap().into());
                    (200, "null".into())
                }
                ("POST", MEMBERS_PATH) => {
                    let added: Vec<String> = serde_json::from_str(&req.body).unwrap();
                    w.members.extend(added);
                    (200, "null".into())
                }
                ("DELETE", MEMBERS_PATH) => {
                    let removed: Vec<String> = serde_json::from_str(&req.body).unwrap();
                    w.members.retain(|m| !removed.contains(m));
                    (200, "null".into())
                }
                ("DELETE", ACCOUNT_PATH) => {
                    w.account = None;
                    (200, "null".into())
                }
                _ => (500, "{}".into()),
            }
        }
    }

    #[test]
    fn account_state_requires_the_exact_marker() {
        let id = identity();
        let owned: Entry =
            serde_json::from_value(json!({"attrs": {"displayname": [id.display_name.clone()]}}))
                .unwrap();
        assert_eq!(account_state(Some(&owned), &id), AccountState::Owned);
        assert_eq!(account_state(None, &id), AccountState::Absent);
        let other: Entry =
            serde_json::from_value(json!({"attrs": {"displayname": ["Someone else"]}})).unwrap();
        assert_eq!(account_state(Some(&other), &id), AccountState::Foreign);
        let none: Entry = serde_json::from_value(json!({"attrs": {}})).unwrap();
        assert_eq!(account_state(Some(&none), &id), AccountState::Foreign);
        let two: Entry = serde_json::from_value(
            json!({"attrs": {"displayname": [id.display_name.clone(), "x"]}}),
        )
        .unwrap();
        assert_eq!(account_state(Some(&two), &id), AccountState::Foreign);
    }

    #[test]
    fn membership_matches_names_and_spns() {
        let members = vec![
            "other@idm.example.com".to_string(),
            format!("{ACCOUNT}@idm.example.com"),
        ];
        assert!(is_member(&members, ACCOUNT));
        assert!(!is_member(&members[..1], ACCOUNT));
        assert!(!is_member(&[], ACCOUNT));
    }

    #[test]
    fn identity_for_resource_uses_namespace_name_and_uid() {
        let radius: KanidmRadiusServer = serde_json::from_value(json!({
            "apiVersion": "kaniop.rs/v1beta1", "kind": "KanidmRadiusServer",
            "metadata": {"name": "campus", "namespace": "network", "uid": UID},
            "spec": {
                "kanidmRef": {"name": "idm"},
                "authorization": {"requiredGroups": ["allowed@idm.example.com"]},
                "clients": [{
                    "name": "nas",
                    "ipaddr": "192.0.2.1",
                    "sharedSecretRef": {"name": "nas-secret", "key": "nas"}
                }],
                "radiusTls": {"secretRef": {"name": "radius-tls"}}
            }
        }))
        .unwrap();
        assert_eq!(RadiusIdentity::for_resource(&radius).unwrap(), identity());
        let mut no_uid = radius.clone();
        no_uid.metadata.uid = None;
        assert_eq!(
            RadiusIdentity::for_resource(&no_uid),
            Err(RadiusIdentityError::InvalidUid)
        );
    }

    #[tokio::test]
    async fn ensure_creates_account_and_adds_only_its_own_membership() {
        let world = Arc::new(Mutex::new(World {
            members: vec!["unrelated_radius_server".into()],
            ..Default::default()
        }));
        let mock = kanidm_mock::start(handler(world.clone())).await;
        let outcome = ensure_identity(&mock.client, &mock.metrics, &identity())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            IdentityOutcome {
                account_created: true,
                membership_added: true
            }
        );
        let w = world.lock().unwrap();
        assert_eq!(w.account.as_deref(), Some(identity().display_name.as_str()));
        assert_eq!(w.members, ["unrelated_radius_server", ACCOUNT]);
        // Additive membership only: never a set/purge of the group.
        assert!(
            mock.writes().iter().all(|r| r.method == "POST"
                && (r.path == "/v1/service_account" || r.path == MEMBERS_PATH))
        );
    }

    #[tokio::test]
    async fn ensure_is_idempotent() {
        let world = Arc::new(Mutex::new(World {
            account: Some(identity().display_name),
            members: vec![ACCOUNT.into()],
            ..Default::default()
        }));
        let mock = kanidm_mock::start(handler(world)).await;
        let outcome = ensure_identity(&mock.client, &mock.metrics, &identity())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            IdentityOutcome {
                account_created: false,
                membership_added: false
            }
        );
        assert!(mock.writes().is_empty());
    }

    #[tokio::test]
    async fn ensure_never_adopts_a_foreign_account() {
        let world = Arc::new(Mutex::new(World {
            account: Some("Hand-made RADIUS account".into()),
            ..Default::default()
        }));
        let mock = kanidm_mock::start(handler(world.clone())).await;
        let error = ensure_identity(&mock.client, &mock.metrics, &identity())
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::AccountNotOwned { .. }));
        assert!(mock.writes().is_empty(), "no update, membership or delete");
        assert_eq!(
            world.lock().unwrap().account.as_deref(),
            Some("Hand-made RADIUS account")
        );
    }

    #[tokio::test]
    async fn ensure_after_a_create_conflict_verifies_the_marker() {
        // Lost the creation race to a reconcile that wrote the marker.
        let world = Arc::new(Mutex::new(World::default()));
        let calls = Arc::new(Mutex::new(0));
        let (w, c) = (world.clone(), calls.clone());
        let inner = handler(world.clone());
        let mock = kanidm_mock::start(move |req| {
            if req.method == "GET" && req.path == ACCOUNT_PATH {
                let mut n = c.lock().unwrap();
                *n += 1;
                if *n == 2 {
                    // After our 409, the other writer's account is visible.
                    w.lock().unwrap().account = Some(identity().display_name);
                }
            }
            if req.method == "POST" && req.path == "/v1/service_account" {
                return (409, "{}".into());
            }
            inner(req)
        })
        .await;
        let outcome = ensure_identity(&mock.client, &mock.metrics, &identity())
            .await
            .unwrap();
        assert!(!outcome.account_created);
        assert!(outcome.membership_added);

        // The same race against a foreign account is a conflict.
        let world = Arc::new(Mutex::new(World {
            create_conflicts: true,
            ..Default::default()
        }));
        let (w, c) = (world.clone(), Arc::new(Mutex::new(0)));
        let inner = handler(world);
        let mock = kanidm_mock::start(move |req| {
            if req.method == "GET" && req.path == ACCOUNT_PATH {
                let mut n = c.lock().unwrap();
                *n += 1;
                if *n == 2 {
                    w.lock().unwrap().account = Some("Foreign".into());
                }
            }
            inner(req)
        })
        .await;
        assert!(matches!(
            ensure_identity(&mock.client, &mock.metrics, &identity()).await,
            Err(ExecError::AccountNotOwned { .. })
        ));
    }

    #[tokio::test]
    async fn delete_removes_only_our_edge_then_the_account() {
        let world = Arc::new(Mutex::new(World {
            account: Some(identity().display_name),
            members: vec!["unrelated_radius_server".into(), ACCOUNT.into()],
            ..Default::default()
        }));
        let mock = kanidm_mock::start(handler(world.clone())).await;
        assert_eq!(
            delete_identity(&mock.client, &mock.metrics, &identity())
                .await
                .unwrap(),
            DeleteOutcome::Deleted
        );
        let w = world.lock().unwrap();
        assert!(w.account.is_none());
        assert_eq!(w.members, ["unrelated_radius_server"]);
        let writes = mock.writes();
        assert_eq!(writes[0].path, MEMBERS_PATH);
        assert_eq!(writes[0].body, format!("[\"{ACCOUNT}\"]"));
        assert_eq!(writes[1].path, ACCOUNT_PATH);
    }

    #[tokio::test]
    async fn delete_absent_account_is_success_and_foreign_is_untouched() {
        let mock = kanidm_mock::start(handler(Arc::new(Mutex::new(World::default())))).await;
        assert_eq!(
            delete_identity(&mock.client, &mock.metrics, &identity())
                .await
                .unwrap(),
            DeleteOutcome::AlreadyAbsent
        );
        assert!(mock.writes().is_empty());

        let world = Arc::new(Mutex::new(World {
            account: Some("Not ours".into()),
            members: vec![ACCOUNT.into()],
            ..Default::default()
        }));
        let mock = kanidm_mock::start(handler(world.clone())).await;
        let error = delete_identity(&mock.client, &mock.metrics, &identity())
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::AccountNotOwned { .. }));
        assert!(mock.writes().is_empty());
        assert!(world.lock().unwrap().account.is_some());
    }

    #[tokio::test]
    async fn delete_treats_a_vanished_account_as_done() {
        let world = Arc::new(Mutex::new(World {
            account: Some(identity().display_name),
            ..Default::default()
        }));
        let inner = handler(world);
        let mock = kanidm_mock::start(move |req| {
            if req.method == "DELETE" && req.path == ACCOUNT_PATH {
                return (404, "\"NoMatchingEntries\"".into());
            }
            inner(req)
        })
        .await;
        assert_eq!(
            delete_identity(&mock.client, &mock.metrics, &identity())
                .await
                .unwrap(),
            DeleteOutcome::AlreadyAbsent
        );
    }

    #[tokio::test]
    async fn kanidm_outage_keeps_its_source() {
        let mock = kanidm_mock::start(|_| (503, "{}".into())).await;
        let error = ensure_identity(&mock.client, &mock.metrics, &identity())
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::Api(_)));
    }
}
