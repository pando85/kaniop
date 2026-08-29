use k8s_openapi::api::core::v1::{
    ConfigMapVolumeSource, EnvVar, EnvVarSource, KeyToPath, ProjectedVolumeSource,
    SecretKeySelector, ServiceAccountTokenProjection, Volume, VolumeMount, VolumeProjection,
};

use crate::crd::{AuthMethod, SecretRef};

pub const SECRET_KEY_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";
pub const SECRET_KEY_SECRET_ACCESS_KEY: &str = "AWS_SECRET_ACCESS_KEY";
pub const SECRET_KEY_SESSION_TOKEN: &str = "AWS_SESSION_TOKEN";

pub const ENCRYPTION_KEY_ENV: &str = "KANIOP_ENCRYPTION_KEY";
pub const ENCRYPTION_KEY_SECRET_ENTRY: &str = "encryption-key";

pub const CA_BUNDLE_VOLUME_NAME: &str = "ca-bundle";
pub const CA_BUNDLE_MOUNT_PATH: &str = "/etc/ssl/certs/ca-certificates.crt";
pub const CA_BUNDLE_FILE_NAME: &str = "ca-bundle.pem";

pub const PROJECTED_TOKEN_VOLUME_NAME: &str = "projected-token";
pub const PROJECTED_TOKEN_MOUNT_PATH: &str = "/var/run/secrets/projected-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRole {
    Writer,
    Reader,
    Deleter,
}

impl AuthRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthRole::Writer => "writer",
            AuthRole::Reader => "reader",
            AuthRole::Deleter => "deleter",
        }
    }
}

pub fn validate_auth_method(method: &AuthMethod, role: &str) -> Result<(), String> {
    match (&method.workload_identity, &method.secret_ref) {
        (None, None) => Err(format!(
            "{role} authentication: either workloadIdentity or secretRef must be set"
        )),
        (Some(_), Some(_)) => Err(format!(
            "{role} authentication: workloadIdentity and secretRef are mutually exclusive"
        )),
        _ => Ok(()),
    }
}

pub fn build_auth_env_vars(method: &AuthMethod, repo_name: &str, role: AuthRole) -> Vec<EnvVar> {
    if let Some(wi) = &method.workload_identity {
        if wi.audience.is_some() {
            return vec![EnvVar {
                name: "AWS_WEB_IDENTITY_TOKEN_FILE".to_string(),
                value: Some(format!("{PROJECTED_TOKEN_MOUNT_PATH}/token")),
                ..Default::default()
            }];
        }
        return vec![];
    }

    let secret_name = method
        .secret_ref
        .as_ref()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| format!("{repo_name}-{}", role.as_str()));

    build_secret_ref_env_vars(&secret_name)
}

pub fn build_secret_ref_env_vars(secret_name: &str) -> Vec<EnvVar> {
    vec![
        EnvVar {
            name: "AWS_ACCESS_KEY_ID".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: secret_name.to_string(),
                    key: SECRET_KEY_ACCESS_KEY_ID.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        EnvVar {
            name: "AWS_SECRET_ACCESS_KEY".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: secret_name.to_string(),
                    key: SECRET_KEY_SECRET_ACCESS_KEY.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        EnvVar {
            name: "AWS_SESSION_TOKEN".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: secret_name.to_string(),
                    key: SECRET_KEY_SESSION_TOKEN.to_string(),
                    optional: Some(true),
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    ]
}

pub fn build_auth_volumes(method: &AuthMethod) -> Vec<Volume> {
    if let Some(wi) = &method.workload_identity {
        if let Some(audience) = &wi.audience {
            return vec![build_projected_token_volume(audience)];
        }
    }
    vec![]
}

pub fn build_auth_volume_mounts(method: &AuthMethod) -> Vec<VolumeMount> {
    if let Some(wi) = &method.workload_identity {
        if wi.audience.is_some() {
            return vec![VolumeMount {
                name: PROJECTED_TOKEN_VOLUME_NAME.to_string(),
                mount_path: PROJECTED_TOKEN_MOUNT_PATH.to_string(),
                read_only: Some(true),
                ..Default::default()
            }];
        }
    }
    vec![]
}

fn build_projected_token_volume(audience: &str) -> Volume {
    Volume {
        name: PROJECTED_TOKEN_VOLUME_NAME.to_string(),
        projected: Some(ProjectedVolumeSource {
            sources: Some(vec![VolumeProjection {
                service_account_token: Some(ServiceAccountTokenProjection {
                    audience: Some(audience.to_string()),
                    path: "token".to_string(),
                    expiration_seconds: Some(3600),
                }),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub fn build_ca_bundle_volume(ca_bundle_ref: &str) -> Volume {
    Volume {
        name: CA_BUNDLE_VOLUME_NAME.to_string(),
        config_map: Some(ConfigMapVolumeSource {
            name: ca_bundle_ref.to_string(),
            items: Some(vec![KeyToPath {
                key: CA_BUNDLE_FILE_NAME.to_string(),
                path: CA_BUNDLE_FILE_NAME.to_string(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub fn build_ca_bundle_volume_mount() -> VolumeMount {
    VolumeMount {
        name: CA_BUNDLE_VOLUME_NAME.to_string(),
        mount_path: CA_BUNDLE_MOUNT_PATH.to_string(),
        sub_path: Some(CA_BUNDLE_FILE_NAME.to_string()),
        read_only: Some(true),
        ..Default::default()
    }
}

pub fn ca_bundle_path() -> String {
    CA_BUNDLE_MOUNT_PATH.to_string()
}

pub fn ca_bundle_env_var() -> EnvVar {
    EnvVar {
        name: "SSL_CERT_FILE".to_string(),
        value: Some(ca_bundle_path()),
        ..Default::default()
    }
}

pub fn build_encryption_env_vars(key_ref: Option<&SecretRef>) -> Vec<EnvVar> {
    let secret_ref = match key_ref {
        Some(r) => r,
        None => return vec![],
    };
    vec![EnvVar {
        name: ENCRYPTION_KEY_ENV.to_string(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: secret_ref.name.clone(),
                key: ENCRYPTION_KEY_SECRET_ENTRY.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{SecretRef, WorkloadIdentity};

    fn secret_ref_method(name: &str) -> AuthMethod {
        AuthMethod {
            workload_identity: None,
            secret_ref: Some(SecretRef {
                name: name.to_string(),
            }),
        }
    }

    fn workload_identity_method() -> AuthMethod {
        AuthMethod {
            workload_identity: Some(WorkloadIdentity { audience: None }),
            secret_ref: None,
        }
    }

    fn workload_identity_with_audience_method(audience: &str) -> AuthMethod {
        AuthMethod {
            workload_identity: Some(WorkloadIdentity {
                audience: Some(audience.to_string()),
            }),
            secret_ref: None,
        }
    }

    fn empty_method() -> AuthMethod {
        AuthMethod {
            workload_identity: None,
            secret_ref: None,
        }
    }

    fn both_method() -> AuthMethod {
        AuthMethod {
            workload_identity: Some(WorkloadIdentity { audience: None }),
            secret_ref: Some(SecretRef {
                name: "secret".to_string(),
            }),
        }
    }

    #[test]
    fn validate_accepts_secret_ref_only() {
        assert!(validate_auth_method(&secret_ref_method("s"), "writer").is_ok());
    }

    #[test]
    fn validate_accepts_workload_identity_only() {
        assert!(validate_auth_method(&workload_identity_method(), "reader").is_ok());
    }

    #[test]
    fn validate_rejects_neither_set() {
        let result = validate_auth_method(&empty_method(), "deleter");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("deleter"));
    }

    #[test]
    fn validate_rejects_both_set() {
        let result = validate_auth_method(&both_method(), "writer");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("mutually exclusive"));
    }

    #[test]
    fn secret_ref_env_vars_use_canonical_keys() {
        let envs = build_secret_ref_env_vars("my-secret");
        assert_eq!(envs.len(), 3);
        assert_eq!(envs[0].name, "AWS_ACCESS_KEY_ID");
        assert_eq!(envs[1].name, "AWS_SECRET_ACCESS_KEY");
        assert_eq!(envs[2].name, "AWS_SESSION_TOKEN");

        let key_ref = envs[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.name, "my-secret");
        assert_eq!(key_ref.key, SECRET_KEY_ACCESS_KEY_ID);

        let key_ref = envs[1]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.key, SECRET_KEY_SECRET_ACCESS_KEY);

        let key_ref = envs[2]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.key, SECRET_KEY_SESSION_TOKEN);
        assert_eq!(key_ref.optional, Some(true));
    }

    #[test]
    fn build_auth_env_vars_secret_ref_uses_explicit_name() {
        let method = secret_ref_method("explicit-secret");
        let envs = build_auth_env_vars(&method, "repo", AuthRole::Writer);
        assert_eq!(envs.len(), 3);
        let key_ref = envs[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.name, "explicit-secret");
    }

    #[test]
    fn build_auth_env_vars_workload_identity_no_audience_returns_empty() {
        let method = workload_identity_method();
        let envs = build_auth_env_vars(&method, "repo", AuthRole::Reader);
        assert!(envs.is_empty());
    }

    #[test]
    fn build_auth_env_vars_workload_identity_with_audience_returns_token_file() {
        let method = workload_identity_with_audience_method("sts.amazonaws.com");
        let envs = build_auth_env_vars(&method, "repo", AuthRole::Writer);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "AWS_WEB_IDENTITY_TOKEN_FILE");
        assert_eq!(
            envs[0].value,
            Some(format!("{PROJECTED_TOKEN_MOUNT_PATH}/token"))
        );
    }

    #[test]
    fn build_auth_volumes_empty_for_secret_ref() {
        let method = secret_ref_method("s");
        assert!(build_auth_volumes(&method).is_empty());
    }

    #[test]
    fn build_auth_volumes_empty_for_workload_identity_without_audience() {
        let method = workload_identity_method();
        assert!(build_auth_volumes(&method).is_empty());
    }

    #[test]
    fn build_auth_volumes_projected_token_for_audience() {
        let method = workload_identity_with_audience_method("sts.amazonaws.com");
        let volumes = build_auth_volumes(&method);
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].name, PROJECTED_TOKEN_VOLUME_NAME);
        let projected = volumes[0].projected.as_ref().unwrap();
        let sources = projected.sources.as_ref().unwrap();
        let token = sources[0].service_account_token.as_ref().unwrap();
        assert_eq!(token.audience, Some("sts.amazonaws.com".to_string()));
        assert_eq!(token.path, "token");
    }

    #[test]
    fn build_auth_volume_mounts_empty_without_audience() {
        let method = secret_ref_method("s");
        assert!(build_auth_volume_mounts(&method).is_empty());
        let method = workload_identity_method();
        assert!(build_auth_volume_mounts(&method).is_empty());
    }

    #[test]
    fn build_auth_volume_mounts_projected_token_with_audience() {
        let method = workload_identity_with_audience_method("sts.amazonaws.com");
        let mounts = build_auth_volume_mounts(&method);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name, PROJECTED_TOKEN_VOLUME_NAME);
        assert_eq!(mounts[0].mount_path, PROJECTED_TOKEN_MOUNT_PATH);
        assert_eq!(mounts[0].read_only, Some(true));
    }

    #[test]
    fn ca_bundle_volume_uses_config_map_with_explicit_key() {
        let volume = build_ca_bundle_volume("my-ca-cm");
        assert_eq!(volume.name, CA_BUNDLE_VOLUME_NAME);
        let cm = volume.config_map.as_ref().unwrap();
        assert_eq!(cm.name, "my-ca-cm");
        let items = cm.items.as_ref().unwrap();
        assert_eq!(items[0].key, CA_BUNDLE_FILE_NAME);
        assert_eq!(items[0].path, CA_BUNDLE_FILE_NAME);
    }

    #[test]
    fn ca_bundle_volume_mount_is_read_only() {
        let mount = build_ca_bundle_volume_mount();
        assert_eq!(mount.name, CA_BUNDLE_VOLUME_NAME);
        assert_eq!(mount.mount_path, CA_BUNDLE_MOUNT_PATH);
        assert_eq!(mount.read_only, Some(true));
    }

    #[test]
    fn ca_bundle_path_is_absolute() {
        let path = ca_bundle_path();
        assert!(path.starts_with('/'));
        assert_eq!(path, CA_BUNDLE_MOUNT_PATH);
    }

    #[test]
    fn auth_role_as_str() {
        assert_eq!(AuthRole::Writer.as_str(), "writer");
        assert_eq!(AuthRole::Reader.as_str(), "reader");
        assert_eq!(AuthRole::Deleter.as_str(), "deleter");
    }
}
