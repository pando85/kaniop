//! Ownership-safe Kubernetes apply for RADIUS workloads.
//!
//! This module may only be called after reference validation and Kanidm
//! identity/token preflight. It never adopts a same-name Deployment or
//! Service owned by another controller, even when the labels match.

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Service;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Patch, PatchParams, PostParams};
use kube::{Api, Client};
use thiserror::Error;

use super::radius_cleanup::verify_owner;
use super::radius_workload::{RadiusWorkload, RadiusWorkloadError};

const FIELD_MANAGER: &str = "kaniop-radius";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusApplyError {
    #[error("RADIUS workload validation failed")]
    InvalidWorkload,
    #[error("RADIUS workload conflicts with an unrelated Kubernetes resource")]
    OwnershipConflict,
    #[error("Kubernetes RADIUS workload request failed")]
    KubernetesUnavailable,
}

fn owner_from_workload(workload: &RadiusWorkload) -> &OwnerReference {
    &workload.owner
}

async fn ensure_owned_deployment(
    api: &Api<Deployment>,
    desired: &Deployment,
    owner: &OwnerReference,
    name: &str,
) -> Result<(), RadiusApplyError> {
    match api.get(name).await {
        Ok(existing) => {
            verify_owner(&existing.metadata, owner)
                .map_err(|_| RadiusApplyError::OwnershipConflict)?;
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            match api.create(&PostParams::default(), desired).await {
                Ok(_) => return Ok(()),
                Err(kube::Error::Api(err)) if err.code == 409 => {
                    let existing = api
                        .get(name)
                        .await
                        .map_err(|_| RadiusApplyError::KubernetesUnavailable)?;
                    verify_owner(&existing.metadata, owner)
                        .map_err(|_| RadiusApplyError::OwnershipConflict)?;
                }
                Err(_) => return Err(RadiusApplyError::KubernetesUnavailable),
            }
        }
        Err(_) => return Err(RadiusApplyError::KubernetesUnavailable),
    }
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER),
        &Patch::Apply(desired),
    )
    .await
    .map_err(|_| RadiusApplyError::KubernetesUnavailable)?;
    Ok(())
}

async fn ensure_owned_service(
    api: &Api<Service>,
    desired: &Service,
    owner: &OwnerReference,
    name: &str,
) -> Result<(), RadiusApplyError> {
    match api.get(name).await {
        Ok(existing) => {
            verify_owner(&existing.metadata, owner)
                .map_err(|_| RadiusApplyError::OwnershipConflict)?;
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            match api.create(&PostParams::default(), desired).await {
                Ok(_) => return Ok(()),
                Err(kube::Error::Api(err)) if err.code == 409 => {
                    let existing = api
                        .get(name)
                        .await
                        .map_err(|_| RadiusApplyError::KubernetesUnavailable)?;
                    verify_owner(&existing.metadata, owner)
                        .map_err(|_| RadiusApplyError::OwnershipConflict)?;
                }
                Err(_) => return Err(RadiusApplyError::KubernetesUnavailable),
            }
        }
        Err(_) => return Err(RadiusApplyError::KubernetesUnavailable),
    }
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER),
        &Patch::Apply(desired),
    )
    .await
    .map_err(|_| RadiusApplyError::KubernetesUnavailable)?;
    Ok(())
}

/// Apply both UID-owned resources only after their input configuration has
/// been validated and the immutable generation Secret is safely persisted.
/// The caller must not treat apply success as proof of authentication.
pub async fn ensure_radius_workload(
    client: Client,
    workload: &RadiusWorkload,
) -> Result<(), RadiusApplyError> {
    let deployment = workload
        .deployment()
        .map_err(|_: RadiusWorkloadError| RadiusApplyError::InvalidWorkload)?;
    let service = workload
        .service()
        .map_err(|_| RadiusApplyError::InvalidWorkload)?;

    let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), &workload.namespace);
    let service_api: Api<Service> = Api::namespaced(client, &workload.namespace);

    // Preflight collisions across BOTH names before applying EITHER object.
    // This avoids creating a Deployment if the Service name is foreign.
    if let Ok(existing) = deployment_api.get(&workload.name).await {
        verify_owner(&existing.metadata, owner_from_workload(workload))
            .map_err(|_| RadiusApplyError::OwnershipConflict)?;
    }
    if let Ok(existing) = service_api.get(&workload.name).await {
        verify_owner(&existing.metadata, owner_from_workload(workload))
            .map_err(|_| RadiusApplyError::OwnershipConflict)?;
    }

    ensure_owned_deployment(
        &deployment_api,
        &deployment,
        owner_from_workload(workload),
        &workload.name,
    )
    .await?;
    ensure_owned_service(
        &service_api,
        &service,
        owner_from_workload(workload),
        &workload.name,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_name_is_stable() {
        assert_eq!(FIELD_MANAGER, "kaniop-radius");
    }
}
