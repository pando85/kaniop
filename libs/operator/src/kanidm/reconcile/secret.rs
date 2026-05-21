use crate::kanidm::controller::context::Context;
use crate::kanidm::crd::Kanidm;
use crate::kanidm::reconcile::statefulset::{KANIDM_CONFIG_PATH, REPLICA_LABEL};

use kaniop_k8s_util::error::{Error, Result};

use std::sync::{Arc, LazyLock};

use k8s_openapi::api::core::v1::Secret;
use kube::ResourceExt;
use kube::api::{ObjectMeta, Resource};
use regex::Regex;
use serde::Serialize;
use tracing::info;

pub const ADMIN_PASSWORD_KEY: &str = "ADMIN_PASSWORD";
pub const ADMIN_USER: &str = "admin";
pub const IDM_ADMIN_PASSWORD_KEY: &str = "IDM_ADMIN_PASSWORD";
pub const IDM_ADMIN_USER: &str = "idm_admin";
pub const SECRET_TYPE_LABEL: &str = "kaniop.rs/secret-type";
// decode with `basenc --base64url -d | openssl x509 -noout -text -inform DER`
pub const REPLICA_SECRET_KEY: &str = "tls.der.b64url";

#[allow(async_fn_in_trait)]
pub trait SecretExt {
    fn admins_secret_name(&self) -> String;
    fn replica_secret_name(&self, pod_name: &str) -> String;
    async fn generate_admins_secret(&self, ctx: Arc<Context>) -> Result<Secret>;
    async fn generate_replica_secret(&self, ctx: Arc<Context>, pod_name: &str) -> Result<Secret>;
    async fn update_replica_secret(&self, ctx: Arc<Context>, pod_name: &str) -> Result<Secret>;
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum SecretType {
    AdminPasswords,
    ReplicaCert,
}

impl SecretExt for Kanidm {
    #[inline]
    fn admins_secret_name(&self) -> String {
        format!("{}-admin-passwords", self.name_any())
    }

    #[inline]
    fn replica_secret_name(&self, pod_name: &str) -> String {
        format!("{pod_name}-cert")
    }

    async fn generate_admins_secret(&self, ctx: Arc<Context>) -> Result<Secret> {
        let admin_password = self.recover_password(ctx.clone(), ADMIN_USER).await?;
        let idm_admin_password = self.recover_password(ctx.clone(), IDM_ADMIN_USER).await?;

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(self.admins_secret_name()),
                namespace: Some(self.namespace().unwrap()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                annotations: self
                    .spec
                    .service
                    .as_ref()
                    .and_then(|s| s.annotations.clone()),
                labels: Some(
                    self.generate_resource_labels()
                        .clone()
                        .into_iter()
                        .chain(std::iter::once((
                            SECRET_TYPE_LABEL.to_string(),
                            serde_plain::to_string(&SecretType::AdminPasswords).unwrap(),
                        )))
                        .collect(),
                ),
                ..ObjectMeta::default()
            },
            string_data: Some(
                [
                    ("ADMIN_USERNAME".to_string(), ADMIN_USER.to_string()),
                    (ADMIN_PASSWORD_KEY.to_string(), admin_password),
                    ("IDM_ADMIN_USERNAME".to_string(), IDM_ADMIN_USER.to_string()),
                    (IDM_ADMIN_PASSWORD_KEY.to_string(), idm_admin_password),
                ]
                .iter()
                .cloned()
                .collect(),
            ),
            ..Secret::default()
        };

        Ok(secret)
    }

    async fn generate_replica_secret(&self, ctx: Arc<Context>, pod_name: &str) -> Result<Secret> {
        let cert = self.get_replica_cert(ctx.clone(), pod_name).await?;
        let secret = self.build_replica_secret(cert, pod_name);
        Ok(secret)
    }

    async fn update_replica_secret(&self, ctx: Arc<Context>, pod_name: &str) -> Result<Secret> {
        info!(msg = format!("renewing replica certificate for pod {pod_name}"));
        let cert = self.update_replica_cert(ctx.clone(), pod_name).await?;
        let secret = self.build_replica_secret(cert, pod_name);
        Ok(secret)
    }
}

impl Kanidm {
    async fn recover_password(&self, ctx: Arc<Context>, user: &str) -> Result<String, Error> {
        let recover_command = vec!["kanidmd", "recover-account"];
        let password_output = self
            .exec_any(
                ctx.clone(),
                recover_command.into_iter().chain(std::iter::once(user)),
            )
            .await
            .map_err(|e| match e {
                Error::KubeExecError(_) => {
                    Error::ReceiveOutput(format!("failed to recover password for {user}"))
                }
                _ => e,
            })?;
        extract_password(password_output)
    }

    fn build_replica_secret(&self, cert: String, pod_name: &str) -> Secret {
        Secret {
            metadata: ObjectMeta {
                name: Some(self.replica_secret_name(pod_name)),
                namespace: Some(self.namespace().unwrap()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                annotations: self
                    .spec
                    .service
                    .as_ref()
                    .and_then(|s| s.annotations.clone()),
                labels: Some(
                    self.generate_resource_labels()
                        .clone()
                        .into_iter()
                        .chain([
                            (REPLICA_LABEL.to_string(), pod_name.to_string()),
                            (
                                SECRET_TYPE_LABEL.to_string(),
                                serde_plain::to_string(&SecretType::ReplicaCert).unwrap(),
                            ),
                        ])
                        .collect(),
                ),
                ..ObjectMeta::default()
            },
            string_data: Some(
                [(REPLICA_SECRET_KEY.to_string(), cert)]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            ..Secret::default()
        }
    }

    async fn get_replica_cert(&self, ctx: Arc<Context>, pod_name: &str) -> Result<String, Error> {
        let show_certificate_command = vec![
            "kanidmd",
            "show-replication-certificate",
            "-c",
            KANIDM_CONFIG_PATH,
        ];
        let cert_output = self
            .exec(ctx.clone(), pod_name, show_certificate_command)
            .await
            .map_err(|e| match e {
                Error::KubeExecError(_) => {
                    Error::ReceiveOutput(format!("failed to get certificate for {pod_name}"))
                }
                _ => e,
            })?;
        extract_cert(cert_output)
    }

    async fn update_replica_cert(
        &self,
        ctx: Arc<Context>,
        pod_name: &str,
    ) -> Result<String, Error> {
        let renew_command = vec![
            "kanidmd",
            "renew-replication-certificate",
            "-c",
            KANIDM_CONFIG_PATH,
        ];
        let cert_output = self
            .exec(ctx.clone(), pod_name, renew_command)
            .await
            .map_err(|e| {
                Error::ReceiveOutput(format!("failed to renew certificate for {pod_name}: {e}"))
            })?;
        extract_cert(cert_output)
    }
}

static PASSWORD_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"new_password:\s*"([^"]+)"#).expect("password regex must be valid")
});

static CERT_REGEX_V1_9: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"certificate:\s*"([^"]+)"#).expect("certificate regex (v1.9) must be valid")
});

static CERT_REGEX_V1_10: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"certificate=([A-Za-z0-9_+/=-]+)"#)
        .expect("certificate regex (v1.10) must be valid")
});

fn extract_password(output: String) -> Result<String, Error> {
    PASSWORD_REGEX
        .captures(&output)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| Error::ReceiveOutput("password was not found".to_string()))
}

fn extract_cert(output: String) -> Result<String, Error> {
    if let Some(caps) = CERT_REGEX_V1_9.captures(&output) {
        caps.get(1)
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| Error::ReceiveOutput("certificate was not found (v1.9)".to_string()))
    } else {
        CERT_REGEX_V1_10
            .captures(&output)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| Error::ReceiveOutput("certificate was not found".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_password() {
        let output = r#"
        00000000-0000-0000-0000-000000000000 WARN     🚧 [warn]: This is running as uid == 0 (root) which may be a security risk.
        00000000-0000-0000-0000-000000000000 WARN     🚧 [warn]: WARNING: DB folder /data has 'everyone' permission bits in the mode. This could be a security risk ...
        00000000-0000-0000-0000-000000000000 INFO     ｉ [info]: Running account recovery ...
        00000000-0000-0000-0000-000000000000 INFO     ｉ [info]:  | new_password: "ZJhV4PU18qrf7x2AJGTJvS1LyQbh7v9ZER8aZrFu9Evc9sqH""#.to_string();

        let result = extract_password(output).unwrap();
        assert_eq!(result, "ZJhV4PU18qrf7x2AJGTJvS1LyQbh7v9ZER8aZrFu9Evc9sqH");
    }

    #[test]
    fn test_extract_password_with_otel_tracing() {
        let output = r#"
        2026-05-07T18:10:01.000000Z INFO kanidmd::recover_account: Running account recovery
        trace_id=12345 span_id=67890
        2026-05-07T18:10:01.000000Z INFO kanidmd::recover_account:new_password: "OTELTestPassword123456789ABCDEFG"
        trace_id=12345 span_id=67890"#.to_string();

        let result = extract_password(output).unwrap();
        assert_eq!(result, "OTELTestPassword123456789ABCDEFG");
    }

    #[test]
    fn test_extract_password_with_extra_whitespace() {
        let output = r#"
        Running account recovery ...
         | new_password:     "WhitespaceTestPassword12345"  "#
            .to_string();

        let result = extract_password(output).unwrap();
        assert_eq!(result, "WhitespaceTestPassword12345");
    }

    #[test]
    fn test_extract_cert() {
        let output = r#"
        00000000-0000-0000-0000-000000000000 WARN     🚧 [warn]: This is running as uid == 0 (root) which may be a security risk.
        00000000-0000-0000-0000-000000000000 WARN     🚧 [warn]: WARNING: DB folder /data has 'everyone' permission bits in the mode. This could be a security risk ...
        00000000-0000-0000-0000-000000000000 INFO     ｉ [info]: Running show replication certificate ...
        00000000-0000-0000-0000-000000000000 INFO     ｉ [info]:  | certificate: "MIIB_DCCAaGgAwIBAgIBATAKBggqhkjOPQQDAjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDAeFw0yNDExMDYxOTEzMjdaFw0yODExMDYxOTEzMjdaMEwxGzAZBgNVBAoMEkthbmlkbSBSZXBsaWNhdGlvbjEtMCsGA1UEAwwkMmJhODMxNmEtZWJhYS00YmMxLTg0OTMtNWY4NmZhZmFlNTk0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuXp1hNNZerxDQbCh7rAGW6uM0CPECNd3IvbSh7qH34MkO_plwwDVKFbzcTG8HJE2ouIJlJYN8P4wf6qmrRQMAKN0MHIwDAYDVR0TAQH_BAIwADAOBgNVHQ8BAf8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBTaOaPuXmtLDTJVv--VYBiQr9gHCTAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSQAwRgIhAIZD_J4LyR7D0kg41GRg_TcRxm5mEVhM6WL9BO3XmfUsAiEA7Wpbkvd0b1e-Sg8AS9jP-CpBpmTnC7oEChkyhUYKyFc=""#.to_string();

        let result = extract_cert(output).unwrap();
        assert_eq!(
            result,
            "MIIB_DCCAaGgAwIBAgIBATAKBggqhkjOPQQDAjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDAeFw0yNDExMDYxOTEzMjdaFw0yODExMDYxOTEzMjdaMEwxGzAZBgNVBAoMEkthbmlkbSBSZXBsaWNhdGlvbjEtMCsGA1UEAwwkMmJhODMxNmEtZWJhYS00YmMxLTg0OTMtNWY4NmZhZmFlNTk0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuXp1hNNZerxDQbCh7rAGW6uM0CPECNd3IvbSh7qH34MkO_plwwDVKFbzcTG8HJE2ouIJlJYN8P4wf6qmrRQMAKN0MHIwDAYDVR0TAQH_BAIwADAOBgNVHQ8BAf8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBTaOaPuXmtLDTJVv--VYBiQr9gHCTAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSQAwRgIhAIZD_J4LyR7D0kg41GRg_TcRxm5mEVhM6WL9BO3XmfUsAiEA7Wpbkvd0b1e-Sg8AS9jP-CpBpmTnC7oEChkyhUYKyFc="
        );
    }

    #[test]
    fn test_extract_cert_with_otel_tracing() {
        let output = r#"
        2026-05-07T18:10:01.000000Z INFO kanidmd::show_certificate: Running show replication certificate
        trace_id=12345 span_id=67890
        2026-05-07T18:10:01.000000Z INFO kanidmd::show_certificate:certificate: "MIIBTestCertificateForOTELTracing123456789"
        trace_id=12345 span_id=67890"#.to_string();

        let result = extract_cert(output).unwrap();
        assert_eq!(result, "MIIBTestCertificateForOTELTracing123456789");
    }

    #[test]
    fn test_extract_cert_with_extra_whitespace() {
        let output = r#"
        Running show replication certificate ...
         | certificate:     "WhitespaceTestCert12345"  "#
            .to_string();

        let result = extract_cert(output).unwrap();
        assert_eq!(result, "WhitespaceTestCert12345");
    }

    #[test]
    fn test_extract_cert_v1_10_format() {
        let output = r#"
        2026-05-07T18:10:01.000000Z INFO kanidmd::show_certificate: Running show replication certificate
        2026-05-07T18:10:01.000000Z INFO kanidmd::show_certificate:certificate=MIIB_DCCAaGgAwIBAgIBATAKBggqhkjOPQQDAjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDAeFw0yNDExMDYxOTEzMjdaFw0yODExMDYxOTEzMjdaMEwxGzAZBgNVBAoMEkthbmlkbSBSZXBsaWNhdGlvbjEtMCsGA1UEAwwkMmJhODMxNmEtZWJhYS00YmMxLTg0OTMtNWY4NmZhZmFlNTk0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuXp1hNNZerxDQbCh7rAGW6uM0CPECNd3IvbSh7qH34MkO_plwwDVKFbzcTG8HJE2ouIJlJYN8P4wf6qmrRQMAKN0MHIwDAYDVR0TAQH_BAIwADAOBgNVHQ8BAf8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBTaOaPuXmtLDTJVv--VYBiQr9gHCTAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSQAwRgIhAIZD_J4LyR7D0kg41GRg_TcRxm5mEVhM6WL9BO3XmfUsAiEA7Wpbkvd0b1e-Sg8AS9jP-CpBpmTnC7oEChkyhUYKyFc="#.to_string();

        let result = extract_cert(output).unwrap();
        assert_eq!(
            result,
            "MIIB_DCCAaGgAwIBAgIBATAKBggqhkjOPQQDAjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDAeFw0yNDExMDYxOTEzMjdaFw0yODExMDYxOTEzMjdaMEwxGzAZBgNVBAoMEkthbmlkbSBSZXBsaWNhdGlvbjEtMCsGA1UEAwwkMmJhODMxNmEtZWJhYS00YmMxLTg0OTMtNWY4NmZhZmFlNTk0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuXp1hNNZerxDQbCh7rAGW6uM0CPECNd3IvbSh7qH34MkO_plwwDVKFbzcTG8HJE2ouIJlJYN8P4wf6qmrRQMAKN0MHIwDAYDVR0TAQH_BAIwADAOBgNVHQ8BAf8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBTaOaPuXmtLDTJVv--VYBiQr9gHCTAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSQAwRgIhAIZD_J4LyR7D0kg41GRg_TcRxm5mEVhM6WL9BO3XmfUsAiEA7Wpbkvd0b1e-Sg8AS9jP-CpBpmTnC7oEChkyhUYKyFc="
        );
    }
}
