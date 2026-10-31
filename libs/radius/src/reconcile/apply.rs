//! Ownership-checked Kubernetes writes for the RADIUS controller.
//!
//! Every generated object is checked with [`ensure_not_foreign`] before the
//! controller touches it, so a same-named Deployment or Service created by
//! someone else is reported as a conflict and never adopted, even when its
//! labels match. Generated objects are written with server-side apply under
//! [`FIELD_MANAGER`] with `force`, so a conflict with another field manager
//! (for example `kubectl scale`) cannot wedge the reconcile with a 409. Config
//! Secrets are create-only and immutable; deletes carry UID and
//! resourceVersion preconditions so a recreated object is never removed.
//!
//! Server-side apply cannot be made conditional on non-existence, so a
//! foreign object created between the read and the patch would be merged
//! into. The patch response is therefore checked again and a foreign result
//! is reported as a conflict; the window is a single API round trip.

use std::fmt::Debug;

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions, PropagationPolicy};
use kube::{Api, Resource, ResourceExt};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::reconcile::error::{ExecError, ExecResult, kube_error};
use crate::reconcile::meta::{ANNOTATION_ROTATION_REQUEST, ANNOTATION_TOKEN_ID};

/// Server-side apply field manager (plan: `kanidmradiusservers.kaniop.rs`).
pub const FIELD_MANAGER: &str = "kanidmradiusservers.kaniop.rs";

/// Is `meta` controlled by `owner`? The controller reference (at most one per
/// object) must name the owner's UID and kind. Extra non-controller
/// references are allowed; a missing, different or non-controller reference
/// is not ours.
pub fn is_controlled_by(meta: &ObjectMeta, owner: &OwnerReference) -> bool {
    meta.owner_references
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .any(|r| r.controller == Some(true) && r.uid == owner.uid && r.kind == owner.kind)
}

fn foreign<K: Resource<DynamicType = ()>>(object: &K) -> ExecError {
    ExecError::ForeignObject {
        kind: K::kind(&()).into_owned(),
        namespace: object.namespace().unwrap_or_default(),
        name: object.name_any(),
    }
}

/// Reject an existing object that is not controlled by `owner`.
pub fn ensure_not_foreign<K: Resource<DynamicType = ()>>(
    object: &K,
    owner: &OwnerReference,
) -> ExecResult<()> {
    if is_controlled_by(object.meta(), owner) {
        Ok(())
    } else {
        Err(foreign(object))
    }
}

/// Parameters for every server-side apply: our field manager, with `force`.
pub fn apply_params() -> PatchParams {
    PatchParams::apply(FIELD_MANAGER).force()
}

fn identity<K: Resource<DynamicType = ()>>(object: &K) -> ExecResult<(String, String)> {
    let name = object
        .meta()
        .name
        .clone()
        .ok_or(ExecError::MissingMetadata {
            kind: "object",
            field: "name",
        })?;
    Ok((object.namespace().unwrap_or_default(), name))
}

async fn get_optional<K>(api: &Api<K>, namespace: &str, name: &str) -> ExecResult<Option<K>>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug,
{
    match api.get(name).await {
        Ok(object) => Ok(Some(object)),
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(None),
        Err(error) => Err(kube_error("get", &K::kind(&()), namespace, name, error)),
    }
}

/// Read the object if present and fail on a foreign one, without writing.
/// Use it to refuse early (for example before minting a token).
pub async fn check_not_foreign<K>(
    api: &Api<K>,
    namespace: &str,
    name: &str,
    owner: &OwnerReference,
) -> ExecResult<Option<K>>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug,
{
    let existing = get_optional(api, namespace, name).await?;
    if let Some(existing) = &existing {
        ensure_not_foreign(existing, owner)?;
    }
    Ok(existing)
}

/// Server-side apply `desired` unless a foreign object holds the name.
pub async fn apply_owned<K>(api: &Api<K>, desired: &K, owner: &OwnerReference) -> ExecResult<K>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Serialize + Debug,
{
    let (namespace, name) = identity(desired)?;
    check_not_foreign(api, &namespace, &name, owner).await?;
    let applied = api
        .patch(&name, &apply_params(), &Patch::Apply(desired))
        .await
        .map_err(|e| kube_error("apply", &K::kind(&()), &namespace, &name, e))?;
    ensure_not_foreign(&applied, owner)?;
    Ok(applied)
}

/// What [`ensure_config_secret`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretOutcome {
    Created,
    /// An identical Secret of ours was already there.
    Existing,
}

/// The existing Secret must be ours and byte-for-byte what we would create:
/// same type, immutability, data, token id and rotation request. The name is
/// content-addressed, so a difference means a conflicting writer, not a
/// stale read.
pub fn verify_existing_secret(
    existing: &Secret,
    desired: &Secret,
    owner: &OwnerReference,
) -> ExecResult<()> {
    ensure_not_foreign(existing, owner)?;
    let annotation = |secret: &Secret, key: &str| {
        secret
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(key))
            .cloned()
    };
    let identical = existing.immutable == desired.immutable
        && existing.type_ == desired.type_
        && existing.data == desired.data
        && annotation(existing, ANNOTATION_TOKEN_ID) == annotation(desired, ANNOTATION_TOKEN_ID)
        && annotation(existing, ANNOTATION_ROTATION_REQUEST)
            == annotation(desired, ANNOTATION_ROTATION_REQUEST);
    if identical {
        Ok(())
    } else {
        Err(ExecError::SecretConflict {
            namespace: existing.namespace().unwrap_or_default(),
            name: existing.name_any(),
        })
    }
}

/// Create the immutable config Secret if absent. Never patches or
/// overwrites: an existing Secret (found by GET, or by a 409 from a racing
/// reconcile) must be ours and identical, otherwise it is a conflict.
pub async fn ensure_config_secret(
    api: &Api<Secret>,
    desired: &Secret,
    owner: &OwnerReference,
) -> ExecResult<SecretOutcome> {
    let (namespace, name) = identity(desired)?;
    if let Some(existing) = get_optional(api, &namespace, &name).await? {
        verify_existing_secret(&existing, desired, owner)?;
        return Ok(SecretOutcome::Existing);
    }
    let params = PostParams {
        field_manager: Some(FIELD_MANAGER.to_string()),
        ..Default::default()
    };
    match api.create(&params, desired).await {
        Ok(_) => Ok(SecretOutcome::Created),
        Err(kube::Error::Api(status)) if status.code == 409 => {
            let existing = get_optional(api, &namespace, &name).await?.ok_or_else(|| {
                // Created and deleted again inside one reconcile.
                ExecError::ConcurrentMutation {
                    kind: "Secret".into(),
                    namespace: namespace.clone(),
                    name: name.clone(),
                }
            })?;
            verify_existing_secret(&existing, desired, owner)?;
            Ok(SecretOutcome::Existing)
        }
        Err(error) => Err(kube_error("create", "Secret", &namespace, &name, error)),
    }
}

/// How an object is deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteMode {
    /// Plain delete (background propagation by default).
    Default,
    /// The object stays until its dependents (ReplicaSets, pods) are gone.
    Foreground,
}

/// Bind a delete to the exact object that was checked: a name can be deleted
/// and recreated between GET and DELETE, and without both preconditions the
/// replacement would be removed.
pub fn delete_params(meta: &ObjectMeta, mode: DeleteMode) -> ExecResult<DeleteParams> {
    let nonempty = |value: &Option<String>, field: &'static str| {
        value
            .as_ref()
            .filter(|v| !v.is_empty())
            .cloned()
            .ok_or(ExecError::MissingMetadata {
                kind: "object",
                field,
            })
    };
    let preconditions = Preconditions {
        uid: Some(nonempty(&meta.uid, "uid")?),
        resource_version: Some(nonempty(&meta.resource_version, "resourceVersion")?),
    };
    let params = match mode {
        DeleteMode::Default => DeleteParams::default(),
        DeleteMode::Foreground => DeleteParams {
            propagation_policy: Some(PropagationPolicy::Foreground),
            ..Default::default()
        },
    };
    Ok(params.preconditions(preconditions))
}

/// What [`delete_owned`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    AlreadyAbsent,
}

/// Delete an object we control. Absent is success; a foreign object is an
/// error and is never touched; failed preconditions are
/// [`ExecError::ConcurrentMutation`] (re-observe and retry).
pub async fn delete_owned<K>(
    api: &Api<K>,
    namespace: &str,
    name: &str,
    owner: &OwnerReference,
    mode: DeleteMode,
) -> ExecResult<DeleteOutcome>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug,
{
    let Some(existing) = check_not_foreign(api, namespace, name, owner).await? else {
        return Ok(DeleteOutcome::AlreadyAbsent);
    };
    let params = delete_params(existing.meta(), mode)?;
    match api.delete(name, &params).await {
        Ok(_) => Ok(DeleteOutcome::Deleted),
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(DeleteOutcome::AlreadyAbsent),
        Err(kube::Error::Api(status)) if status.code == 409 => Err(ExecError::ConcurrentMutation {
            kind: K::kind(&()).into_owned(),
            namespace: namespace.to_string(),
            name: name.to_string(),
        }),
        Err(error) => Err(kube_error("delete", &K::kind(&()), namespace, name, error)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use k8s_openapi::ByteString;
    use k8s_openapi::api::apps::v1::Deployment;
    use serde_json::json;

    use super::*;
    use crate::reconcile::kube_mock::{api_status, mock_client, to_json};

    const UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";
    const NS: &str = "network";

    fn owner() -> OwnerReference {
        OwnerReference {
            api_version: "kaniop.rs/v1beta1".into(),
            kind: "KanidmRadiusServer".into(),
            name: "campus".into(),
            uid: UID.into(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }

    fn foreign_owner() -> OwnerReference {
        OwnerReference {
            api_version: "apps/v1".into(),
            kind: "StatefulSet".into(),
            name: "someone-else".into(),
            uid: "other-uid".into(),
            controller: Some(true),
            block_owner_deletion: None,
        }
    }

    fn deployment(owners: Option<Vec<OwnerReference>>) -> Deployment {
        Deployment {
            metadata: ObjectMeta {
                name: Some("campus".into()),
                namespace: Some(NS.into()),
                uid: Some("dep-uid".into()),
                resource_version: Some("42".into()),
                owner_references: owners,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn secret(data: &[u8], owners: Vec<OwnerReference>) -> Secret {
        Secret {
            metadata: ObjectMeta {
                name: Some("campus-0123456789".into()),
                namespace: Some(NS.into()),
                annotations: Some(BTreeMap::from([(
                    ANNOTATION_TOKEN_ID.to_string(),
                    "9ad74e68-147f-49e8-96db-dc21c920ff3b".to_string(),
                )])),
                owner_references: Some(owners),
                ..Default::default()
            },
            data: Some(BTreeMap::from([(
                "auth_token".to_string(),
                ByteString(data.to_vec()),
            )])),
            immutable: Some(true),
            type_: Some("Opaque".into()),
            ..Default::default()
        }
    }

    #[test]
    fn controller_reference_decides_ownership() {
        let ours = deployment(Some(vec![owner()]));
        assert!(ensure_not_foreign(&ours, &owner()).is_ok());

        let ownerless = deployment(None);
        assert!(matches!(
            ensure_not_foreign(&ownerless, &owner()),
            Err(ExecError::ForeignObject { .. })
        ));
        let other = deployment(Some(vec![foreign_owner()]));
        assert!(matches!(
            ensure_not_foreign(&other, &owner()),
            Err(ExecError::ForeignObject { .. })
        ));
        // Same UID but not the controller reference is not ours.
        let mut weak = owner();
        weak.controller = Some(false);
        assert!(ensure_not_foreign(&deployment(Some(vec![weak])), &owner()).is_err());
        // Same name and kind, different UID (a recreated CR) is not ours.
        let mut recreated = owner();
        recreated.uid = "new-uid".into();
        assert!(ensure_not_foreign(&deployment(Some(vec![recreated])), &owner()).is_err());
        // An extra non-controller reference does not disown it.
        let mut extra = foreign_owner();
        extra.controller = None;
        assert!(ensure_not_foreign(&deployment(Some(vec![extra, owner()])), &owner()).is_ok());
    }

    #[test]
    fn foreign_error_names_object_without_content() {
        let error = ensure_not_foreign(&deployment(None), &owner()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Deployment network/campus exists but is not controlled by this resource"
        );
        assert!(!error.is_retryable());
    }

    #[test]
    fn apply_params_force_and_manager() {
        let params = apply_params();
        assert!(params.force, "SSA without force wedges on field conflicts");
        assert_eq!(params.field_manager.as_deref(), Some(FIELD_MANAGER));
        assert_eq!(FIELD_MANAGER, "kanidmradiusservers.kaniop.rs");
    }

    #[test]
    fn delete_params_need_uid_and_resource_version() {
        let mut meta = ObjectMeta::default();
        assert!(delete_params(&meta, DeleteMode::Default).is_err());
        meta.uid = Some("uid-1".into());
        assert!(delete_params(&meta, DeleteMode::Default).is_err());
        meta.resource_version = Some("".into());
        assert!(delete_params(&meta, DeleteMode::Default).is_err());
        meta.resource_version = Some("77".into());
        let params = delete_params(&meta, DeleteMode::Foreground).unwrap();
        assert!(matches!(
            params.propagation_policy,
            Some(PropagationPolicy::Foreground)
        ));
        let pre = params.preconditions.unwrap();
        assert_eq!(pre.uid.as_deref(), Some("uid-1"));
        assert_eq!(pre.resource_version.as_deref(), Some("77"));
        assert!(
            delete_params(&meta, DeleteMode::Default)
                .unwrap()
                .propagation_policy
                .is_none()
        );
    }

    #[tokio::test]
    async fn apply_creates_absent_object_with_forced_server_side_apply() {
        let desired = deployment(Some(vec![owner()]));
        let (client, recorder) =
            mock_client(vec![api_status(404, "NotFound"), (200, to_json(&desired))]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        apply_owned(&api, &desired, &owner()).await.unwrap();
        drop(api);
        let requests = recorder.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        let patch = &requests[1];
        assert_eq!(patch.method, "PATCH");
        assert!(patch.uri.contains("force=true"), "uri: {}", patch.uri);
        assert!(
            patch.uri.contains(&format!("fieldManager={FIELD_MANAGER}")),
            "uri: {}",
            patch.uri
        );
        assert_eq!(
            patch.content_type.as_deref(),
            Some("application/apply-patch+yaml")
        );
        assert_eq!(patch.json()["metadata"]["name"], "campus");
    }

    #[tokio::test]
    async fn apply_refuses_foreign_object_without_writing() {
        let desired = deployment(Some(vec![owner()]));
        let theirs = deployment(Some(vec![foreign_owner()]));
        let (client, recorder) = mock_client(vec![(200, to_json(&theirs))]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        let error = apply_owned(&api, &desired, &owner()).await.unwrap_err();
        assert!(matches!(error, ExecError::ForeignObject { .. }));
        drop(api);
        let requests = recorder.await.unwrap();
        assert_eq!(requests.len(), 1, "only the read, never a patch");
        assert_eq!(requests[0].method, "GET");
    }

    #[tokio::test]
    async fn apply_reports_foreign_object_that_appeared_after_the_read() {
        let desired = deployment(Some(vec![owner()]));
        let theirs = deployment(Some(vec![foreign_owner()]));
        let (client, recorder) =
            mock_client(vec![api_status(404, "NotFound"), (200, to_json(&theirs))]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        let error = apply_owned(&api, &desired, &owner()).await.unwrap_err();
        assert!(matches!(error, ExecError::ForeignObject { .. }));
        drop(api);
        recorder.await.unwrap();
    }

    #[tokio::test]
    async fn api_failure_keeps_its_source_and_is_retryable() {
        let desired = deployment(Some(vec![owner()]));
        let (client, recorder) = mock_client(vec![api_status(503, "ServiceUnavailable")]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        let error = apply_owned(&api, &desired, &owner()).await.unwrap_err();
        assert!(matches!(error, ExecError::Api(_)));
        assert!(error.is_retryable());
        drop(api);
        recorder.await.unwrap();
    }

    #[tokio::test]
    async fn secret_is_created_when_absent() {
        let desired = secret(b"bearer-one", vec![owner()]);
        let (client, recorder) =
            mock_client(vec![api_status(404, "NotFound"), (201, to_json(&desired))]);
        let api: Api<Secret> = Api::namespaced(client, NS);
        assert_eq!(
            ensure_config_secret(&api, &desired, &owner())
                .await
                .unwrap(),
            SecretOutcome::Created
        );
        drop(api);
        let requests = recorder.await.unwrap();
        assert_eq!(requests[1].method, "POST");
        assert!(
            requests[1]
                .uri
                .contains(&format!("fieldManager={FIELD_MANAGER}"))
        );
    }

    #[tokio::test]
    async fn secret_race_409_accepts_identical_content() {
        let desired = secret(b"bearer-one", vec![owner()]);
        let (client, recorder) = mock_client(vec![
            api_status(404, "NotFound"),
            api_status(409, "AlreadyExists"),
            (200, to_json(&desired)),
        ]);
        let api: Api<Secret> = Api::namespaced(client, NS);
        assert_eq!(
            ensure_config_secret(&api, &desired, &owner())
                .await
                .unwrap(),
            SecretOutcome::Existing
        );
        drop(api);
        let methods: Vec<_> = recorder
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.method)
            .collect();
        assert_eq!(methods, ["GET", "POST", "GET"], "never patched or replaced");
    }

    #[tokio::test]
    async fn secret_race_409_rejects_different_content_and_foreign_owner() {
        let desired = secret(b"bearer-one", vec![owner()]);
        let different = secret(b"another-bearer", vec![owner()]);
        let (client, recorder) = mock_client(vec![
            api_status(404, "NotFound"),
            api_status(409, "AlreadyExists"),
            (200, to_json(&different)),
        ]);
        let api: Api<Secret> = Api::namespaced(client, NS);
        let error = ensure_config_secret(&api, &desired, &owner())
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::SecretConflict { .. }));
        assert!(!error.to_string().contains("another-bearer"));
        drop(api);
        recorder.await.unwrap();

        let theirs = secret(b"bearer-one", vec![foreign_owner()]);
        let (client, recorder) = mock_client(vec![(200, to_json(&theirs))]);
        let api: Api<Secret> = Api::namespaced(client, NS);
        assert!(matches!(
            ensure_config_secret(&api, &desired, &owner()).await,
            Err(ExecError::ForeignObject { .. })
        ));
        drop(api);
        recorder.await.unwrap();
    }

    #[test]
    fn existing_secret_with_other_token_id_is_a_conflict() {
        let desired = secret(b"bearer-one", vec![owner()]);
        let mut other = desired.clone();
        other
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(ANNOTATION_TOKEN_ID.into(), "someone-else".into());
        assert!(matches!(
            verify_existing_secret(&other, &desired, &owner()),
            Err(ExecError::SecretConflict { .. })
        ));
        let mut mutable = desired.clone();
        mutable.immutable = None;
        assert!(verify_existing_secret(&mutable, &desired, &owner()).is_err());
    }

    #[tokio::test]
    async fn delete_binds_uid_and_resource_version() {
        let existing = deployment(Some(vec![owner()]));
        let (client, recorder) = mock_client(vec![
            (200, to_json(&existing)),
            (
                200,
                json!({"kind": "Status", "apiVersion": "v1", "status": "Success"}),
            ),
        ]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        let outcome = delete_owned(&api, NS, "campus", &owner(), DeleteMode::Foreground)
            .await
            .unwrap();
        assert_eq!(outcome, DeleteOutcome::Deleted);
        drop(api);
        let requests = recorder.await.unwrap();
        assert_eq!(requests[1].method, "DELETE");
        let body = requests[1].json();
        assert_eq!(body["preconditions"]["uid"], "dep-uid");
        assert_eq!(body["preconditions"]["resourceVersion"], "42");
        assert_eq!(body["propagationPolicy"], "Foreground");
    }

    #[tokio::test]
    async fn delete_absent_is_success_and_foreign_is_untouched() {
        let (client, recorder) = mock_client(vec![api_status(404, "NotFound")]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        assert_eq!(
            delete_owned(&api, NS, "campus", &owner(), DeleteMode::Default)
                .await
                .unwrap(),
            DeleteOutcome::AlreadyAbsent
        );
        drop(api);
        recorder.await.unwrap();

        let theirs = deployment(Some(vec![foreign_owner()]));
        let (client, recorder) = mock_client(vec![(200, to_json(&theirs))]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        assert!(matches!(
            delete_owned(&api, NS, "campus", &owner(), DeleteMode::Default).await,
            Err(ExecError::ForeignObject { .. })
        ));
        drop(api);
        assert_eq!(recorder.await.unwrap().len(), 1, "no DELETE issued");
    }

    #[tokio::test]
    async fn delete_precondition_failure_is_concurrent_mutation() {
        let existing = deployment(Some(vec![owner()]));
        let (client, recorder) =
            mock_client(vec![(200, to_json(&existing)), api_status(409, "Conflict")]);
        let api: Api<Deployment> = Api::namespaced(client, NS);
        let error = delete_owned(&api, NS, "campus", &owner(), DeleteMode::Default)
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::ConcurrentMutation { .. }));
        assert!(error.is_retryable());
        drop(api);
        recorder.await.unwrap();
    }
}
