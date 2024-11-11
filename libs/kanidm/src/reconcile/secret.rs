use crate::crd::Kanidm;

use kaniop_operator::error::{Error, Result};
use serde_json::Value;

use std::sync::Arc;

use k8s_openapi::api::core::v1::Secret;
use kaniop_operator::controller::Context;
use kube::api::{ObjectMeta, Resource};
use kube::ResourceExt;

// decode with `basenc --base64url -d | openssl x509 -noout -text -inform DER`
pub const REPLICA_SECRET_KEY: &str = "tls.der.b64url";

const ADMIN_USER: &str = "admin";
const IDM_ADMIN_USER: &str = "idm_admin";

#[allow(async_fn_in_trait)]
pub trait SecretExt {
    fn admins_secret_name(&self) -> String;
    fn replica_secret_name(&self, pod_name: &str) -> String;
    async fn recover_password(&self, ctx: Arc<Context>, user: &str) -> Result<String, Error>;
    async fn get_replica_cert(&self, ctx: Arc<Context>, pod_name: &str) -> Result<String, Error>;
    async fn generate_admins_secret(&self, ctx: Arc<Context>) -> Result<Secret>;
    async fn generate_replica_secret(&self, ctx: Arc<Context>, pod_name: &str) -> Result<Secret>;
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

    async fn recover_password(&self, ctx: Arc<Context>, user: &str) -> Result<String, Error> {
        let recover_command = vec!["kanidmd", "recover-account", "--output", "json"];
        let password_output = self
            .exec_any(
                ctx.clone(),
                recover_command.into_iter().chain(std::iter::once(user)),
            )
            .await?
            .ok_or_else(|| {
                Error::ReceiveOutput(format!("failed to recover password for {user}"))
            })?;
        extract_password(password_output)
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
                        .chain(self.labels().clone())
                        .collect(),
                ),
                ..ObjectMeta::default()
            },
            string_data: Some(
                [
                    ("admin".to_string(), admin_password),
                    ("idm_admin".to_string(), idm_admin_password),
                ]
                .iter()
                .cloned()
                .collect(),
            ),
            ..Secret::default()
        };

        Ok(secret)
    }

    async fn get_replica_cert(&self, ctx: Arc<Context>, pod_name: &str) -> Result<String, Error> {
        // JSON output cannot be used here because of: https://github.com/kanidm/kanidm/pull/3179
        // from 1.5.0+ we can use --output json
        let show_certificate_command = vec!["kanidmd", "show-replication-certificate"];
        let cert_output = self
            .exec(ctx.clone(), pod_name, show_certificate_command)
            .await?
            .ok_or_else(|| {
                Error::ReceiveOutput(format!("failed to get certificate for {pod_name}"))
            })?;
        extract_cert(cert_output)
    }

    async fn generate_replica_secret(&self, ctx: Arc<Context>, pod_name: &str) -> Result<Secret> {
        let cert = self.get_replica_cert(ctx.clone(), pod_name).await?;

        let secret = Secret {
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
                        .chain(self.labels().clone())
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
        };

        Ok(secret)
    }
}

fn extract_password(output: String) -> Result<String, Error> {
    let last_line = output
        .lines()
        .last()
        .ok_or_else(|| Error::ReceiveOutput("no lines parsing exec output".to_string()))?;
    let json: Value = serde_json::from_str(last_line).map_err(|e| {
        Error::SerializationError(
            "failed password serialization parsing exec output".to_string(),
            e,
        )
    })?;
    let password = json
        .get("password")
        .ok_or_else(|| {
            Error::ReceiveOutput("no password field in JSON parsing exec output".to_string())
        })?
        .as_str()
        .ok_or_else(|| {
            Error::ReceiveOutput("password field is not a string parsing exec output".to_string())
        })?;
    Ok(password.to_string())
}

fn extract_cert(output: String) -> Result<String, Error> {
    let start_pattern = r#"certificate: ""#;
    if let Some(start_index) = output.find(start_pattern) {
        let start = start_index + start_pattern.len();
        if let Some(end_index) = output[start..].find('"') {
            return Ok(output[start..start + end_index].to_string());
        }
    }
    Err(Error::ReceiveOutput(
        "certificate was not found".to_string(),
    ))
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
        {"password":"ZJhV4PU18qrf7x2AJGTJvS1LyQbh7v9ZER8aZrFu9Evc9sqH"}"#.to_string();

        let result = extract_password(output).unwrap();
        assert_eq!(result, "ZJhV4PU18qrf7x2AJGTJvS1LyQbh7v9ZER8aZrFu9Evc9sqH");
    }

    #[test]
    fn test_extract_cert() {
        let output = r#"
        00000000-0000-0000-0000-000000000000 WARN     🚧 [warn]: This is running as uid == 0 (root) which may be a security risk.
        00000000-0000-0000-0000-000000000000 WARN     🚧 [warn]: WARNING: DB folder /data has 'everyone' permission bits in the mode. This could be a security risk ...
        00000000-0000-0000-0000-000000000000 INFO     ｉ [info]: Running show replication certificate ...
        00000000-0000-0000-0000-000000000000 INFO     ｉ [info]:  | certificate: "MIIB_DCCAaGgAwIBAgIBATAKBggqhkjOPQQDAjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDAeFw0yNDExMDYxOTEzMjdaFw0yODExMDYxOTEzMjdaMEwxGzAZBgNVBAoMEkthbmlkbSBSZXBsaWNhdGlvbjEtMCsGA1UEAwwkMmJhODMxNmEtZWJhYS00YmMxLTg0OTMtNWY4NmZhZmFlNTk0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuXp1hNNZerxDQbCh7rAGW6uM0CPECNd3IvbSh7qH34MkO_plwwDVKFbzcTG8HJE2ouIJlJYN8P4wf6qmrRQMAKN0MHIwDAYDVR0TAQH_BAIwADAOBgNVHQ8BAf8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBTaOaPuXmtLDTJVv--VYBiQr9gHCTAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSQAwRgIhAIZD_J4LyR7D0kg41GRg_TcRxm5mEVhM6WL9BO3XmfUsAiEA7Wpbkvd0b1e-Sg8AS9jP-CpBpmTnC7oEChkyhUYKyFc=""#.to_string();

        let result = extract_cert(output).unwrap();
        assert_eq!(result, "MIIB_DCCAaGgAwIBAgIBATAKBggqhkjOPQQDAjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDAeFw0yNDExMDYxOTEzMjdaFw0yODExMDYxOTEzMjdaMEwxGzAZBgNVBAoMEkthbmlkbSBSZXBsaWNhdGlvbjEtMCsGA1UEAwwkMmJhODMxNmEtZWJhYS00YmMxLTg0OTMtNWY4NmZhZmFlNTk0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuXp1hNNZerxDQbCh7rAGW6uM0CPECNd3IvbSh7qH34MkO_plwwDVKFbzcTG8HJE2ouIJlJYN8P4wf6qmrRQMAKN0MHIwDAYDVR0TAQH_BAIwADAOBgNVHQ8BAf8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBTaOaPuXmtLDTJVv--VYBiQr9gHCTAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSQAwRgIhAIZD_J4LyR7D0kg41GRg_TcRxm5mEVhM6WL9BO3XmfUsAiEA7Wpbkvd0b1e-Sg8AS9jP-CpBpmTnC7oEChkyhUYKyFc=");
    }
}
