use super::super::controller::context::Context;
use super::CLUSTER_LABEL;
use crate::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use crate::kanidm::crd::{Kanidm, MailSenderSpec, MailSenderStatus};
use kaniop_k8s_util::error::{Error, Result};

use std::collections::BTreeMap;
use std::sync::Arc;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::{
    Container, KeyToPath, PodSecurityContext, PodSpec, PodTemplateSpec, Secret, SecretVolumeSource,
    Volume, VolumeMount,
};
use kanidm_client::{ClientError, KanidmClient, StatusCode};
use kanidm_proto::internal::OperationError;
use kube::ResourceExt;
use kube::api::{Api, DeleteParams};
use tracing::{debug, info};

const MAIL_SENDER_LABEL: &str = "mail-sender";
const MAIL_SENDER_COMPONENT: &str = "kanidm-mail-sender";
const MESSAGE_SENDERS_GROUP: &str = "idm_message_senders";
const MAIL_SENDER_SERVICE_ACCOUNT_SUFFIX: &str = "mail-sender";
const MAIL_SENDER_TOKEN_SUFFIX: &str = "mail-sender-token";
const MAIL_SENDER_CONFIG_SUFFIX: &str = "mail-sender-config";
const MAIL_SENDER_DEPLOYMENT_SUFFIX: &str = "mail-sender";
const DEFAULT_QUEUE_POLL_INTERVAL: i32 = 5;
const DEFAULT_CONNECT_TIMEOUT: i32 = 15;
const CLIENT_CONFIG_KEY: &str = "client.toml";
const MAIL_CONFIG_KEY: &str = "mail-sender.toml";
const ENTRY_MANAGED_BY: &str = "idm_admin";

pub fn mail_sender_service_account_name(kanidm_name: &str) -> String {
    format!("{kanidm_name}-{MAIL_SENDER_SERVICE_ACCOUNT_SUFFIX}")
}

pub fn mail_sender_token_secret_name(kanidm_name: &str) -> String {
    format!("{kanidm_name}-{MAIL_SENDER_TOKEN_SUFFIX}")
}

pub fn mail_sender_config_map_name(kanidm_name: &str) -> String {
    format!("{kanidm_name}-{MAIL_SENDER_CONFIG_SUFFIX}")
}

pub fn mail_sender_deployment_name(kanidm_name: &str) -> String {
    format!("{kanidm_name}-{MAIL_SENDER_DEPLOYMENT_SUFFIX}")
}

pub async fn reconcile_mail_sender(
    kanidm: &Kanidm,
    kanidm_client: Arc<KanidmClient>,
    ctx: Arc<Context>,
) -> Result<Option<MailSenderStatus>> {
    let namespace = kanidm.namespace().unwrap();
    let kanidm_name = kanidm.name_any();

    if let Some(mail_sender_spec) = &kanidm.spec.mail_sender {
        info!(msg = "reconciling mail sender", namespace, kanidm_name);

        let sa_name = mail_sender_service_account_name(&kanidm_name);
        let config_secret_name = mail_sender_config_map_name(&kanidm_name);
        let deployment_name = mail_sender_deployment_name(&kanidm_name);

        ensure_mail_sender_service_account(&kanidm_client, &sa_name, &kanidm.spec.domain).await?;

        ensure_mail_sender_in_group(&kanidm_client, &sa_name).await?;

        let current_token_id = kanidm
            .status
            .as_ref()
            .and_then(|s| s.mail_sender.as_ref())
            .and_then(|ms| ms.token_id.clone());

        let (token, token_id) =
            ensure_mail_sender_token(&kanidm_client, &sa_name, current_token_id).await?;

        let smtp_credentials = read_smtp_credentials(&ctx, mail_sender_spec, &namespace).await?;

        let config_secret = create_config_secret(
            kanidm,
            &config_secret_name,
            mail_sender_spec,
            &token,
            &smtp_credentials,
        )?;
        kanidm.patch(&ctx, config_secret).await?;

        let deployment = create_deployment(
            kanidm,
            &deployment_name,
            mail_sender_spec,
            &config_secret_name,
        )?;
        kanidm.patch(&ctx, deployment).await?;

        let deployment_api: Api<Deployment> =
            Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
        let deployment_status = deployment_api
            .get(&deployment_name)
            .await
            .map_err(|e| Error::kube_error("get", "deployment", &namespace, &deployment_name, e))?;

        let ready = deployment_status
            .status
            .as_ref()
            .is_some_and(|s| s.ready_replicas.unwrap_or(0) >= 1);

        Ok(Some(MailSenderStatus {
            service_account_name: sa_name,
            token_secret_name: config_secret_name.clone(),
            deployment_name,
            config_map_name: config_secret_name,
            token_id: Some(token_id),
            ready,
        }))
    } else {
        debug!(
            msg = "mail sender not enabled, cleaning up resources",
            namespace, kanidm_name
        );
        cleanup_mail_sender_resources(kanidm, &ctx).await?;
        Ok(None)
    }
}

async fn ensure_mail_sender_service_account(
    kanidm_client: &KanidmClient,
    name: &str,
    domain: &str,
) -> Result<()> {
    debug!(msg = "ensuring mail sender service account exists", name);

    let display_name = format!("Mail Sender ({domain})");

    let create_result = kanidm_client
        .idm_service_account_create(name, &display_name, ENTRY_MANAGED_BY)
        .await;

    match create_result {
        Ok(_) => Ok(()),
        Err(e) if is_already_exists_error(&e) => {
            debug!(msg = "service account already exists, updating", name);
            kanidm_client
                .idm_service_account_update(name, None, Some(&display_name), None, None)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!("failed to update service account {name}"),
                        Box::new(e),
                    )
                })?;
            Ok(())
        }
        Err(e) => Err(Error::KanidmClientError(
            format!("failed to create service account {name}"),
            Box::new(e),
        )),
    }
}

async fn ensure_mail_sender_in_group(kanidm_client: &KanidmClient, name: &str) -> Result<()> {
    debug!(msg = "adding mail sender to message_senders group", name);

    let add_result = kanidm_client
        .idm_group_add_members(MESSAGE_SENDERS_GROUP, &[name])
        .await;

    match add_result {
        Ok(_) => Ok(()),
        Err(e) if is_already_member_error(&e) => {
            debug!(msg = "service account already in group", name);
            Ok(())
        }
        Err(e) => Err(Error::KanidmClientError(
            format!("failed to add {name} to {MESSAGE_SENDERS_GROUP}"),
            Box::new(e),
        )),
    }
}

async fn ensure_mail_sender_token(
    kanidm_client: &KanidmClient,
    name: &str,
    current_token_id: Option<String>,
) -> Result<(String, String)> {
    debug!(
        msg = "ensuring mail sender API token exists",
        name,
        current_token_id = ?current_token_id
    );

    let existing_tokens = kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .or_else(|e| match e {
            ClientError::Http(status, _, _) if status == 404 => Ok(vec![]),
            _ => Err(Error::KanidmClientError(
                format!("failed to list API tokens for {name}"),
                Box::new(e),
            )),
        })?;

    let existing_token = existing_tokens
        .iter()
        .find(|t| t.label == MAIL_SENDER_COMPONENT);

    if let Some(token) = existing_token {
        if current_token_id.as_ref() == Some(&token.token_id.to_string()) {
            debug!(
                msg = "mail sender token already exists with matching token_id, reusing",
                name,
                token_id = %token.token_id
            );
            return Ok((token.token_id.to_string(), token.token_id.to_string()));
        }

        debug!(
            msg = "mail sender token exists but token_id mismatch, destroying and regenerating",
            name,
            old_token_id = %token.token_id,
            expected_token_id = ?current_token_id
        );
        kanidm_client
            .idm_service_account_destroy_api_token(name, token.token_id)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!("failed to destroy API token {} for {name}", token.token_id),
                    Box::new(e),
                )
            })?;
    }

    debug!(
        msg = "generating new read-write API token for mail sender",
        name
    );

    let token_value = kanidm_client
        .idm_service_account_generate_api_token(name, MAIL_SENDER_COMPONENT, None, true, false)
        .await
        .map_err(|e| {
            Error::KanidmClientError(
                format!("failed to generate API token for {name}"),
                Box::new(e),
            )
        })?;

    let new_tokens = kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .map_err(|e| {
            Error::KanidmClientError(
                format!("failed to list API tokens for {name} after generation"),
                Box::new(e),
            )
        })?;

    let new_token = new_tokens
        .iter()
        .find(|t| t.label == MAIL_SENDER_COMPONENT)
        .ok_or_else(|| {
            Error::MissingData(format!(
                "generated token with label {MAIL_SENDER_COMPONENT} not found for {name}"
            ))
        })?;

    Ok((token_value, new_token.token_id.to_string()))
}

fn create_config_secret(
    kanidm: &Kanidm,
    name: &str,
    spec: &MailSenderSpec,
    token: &str,
    smtp_credentials: &SmtpCredentials,
) -> Result<Secret> {
    debug!(msg = "creating config secret", name);

    let domain = &kanidm.spec.domain;
    let default_origin = format!("https://{domain}");
    let origin = kanidm.spec.origin.as_ref().unwrap_or(&default_origin);
    let default_display_name = format!("Kanidm {domain}");
    let display_name = kanidm
        .spec
        .domain_appearance
        .as_ref()
        .and_then(|da| da.display_name.as_ref())
        .unwrap_or(&default_display_name);
    let poll_interval = spec
        .queue_poll_interval_seconds
        .unwrap_or(DEFAULT_QUEUE_POLL_INTERVAL);
    let connect_timeout = spec
        .connect_timeout_seconds
        .unwrap_or(DEFAULT_CONNECT_TIMEOUT);

    let client_config = format!("uri = {}", toml_basic_string(origin));

    let reply_to = spec
        .reply_to_address
        .as_deref()
        .unwrap_or(&spec.from_address);

    let relay_host = mail_relay_host(&spec.relay);

    let token = toml_basic_string(token);
    let display_name = toml_basic_string(display_name);
    let origin = toml_basic_string(origin);
    let from_address = toml_basic_string(&spec.from_address);
    let reply_to = toml_basic_string(reply_to);
    let relay_host = toml_basic_string(relay_host);
    let username = toml_basic_string(&smtp_credentials.username);
    let password = toml_basic_string(&smtp_credentials.password);
    let schedule = toml_basic_string(&format!("0 */{poll_interval} * * * *"));

    let mail_config = format!(
        r#"token = {token}
instance_display_name = {display_name}
instance_url = {origin}
mail_from_address = {from_address}
mail_reply_to_address = {reply_to}
mail_relay = {relay_host}
mail_username = {username}
mail_password = {password}
connect_timeout_seconds = {connect_timeout}
schedule = {schedule}
"#
    );

    let extended_labels = generate_extended_mail_sender_labels(kanidm);

    Ok(Secret {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(kanidm.namespace().unwrap()),
            labels: Some(extended_labels),
            ..Default::default()
        },
        string_data: Some(BTreeMap::from([
            (CLIENT_CONFIG_KEY.to_string(), client_config),
            (MAIL_CONFIG_KEY.to_string(), mail_config),
        ])),
        type_: Some("Opaque".to_string()),
        ..Default::default()
    })
}

fn create_deployment(
    kanidm: &Kanidm,
    name: &str,
    spec: &MailSenderSpec,
    config_secret_name: &str,
) -> Result<Deployment> {
    debug!(msg = "creating deployment", name);

    let namespace = kanidm.namespace().unwrap();
    let image = spec.image.clone().unwrap_or_else(|| {
        let default_image = kanidm.spec.image.clone();
        if default_image.contains("/server:") {
            default_image.replace("/server:", "/tools:")
        } else {
            format!("{}-tools", default_image)
        }
    });

    let extended_labels = generate_extended_mail_sender_labels(kanidm);

    let container = Container {
        name: MAIL_SENDER_COMPONENT.to_string(),
        image: Some(image),
        image_pull_policy: kanidm.spec.image_pull_policy.clone(),
        command: Some(vec![
            "/sbin/kanidm-mail-sender".to_string(),
            "-c".to_string(),
            format!("/data/config/{CLIENT_CONFIG_KEY}"),
            "-m".to_string(),
            format!("/data/config/{MAIL_CONFIG_KEY}"),
        ]),
        resources: spec.resources.clone(),
        volume_mounts: Some(vec![VolumeMount {
            name: "config".to_string(),
            mount_path: "/data/config".to_string(),
            read_only: Some(true),
            ..Default::default()
        }]),
        ..Default::default()
    };

    let pod_spec = PodSpec {
        containers: vec![container],
        security_context: Some(kanidm.spec.security_context.clone().unwrap_or_else(|| {
            PodSecurityContext {
                run_as_non_root: Some(true),
                run_as_user: Some(65534),
                ..Default::default()
            }
        })),
        volumes: Some(vec![Volume {
            name: "config".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(config_secret_name.to_string()),
                items: Some(vec![
                    KeyToPath {
                        key: CLIENT_CONFIG_KEY.to_string(),
                        path: CLIENT_CONFIG_KEY.to_string(),
                        ..Default::default()
                    },
                    KeyToPath {
                        key: MAIL_CONFIG_KEY.to_string(),
                        path: MAIL_CONFIG_KEY.to_string(),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        }]),
        node_selector: spec.node_selector.clone(),
        affinity: spec.affinity.clone(),
        tolerations: spec.tolerations.clone(),
        automount_service_account_token: Some(false),
        ..Default::default()
    };

    let pod_template = PodTemplateSpec {
        metadata: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            labels: Some(extended_labels.clone()),
            ..Default::default()
        }),
        spec: Some(pod_spec),
    };

    Ok(Deployment {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace),
            labels: Some(extended_labels.clone()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some(extended_labels),
                ..Default::default()
            },
            template: pod_template,
            strategy: Some(DeploymentStrategy {
                type_: Some("Recreate".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
}

struct SmtpCredentials {
    username: String,
    password: String,
}

async fn read_smtp_credentials(
    ctx: &Context,
    spec: &MailSenderSpec,
    namespace: &str,
) -> Result<SmtpCredentials> {
    let secret_api: Api<Secret> = Api::namespaced(ctx.kaniop_ctx.client.clone(), namespace);
    let secret = secret_api
        .get(&spec.credentials_secret.name)
        .await
        .map_err(|e| {
            Error::kube_error(
                "get",
                "SMTP credentials secret",
                namespace,
                &spec.credentials_secret.name,
                e,
            )
        })?;

    let username_key = &spec.credentials_secret.username_key;
    let password_key = &spec.credentials_secret.password_key;

    let get_string = |key: &str| -> Result<String> {
        secret
            .string_data
            .as_ref()
            .and_then(|d| d.get(key).cloned())
            .or_else(|| {
                secret
                    .data
                    .as_ref()
                    .and_then(|d| d.get(key))
                    .map(|v| String::from_utf8_lossy(&v.0).to_string())
            })
            .ok_or_else(|| {
                Error::MissingData(format!(
                    "SMTP credentials secret {} missing key {}",
                    spec.credentials_secret.name, key
                ))
            })
    };

    Ok(SmtpCredentials {
        username: get_string(username_key)?,
        password: get_string(password_key)?,
    })
}

pub async fn cleanup_mail_sender_resources(kanidm: &Kanidm, ctx: &Context) -> Result<()> {
    let namespace = kanidm.namespace().unwrap();
    let kanidm_name = kanidm.name_any();

    let deployment_name = mail_sender_deployment_name(&kanidm_name);
    let config_secret_name = mail_sender_config_map_name(&kanidm_name);

    let deployment_api: Api<Deployment> =
        Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
    let secret_api: Api<k8s_openapi::api::core::v1::Secret> =
        Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);

    let dp = DeleteParams::default();

    match deployment_api.delete(&deployment_name, &dp).await {
        Ok(_) => {}
        Err(kube::Error::Api(e)) if e.code == 404 => {
            debug!(
                msg = "mail sender deployment already absent",
                deployment_name
            );
        }
        Err(e) => {
            return Err(Error::kube_error(
                "delete",
                "deployment",
                &namespace,
                &deployment_name,
                e,
            ));
        }
    }

    match secret_api.delete(&config_secret_name, &dp).await {
        Ok(_) => {}
        Err(kube::Error::Api(e)) if e.code == 404 => {
            debug!(
                msg = "mail sender config secret already absent",
                config_secret_name
            );
        }
        Err(e) => {
            return Err(Error::kube_error(
                "delete",
                "secret",
                &namespace,
                &config_secret_name,
                e,
            ));
        }
    }

    Ok(())
}

pub async fn cleanup_mail_sender_in_kanidm(
    kanidm_client: &KanidmClient,
    kanidm_name: &str,
) -> Result<()> {
    let sa_name = mail_sender_service_account_name(kanidm_name);

    debug!(
        msg = "removing mail sender from message_senders group",
        sa_name
    );
    match kanidm_client
        .idm_group_remove_members(MESSAGE_SENDERS_GROUP, &[&sa_name])
        .await
    {
        Ok(()) => {}
        Err(e) if is_not_found_error(&e) || is_not_a_member_error(&e) => {
            debug!(msg = "mail sender group membership already absent", sa_name);
        }
        Err(e) => {
            return Err(Error::KanidmClientError(
                format!("failed to remove {sa_name} from {MESSAGE_SENDERS_GROUP}"),
                Box::new(e),
            ));
        }
    }

    debug!(msg = "deleting mail sender service account", sa_name);
    match kanidm_client.idm_service_account_delete(&sa_name).await {
        Ok(()) => {}
        Err(e) if is_not_found_error(&e) => {
            debug!(msg = "mail sender service account already absent", sa_name);
        }
        Err(e) => {
            return Err(Error::KanidmClientError(
                format!("failed to delete service account {sa_name}"),
                Box::new(e),
            ));
        }
    }

    Ok(())
}

fn is_already_exists_error(e: &kanidm_client::ClientError) -> bool {
    match e {
        kanidm_client::ClientError::Http(status, _, body) => {
            *status == StatusCode::CONFLICT || body.contains("already exists")
        }
        _ => false,
    }
}

fn is_already_member_error(e: &kanidm_client::ClientError) -> bool {
    match e {
        kanidm_client::ClientError::Http(_, _, body) => body.contains("already a member"),
        _ => false,
    }
}

fn is_not_found_error(e: &kanidm_client::ClientError) -> bool {
    match e {
        ClientError::Http(status, operation_error, body) => {
            *status == StatusCode::NOT_FOUND
                || operation_error == &Some(OperationError::NoMatchingEntries)
                || body.contains("NoMatchingEntries")
                || body.contains("not found")
                || body.contains("does not exist")
        }
        _ => false,
    }
}

fn is_not_a_member_error(e: &kanidm_client::ClientError) -> bool {
    match e {
        ClientError::Http(_, _, body) => body.contains("not a member"),
        _ => false,
    }
}

fn mail_relay_host(relay: &str) -> &str {
    relay
        .strip_prefix("smtps://")
        .or_else(|| relay.strip_prefix("smtp://"))
        .unwrap_or(relay)
}

fn toml_basic_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for c in value.chars() {
        match c {
            '\u{08}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{0c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            c if c.is_control() => {
                let code = c as u32;
                if code <= 0xffff {
                    output.push_str(&format!("\\u{code:04X}"));
                } else {
                    output.push_str(&format!("\\U{code:08X}"));
                }
            }
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

fn generate_mail_sender_labels(kanidm: &Kanidm) -> BTreeMap<String, String> {
    BTreeMap::from([
        (NAME_LABEL.to_string(), "kanidm".to_string()),
        (
            MANAGED_BY_LABEL.to_string(),
            format!("kaniop-{}", super::super::controller::CONTROLLER_ID),
        ),
        (INSTANCE_LABEL.to_string(), kanidm.name_any()),
        (CLUSTER_LABEL.to_string(), kanidm.name_any()),
    ])
}

fn generate_extended_mail_sender_labels(kanidm: &Kanidm) -> BTreeMap<String, String> {
    generate_mail_sender_labels(kanidm)
        .into_iter()
        .chain([
            (MAIL_SENDER_LABEL.to_string(), kanidm.name_any()),
            (NAME_LABEL.to_string(), MAIL_SENDER_COMPONENT.to_string()),
        ])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{mail_relay_host, toml_basic_string};

    #[test]
    fn mail_relay_host_strips_supported_schemes() {
        assert_eq!(
            mail_relay_host("smtps://smtp.example.com"),
            "smtp.example.com"
        );
        assert_eq!(
            mail_relay_host("smtp://smtp.example.com:587"),
            "smtp.example.com:587"
        );
        assert_eq!(mail_relay_host("smtp.example.com"), "smtp.example.com");
    }

    #[test]
    fn toml_basic_string_escapes_config_values() {
        assert_eq!(
            toml_basic_string("quoted \"value\" with \\ slash\nand tab\t"),
            r#""quoted \"value\" with \\ slash\nand tab\t""#
        );
    }
}
