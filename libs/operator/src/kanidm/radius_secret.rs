//! Immutable, owner-verified RADIUS configuration Secrets.
//!
//! All bearer and NAS credentials are confined to Secret.data, never resource
//! annotations, labels, status or logs. A configuration generation is
//! write-once: an existing name with different contents is a conflict rather
//! than an overwrite of pods still using that generation.

use std::collections::BTreeMap;

use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::PostParams;
use kube::{Api, Client};
use thiserror::Error;

use super::radius_config::{RadiusConfig, RadiusConfigError};

const SECRET_DATA_CONFIG: &str = "radius.toml";
const SECRET_DATA_TOKEN: &str = "auth_token";
const UID_LABEL: &str = "kaniop.rs/radius-uid";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusSecretError {
    #[error("RADIUS generated Secret must have a valid DNS label name and namespace")]
    InvalidName,
    #[error("RADIUS generated Secret is owned by another resource")]
    OwnershipConflict,
    #[error("RADIUS generated Secret is not an identical immutable generation")]
    GenerationConflict,
    #[error("RADIUS generated Secret Kubernetes operation failed")]
    KubernetesUnavailable,
    #[error(transparent)]
    InvalidConfiguration(#[from] RadiusConfigError),
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && name
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .last()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Build a single immutable Secret for one UID-bound RADIUS generation.
pub fn build_config_secret(
    name: &str,
    namespace: &str,
    owner: &OwnerReference,
    config: &RadiusConfig,
) -> Result<Secret, RadiusSecretError> {
    if !valid_name(name) || namespace.is_empty() || owner.uid.is_empty() {
        return Err(RadiusSecretError::InvalidName);
    }
    let toml = config.render_toml()?;
    Ok(Secret {
        metadata: ObjectMeta {
            name: Some(name.into()),
            namespace: Some(namespace.into()),
            labels: Some(BTreeMap::from([(UID_LABEL.into(), owner.uid.clone())])),
            owner_references: Some(vec![owner.clone()]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([
            (SECRET_DATA_CONFIG.into(), ByteString(toml.into_bytes())),
            (
                SECRET_DATA_TOKEN.into(),
                ByteString(config.auth_token.as_bytes().to_vec()),
            ),
        ])),
        immutable: Some(true),
        type_: Some("Opaque".into()),
        ..Default::default()
    })
}

/// Return an error instead of adopting someone else's Secret or modifying a
/// prior generation whose consumers may still be running.
pub fn verify_existing_generation(
    existing: &Secret,
    desired: &Secret,
    owner: &OwnerReference,
) -> Result<(), RadiusSecretError> {
    let owners = existing.metadata.owner_references.as_deref().unwrap_or(&[]);
    if owners.len() != 1
        || !owners.iter().any(|reference| {
            reference.uid == owner.uid
                && reference.kind == owner.kind
                && reference.name == owner.name
                && reference.controller == Some(true)
        })
    {
        return Err(RadiusSecretError::OwnershipConflict);
    }
    if existing.immutable != Some(true)
        || existing.type_.as_deref() != Some("Opaque")
        || existing.metadata.name != desired.metadata.name
        || existing.metadata.namespace != desired.metadata.namespace
        || existing.data != desired.data
    {
        return Err(RadiusSecretError::GenerationConflict);
    }
    Ok(())
}

/// Create-only, idempotent operation. A 409 from a racing controller must
/// result in re-reading and verifying ownership/content, never force-applying.
pub async fn ensure_config_secret(
    client: Client,
    desired: &Secret,
    owner: &OwnerReference,
) -> Result<(), RadiusSecretError> {
    let namespace = desired
        .metadata
        .namespace
        .as_deref()
        .ok_or(RadiusSecretError::InvalidName)?;
    let name = desired
        .metadata
        .name
        .as_deref()
        .ok_or(RadiusSecretError::InvalidName)?;
    let api: Api<Secret> = Api::namespaced(client, namespace);
    match api.get(name).await {
        Ok(existing) => verify_existing_generation(&existing, desired, owner),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            match api.create(&PostParams::default(), desired).await {
                Ok(_) => Ok(()),
                Err(kube::Error::Api(e)) if e.code == 409 => {
                    let existing = api
                        .get(name)
                        .await
                        .map_err(|_| RadiusSecretError::KubernetesUnavailable)?;
                    verify_existing_generation(&existing, desired, owner)
                }
                Err(_) => Err(RadiusSecretError::KubernetesUnavailable),
            }
        }
        Err(_) => Err(RadiusSecretError::KubernetesUnavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::super::radius_config::{RadiusClient, RadiusConfig};
    use super::*;

    fn fixture() -> (OwnerReference, RadiusConfig) {
        let owner = OwnerReference {
            api_version: "kaniop.rs/v1beta1".into(),
            kind: "KanidmRadiusServer".into(),
            name: "campus".into(),
            uid: "d10066c3-4599-4487-972d-26bb42587a32".into(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        };
        let config = RadiusConfig {
            uri: "https://idm.example.com".into(),
            auth_token: "bearer-do-not-log".into(),
            ca_path: None,
            radius_required_groups: vec!["allowed@idm.example.com".into()],
            radius_default_vlan: 10,
            radius_groups: vec![],
            radius_clients: vec![RadiusClient {
                name: "nas".into(),
                ipaddr: "192.0.2.10".into(),
                secret: "safeRandomSecret123".into(),
            }],
            radius_ca_path: None,
        };
        (owner, config)
    }

    #[test]
    fn credentials_are_only_in_immutable_secret_data() {
        let (owner, config) = fixture();
        let secret = build_config_secret("campus-v1", "default", &owner, &config).unwrap();
        assert_eq!(secret.immutable, Some(true));
        assert_eq!(secret.type_.as_deref(), Some("Opaque"));
        assert!(secret.data.as_ref().unwrap().contains_key("radius.toml"));
        assert!(secret.data.as_ref().unwrap().contains_key("auth_token"));
        let metadata = serde_json::to_string(&secret.metadata).unwrap();
        assert!(!metadata.contains("bearer-do-not-log"));
        assert!(!metadata.contains("safeRandomSecret123"));
        assert_eq!(verify_existing_generation(&secret, &secret, &owner), Ok(()));
    }

    #[test]
    fn refuse_to_adopt_foreign_or_modified_generation() {
        let (owner, config) = fixture();
        let desired = build_config_secret("campus-v1", "default", &owner, &config).unwrap();
        let mut foreign = desired.clone();
        foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "foreign".into();
        assert_eq!(
            verify_existing_generation(&foreign, &desired, &owner),
            Err(RadiusSecretError::OwnershipConflict)
        );
        let mut mutated = desired.clone();
        mutated
            .data
            .as_mut()
            .unwrap()
            .insert("auth_token".into(), ByteString(b"bad-other-token".to_vec()));
        assert_eq!(
            verify_existing_generation(&mutated, &desired, &owner),
            Err(RadiusSecretError::GenerationConflict)
        );
    }

    #[test]
    fn reject_ambiguous_secret_generation_names() {
        let (owner, config) = fixture();
        assert_eq!(
            build_config_secret("wrong_name", "default", &owner, &config).unwrap_err(),
            RadiusSecretError::InvalidName
        );
    }
}
