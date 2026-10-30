//! Owned workload teardown for Kanidm RADIUS finalizers.
//!
//! A Kubernetes DELETE is asynchronous. Token and service-account revocation
//! MUST NOT run until every old RADIUS pod has disappeared. Both Deployment
//! and Service ownership must be validated before deletion; otherwise an
//! existing user-managed resource would be silently adopted or destroyed.

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Pod, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{DeleteParams, ListParams};
use kube::{Api, Client};
use thiserror::Error;

const UID_LABEL: &str = "kaniop.rs/radius-uid";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusCleanupError {
    #[error("RADIUS workload identity is missing")]
    MissingIdentity,
    #[error("Kubernetes resource exists with a different owner UID")]
    OwnershipConflict,
    #[error("RADIUS Kubernetes cleanup is unavailable")]
    KubernetesUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainState {
    WaitingForPods,
    Drained,
}

pub fn verify_owner(
    metadata: &ObjectMeta,
    owner: &OwnerReference,
) -> Result<(), RadiusCleanupError> {
    let references = metadata.owner_references.as_deref().unwrap_or(&[]);
    if references.len() == 1
        && references.iter().any(|reference| {
            reference.uid == owner.uid
                && reference.kind == owner.kind
                && reference.name == owner.name
                && reference.controller == Some(true)
        })
    {
        Ok(())
    } else {
        Err(RadiusCleanupError::OwnershipConflict)
    }
}

/// Verify and delete only generated objects owned by this exact CR UID.
/// Reports Drained only when *no* matching pods remain. This function does
/// not delete external Secrets, ConfigMaps, Kanidm users or tokens.
pub async fn delete_owned_radius_workload(
    client: Client,
    namespace: &str,
    name: &str,
    owner: &OwnerReference,
) -> Result<DrainState, RadiusCleanupError> {
    if namespace.is_empty() || name.is_empty() || owner.uid.is_empty() {
        return Err(RadiusCleanupError::MissingIdentity);
    }
    let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let service_api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let pod_api: Api<Pod> = Api::namespaced(client, namespace);

    // Verify BOTH resources before deleting EITHER one, preventing partial
    // deletion if one of the names collides with a non-owned resource.
    let deployment = match deployment_api.get(name).await {
        Ok(obj) => Some(obj),
        Err(kube::Error::Api(err)) if err.code == 404 => None,
        Err(_) => return Err(RadiusCleanupError::KubernetesUnavailable),
    };
    let service = match service_api.get(name).await {
        Ok(obj) => Some(obj),
        Err(kube::Error::Api(err)) if err.code == 404 => None,
        Err(_) => return Err(RadiusCleanupError::KubernetesUnavailable),
    };
    if let Some(obj) = &deployment {
        verify_owner(&obj.metadata, owner)?;
    }
    if let Some(obj) = &service {
        verify_owner(&obj.metadata, owner)?;
    }

    let params = DeleteParams::default();
    if deployment.is_some() {
        match deployment_api.delete(name, &params).await {
            Ok(_) => {}
            Err(kube::Error::Api(err)) if err.code == 404 => {}
            Err(_) => return Err(RadiusCleanupError::KubernetesUnavailable),
        }
    }
    if service.is_some() {
        match service_api.delete(name, &params).await {
            Ok(_) => {}
            Err(kube::Error::Api(err)) if err.code == 404 => {}
            Err(_) => return Err(RadiusCleanupError::KubernetesUnavailable),
        }
    }

    let selector = format!("{UID_LABEL}={}", owner.uid);
    let pods = pod_api
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(|_| RadiusCleanupError::KubernetesUnavailable)?;
    if pods.items.is_empty() {
        Ok(DrainState::Drained)
    } else {
        Ok(DrainState::WaitingForPods)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(uid: &str) -> OwnerReference {
        OwnerReference {
            api_version: "kaniop.rs/v1beta1".into(),
            kind: "KanidmRadiusServer".into(),
            name: "campus".into(),
            uid: uid.into(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }

    #[test]
    fn refuses_foreign_or_ownerless_objects() {
        let desired = owner("owner-uid");
        let mut metadata = ObjectMeta::default();
        assert_eq!(
            verify_owner(&metadata, &desired),
            Err(RadiusCleanupError::OwnershipConflict)
        );
        metadata.owner_references = Some(vec![owner("another-uid")]);
        assert_eq!(
            verify_owner(&metadata, &desired),
            Err(RadiusCleanupError::OwnershipConflict)
        );
        metadata.owner_references = Some(vec![desired.clone()]);
        assert_eq!(verify_owner(&metadata, &desired), Ok(()));
        metadata.owner_references.as_mut().unwrap()[0].controller = Some(false);
        assert_eq!(
            verify_owner(&metadata, &desired),
            Err(RadiusCleanupError::OwnershipConflict)
        );
    }
}
