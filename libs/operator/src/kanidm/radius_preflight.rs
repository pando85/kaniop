//! Fail-closed preflight for referenced RADIUS inputs.
//!
//! This module performs read-only Kubernetes lookups. In particular, call it
//! BEFORE creating external Kanidm identities/tokens or modifying an existing
//! Deployment. Validation errors must not include sensitive Secret values.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::{Api, Client, ResourceExt};
use openssl::x509::X509;
use thiserror::Error;

use super::radius_config::{
    KANIDM_CA_PATH, RADIUS_CA_PATH, RadiusClient, RadiusConfig, RadiusConfigError, RadiusGroup,
};
use super::radius_crd::{KanidmRadiusServer, KanidmRadiusServerSpec, RadiusCaKind};
use super::radius_tls::{RadiusTlsError, validate_eap_tls_material};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusPreflightError {
    #[error("RADIUS resource has no namespace")]
    MissingNamespace,
    #[error("RADIUS CR has invalid replicas, service, rotation, or Secret references")]
    InvalidSpec,
    #[error("cannot read a referenced RADIUS NAS Secret")]
    NasSecretUnavailable,
    #[error("RADIUS NAS Secret key is absent or not valid UTF-8")]
    NasSecretKeyUnavailable,
    #[error("cannot read a referenced EAP TLS Secret")]
    TlsSecretUnavailable,
    #[error("RADIUS EAP TLS Secret is missing required key material")]
    TlsKeyUnavailable,
    #[error("cannot read the referenced Kanidm CA Secret or ConfigMap")]
    KanidmCaUnavailable,
    #[error("Kanidm CA input contains no valid certificates")]
    InvalidKanidmCa,
    #[error(transparent)]
    Config(#[from] RadiusConfigError),
    #[error(transparent)]
    Tls(#[from] RadiusTlsError),
}

/// Secret-bearing preflight result. Intentionally no Debug or Serialize.
/// Supply the minted or existing READ-ONLY Kanidm bearer only after this
/// preflight passes, and store the resulting TOML exclusively in a Secret.
pub struct PreparedRadiusInputs {
    uri: String,
    clients: Vec<RadiusClient>,
    required_groups: Vec<String>,
    default_vlan: u16,
    group_vlans: Vec<RadiusGroup>,
    has_kanidm_ca: bool,
    has_eap_ca: bool,
}

impl PreparedRadiusInputs {
    /// This is the only path from Secret-backed inputs to a runtime config.
    /// The bearer token is never exposed via Debug, status or annotations.
    pub fn config_with_token(&self, token: String) -> Result<RadiusConfig, RadiusConfigError> {
        let config = RadiusConfig {
            uri: self.uri.clone(),
            auth_token: token,
            ca_path: self.has_kanidm_ca.then(|| KANIDM_CA_PATH.into()),
            radius_required_groups: self.required_groups.clone(),
            radius_default_vlan: self.default_vlan,
            radius_groups: self.group_vlans.clone(),
            radius_clients: self.clients.clone(),
            radius_ca_path: self.has_eap_ca.then(|| RADIUS_CA_PATH.into()),
        };
        config.validate()?;
        Ok(config)
    }
}

/// Validate the non-secret API fields before reading potentially sensitive
/// Kubernetes Secrets. CEL is defense-in-depth, never the only validation.
fn validate_spec(spec: &KanidmRadiusServerSpec) -> Result<(), RadiusPreflightError> {
    if !(1..=100).contains(&spec.replicas)
        || !(1..=65535).contains(&spec.service.port)
        || !(1..=365).contains(&spec.token_rotation.period_days)
        || !matches!(spec.service.type_.as_str(), "ClusterIP" | "NodePort" | "LoadBalancer")
        || !matches!(spec.service.external_traffic_policy.as_deref(), None | Some("Cluster") | Some("Local"))
        || (spec.service.type_ == "ClusterIP" && spec.service.external_traffic_policy.is_some())
        || spec.radius_tls.secret_ref.name.is_empty()
        || spec.radius_tls.cert_key.is_empty()
        || spec.radius_tls.private_key_key.is_empty()
        || spec.clients.is_empty()
        || spec.clients.len() > 100
        || spec.authorization.required_groups.is_empty()
        || spec.authorization.required_groups.len() > 100
        || spec.clients.iter().any(|c| c.shared_secret_ref.name.is_empty() || c.shared_secret_ref.key.is_empty())
    {
        return Err(RadiusPreflightError::InvalidSpec);
    }
    Ok(())
}

fn data_key<'a>(secret: &'a Secret, key: &str) -> Result<&'a [u8], RadiusPreflightError> {
    secret
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .map(|data| data.0.as_slice())
        .ok_or(RadiusPreflightError::TlsKeyUnavailable)
}

async fn resolve_nas_clients(
    api: &Api<Secret>,
    spec: &KanidmRadiusServerSpec,
) -> Result<Vec<RadiusClient>, RadiusPreflightError> {
    // Avoid repeated API calls for clients sharing one Secret.
    let mut cache = BTreeMap::<String, Secret>::new();
    let mut clients = Vec::with_capacity(spec.clients.len());
    for client in &spec.clients {
        let secret_ref = &client.shared_secret_ref;
        if !cache.contains_key(&secret_ref.name) {
            let secret = api
                .get(&secret_ref.name)
                .await
                .map_err(|_| RadiusPreflightError::NasSecretUnavailable)?;
            cache.insert(secret_ref.name.clone(), secret);
        }
        let secret = cache
            .get(&secret_ref.name)
            .ok_or(RadiusPreflightError::NasSecretUnavailable)?;
        let value = secret
            .data
            .as_ref()
            .and_then(|map| map.get(&secret_ref.key))
            .ok_or(RadiusPreflightError::NasSecretKeyUnavailable)?;
        let value = String::from_utf8(value.0.clone())
            .map_err(|_| RadiusPreflightError::NasSecretKeyUnavailable)?;
        clients.push(RadiusClient {
            name: client.name.clone(),
            ipaddr: client.ipaddr.clone(),
            secret: value,
        });
    }
    Ok(clients)
}

/// Validate every referenced input without changing either Kanidm or
/// Kubernetes. A failed preflight must leave the live generation untouched.
pub async fn prepare_radius_inputs(
    client: Client,
    radius: &KanidmRadiusServer,
    kanidm_uri: &str,
) -> Result<PreparedRadiusInputs, RadiusPreflightError> {
    validate_spec(&radius.spec)?;
    let namespace = radius.namespace().ok_or(RadiusPreflightError::MissingNamespace)?;
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace);

    let clients = resolve_nas_clients(&secrets, &radius.spec).await?;
    let tls = &radius.spec.radius_tls;
    let eap = secrets
        .get(&tls.secret_ref.name)
        .await
        .map_err(|_| RadiusPreflightError::TlsSecretUnavailable)?;
    let cert = data_key(&eap, &tls.cert_key)?;
    let key = data_key(&eap, &tls.private_key_key)?;
    let ca = tls
        .ca_key
        .as_deref()
        .map(|name| data_key(&eap, name))
        .transpose()?;
    validate_eap_tls_material(cert, key, ca)?;

    let has_kanidm_ca = if let Some(ca_ref) = radius
        .spec
        .kanidm_tls
        .as_ref()
        .and_then(|tls| tls.ca_bundle_ref.as_ref())
    {
        if ca_ref.name.is_empty() || ca_ref.key.is_empty() {
            return Err(RadiusPreflightError::InvalidSpec);
        }
        let pem = match ca_ref.kind {
            RadiusCaKind::ConfigMap => {
                let api: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace);
                let config_map = api
                    .get(&ca_ref.name)
                    .await
                    .map_err(|_| RadiusPreflightError::KanidmCaUnavailable)?;
                config_map
                    .data
                    .as_ref()
                    .and_then(|data| data.get(&ca_ref.key))
                    .map(|value| value.as_bytes().to_vec())
                    .ok_or(RadiusPreflightError::InvalidKanidmCa)?
            }
            RadiusCaKind::Secret => {
                let secret = secrets
                    .get(&ca_ref.name)
                    .await
                    .map_err(|_| RadiusPreflightError::KanidmCaUnavailable)?;
                secret
                    .data
                    .as_ref()
                    .and_then(|data| data.get(&ca_ref.key))
                    .map(|value| value.0.clone())
                    .ok_or(RadiusPreflightError::InvalidKanidmCa)?
            }
        };
        let certificates =
            X509::stack_from_pem(&pem).map_err(|_| RadiusPreflightError::InvalidKanidmCa)?;
        if certificates.is_empty() {
            return Err(RadiusPreflightError::InvalidKanidmCa);
        }
        true
    } else {
        false
    };

    let default_vlan = u16::try_from(radius.spec.authorization.default_vlan)
        .map_err(|_| RadiusPreflightError::Config(RadiusConfigError::InvalidVlan))?;
    let group_vlans: Vec<RadiusGroup> = radius
        .spec
        .authorization
        .group_vlans
        .iter()
        .map(|group| {
            u16::try_from(group.vlan)
                .map(|vlan| RadiusGroup {
                    spn: group.group.clone(),
                    vlan,
                })
                .map_err(|_| RadiusPreflightError::Config(RadiusConfigError::InvalidVlan))
        })
        .collect::<Result<_, _>>()?;

    let prepared = PreparedRadiusInputs {
        uri: kanidm_uri.into(),
        clients,
        required_groups: radius.spec.authorization.required_groups.clone(),
        default_vlan,
        group_vlans,
        has_kanidm_ca,
        has_eap_ca: ca.is_some(),
    };
    // Placeholder is intentionally local to the preflight. It is NEVER
    // returned or rendered as a live configuration.
    prepared.config_with_token("preflight-only".into())?;
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::radius_crd::KanidmRadiusServerSpec;

    fn spec() -> KanidmRadiusServerSpec {
        serde_json::from_value(serde_json::json!({
            "kanidmRef": {"name": "idm"},
            "authorization": {"requiredGroups": ["radius_access_allowed@idm.example.com"]},
            "clients": [
                {"name": "nas1", "ipaddr": "192.0.2.1", "sharedSecretRef": {"name": "nas-secret", "key": "nas1"}}
            ],
            "radiusTls": {"secretRef": {"name": "eap-tls"}}
        })).unwrap()
    }

    #[test]
    fn rejects_invalid_spec_before_reading_any_secrets() {
        let mut spec = spec();
        assert_eq!(validate_spec(&spec), Ok(()));
        spec.radius_tls.secret_ref.name.clear();
        assert_eq!(validate_spec(&spec), Err(RadiusPreflightError::InvalidSpec));
        spec.radius_tls.secret_ref.name = "eap-tls".into();
        spec.service.type_ = "LoadBalancer".into();
        spec.service.external_traffic_policy = Some("Local".into());
        assert_eq!(validate_spec(&spec), Ok(()));
        spec.service.type_ = "ClusterIP".into();
        assert_eq!(validate_spec(&spec), Err(RadiusPreflightError::InvalidSpec));
    }

    #[test]
    fn missing_tls_key_is_not_reported_with_data() {
        let secret = Secret::default();
        assert_eq!(data_key(&secret, "tls.key"), Err(RadiusPreflightError::TlsKeyUnavailable));
    }
}
