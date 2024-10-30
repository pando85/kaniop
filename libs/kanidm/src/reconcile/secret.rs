use crate::crd::Kanidm;

use kaniop_operator::error::{Error, Result};
use serde_json::Value;

use std::sync::Arc;

use k8s_openapi::api::core::v1::Secret;
use kaniop_operator::controller::Context;
use kube::api::{ObjectMeta, Resource};
use kube::ResourceExt;

const ADMIN_USER: &str = "admin";
const IDM_ADMIN_USER: &str = "idm_admin";

pub trait SecretExt {
    fn get_admins_secret_name(&self) -> String;
    async fn recover_password(&self, ctx: Arc<Context>, user: &str) -> Result<String, Error>;
    async fn generate_admins_secret(&self, ctx: Arc<Context>) -> Result<Secret>;
}

impl SecretExt for Kanidm {
    #[inline]
    fn get_admins_secret_name(&self) -> String {
        format!("{}-admin-passwords", self.name_any())
    }

    async fn recover_password(&self, ctx: Arc<Context>, user: &str) -> Result<String, Error> {
        let recover_command = vec!["kanidmd", "recover-account", "--output", "json"];
        let password_output = self
            .exec(
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

        let labels = self
            .get_labels()
            .clone()
            .into_iter()
            .chain(self.labels().clone())
            .collect();

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(self.get_admins_secret_name()),
                namespace: Some(self.namespace().unwrap()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                annotations: self
                    .spec
                    .service
                    .as_ref()
                    .and_then(|s| s.annotations.clone()),
                labels: Some(labels),
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
}
