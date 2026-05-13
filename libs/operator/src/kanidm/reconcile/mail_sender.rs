use super::super::controller::context::Context;
use super::CLUSTER_LABEL;
use crate::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use crate::kanidm::crd::{Kanidm, MailSenderSpec, MailSenderStatus};
use kaniop_k8s_util::error::{Error, Result};

use std::collections::BTreeMap;
use std::sync::Arc;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, EnvVar, EnvVarSource, KeyToPath, PodSpec, PodTemplateSpec, Secret,
    SecretKeySelector, SecretVolumeSource, Volume, VolumeMount,
};
use kanidm_client::KanidmClient;
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
const TOKEN_KEY: &str = "token";
const CONFIG_KEY: &str = "server.toml";

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
        let token_secret_name = mail_sender_token_secret_name(&kanidm_name);
        let config_map_name = mail_sender_config_map_name(&kanidm_name);
        let deployment_name = mail_sender_deployment_name(&kanidm_name);

        ensure_mail_sender_service_account(&kanidm_client, &sa_name, &kanidm.spec.domain).await?;

        ensure_mail_sender_in_group(&kanidm_client, &sa_name).await?;

        let token = generate_mail_sender_token(&kanidm_client, &sa_name).await?;

        let token_secret = create_token_secret(kanidm, &token_secret_name, &token)?;
        kanidm.patch(&ctx, token_secret).await?;

        let config_map = create_config_map(
            kanidm,
            &config_map_name,
            mail_sender_spec,
            &token_secret_name,
            &sa_name,
        )?;
        kanidm.patch(&ctx, config_map).await?;

        let deployment = create_deployment(
            kanidm,
            &deployment_name,
            mail_sender_spec,
            &token_secret_name,
            &config_map_name,
        )?;
        kanidm.patch(&ctx, deployment).await?;

        let deployment_api: Api<Deployment> =
            Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
        let deployment_status = deployment_api.get(&deployment_name).await.map_err(|e| {
            Error::KubeError(
                format!("failed to get deployment {namespace}/{deployment_name}"),
                Box::new(e),
            )
        })?;

        let ready = deployment_status
            .status
            .as_ref()
            .is_some_and(|s| s.ready_replicas.unwrap_or(0) >= 1);

        Ok(Some(MailSenderStatus {
            service_account_name: sa_name,
            token_secret_name,
            deployment_name,
            config_map_name,
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
        .idm_service_account_create(name, &display_name, "idm_admin")
        .await;

    match create_result {
        Ok(_) => Ok(()),
        Err(e) if is_already_exists_error(&e) => {
            debug!(msg = "service account already exists, updating", name);
            kanidm_client
                .idm_service_account_update(
                    name,
                    None,
                    Some(&display_name),
                    Some("idm_admin"),
                    None,
                )
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

async fn generate_mail_sender_token(kanidm_client: &KanidmClient, name: &str) -> Result<String> {
    debug!(
        msg = "generating read-write API token for mail sender",
        name
    );

    let token = kanidm_client
        .idm_service_account_generate_api_token(name, MAIL_SENDER_COMPONENT, None, true, false)
        .await
        .map_err(|e| {
            Error::KanidmClientError(
                format!("failed to generate API token for {name}"),
                Box::new(e),
            )
        })?;

    Ok(token)
}

fn create_token_secret(kanidm: &Kanidm, name: &str, token: &str) -> Result<Secret> {
    debug!(msg = "creating token secret", name);

    let extended_labels = generate_extended_mail_sender_labels(kanidm);

    Ok(Secret {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(kanidm.namespace().unwrap()),
            labels: Some(extended_labels),
            ..Default::default()
        },
        string_data: Some(BTreeMap::from([(TOKEN_KEY.to_string(), token.to_string())])),
        type_: Some("Opaque".to_string()),
        ..Default::default()
    })
}

fn create_config_map(
    kanidm: &Kanidm,
    name: &str,
    spec: &MailSenderSpec,
    _token_secret_name: &str,
    _service_account_name: &str,
) -> Result<ConfigMap> {
    debug!(msg = "creating config map", name);

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

    let config_content = format!(
        r#"[kanidm]
uri = "{origin}"

[mail]
# API token from secret
api_token_file = "/data/token/token"

# Display name
instance_display_name = "{display_name}"
instance_url = "{origin}"

# Mail addresses
from_address = "{}"
{}reply_to_address = "{}"

# SMTP settings
relay = "{}"
connect_timeout_seconds = {connect_timeout}

# Queue polling
schedule = "*/{poll_interval} * * * * * * *"
"#,
        spec.from_address,
        if spec.reply_to_address.is_some() {
            ""
        } else {
            "# "
        },
        spec.reply_to_address.as_deref().unwrap_or_default(),
        spec.relay
    );

    let extended_labels = generate_extended_mail_sender_labels(kanidm);

    Ok(ConfigMap {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(kanidm.namespace().unwrap()),
            labels: Some(extended_labels),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(CONFIG_KEY.to_string(), config_content)])),
        ..Default::default()
    })
}

fn create_deployment(
    kanidm: &Kanidm,
    name: &str,
    spec: &MailSenderSpec,
    token_secret_name: &str,
    config_map_name: &str,
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

    let smtp_env_vars = vec![
        EnvVar {
            name: "KANIDM_MAIL_USERNAME".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: spec.credentials_secret.name.clone(),
                    key: spec.credentials_secret.username_key.clone(),
                    optional: Some(false),
                }),
                config_map_key_ref: None,
                field_ref: None,
                resource_field_ref: None,
            }),
            ..Default::default()
        },
        EnvVar {
            name: "KANIDM_MAIL_PASSWORD".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: spec.credentials_secret.name.clone(),
                    key: spec.credentials_secret.password_key.clone(),
                    optional: Some(false),
                }),
                config_map_key_ref: None,
                field_ref: None,
                resource_field_ref: None,
            }),
            ..Default::default()
        },
    ];

    let container = Container {
        name: MAIL_SENDER_COMPONENT.to_string(),
        image: Some(image),
        image_pull_policy: kanidm.spec.image_pull_policy.clone(),
        command: Some(vec![
            "/sbin/kanidm-mail-sender".to_string(),
            "-c".to_string(),
            format!("/data/config/{CONFIG_KEY}"),
        ]),
        env: Some(smtp_env_vars),
        resources: spec.resources.clone(),
        volume_mounts: Some(vec![
            VolumeMount {
                name: "token".to_string(),
                mount_path: "/data/token".to_string(),
                read_only: Some(true),
                ..Default::default()
            },
            VolumeMount {
                name: "config".to_string(),
                mount_path: "/data/config".to_string(),
                read_only: Some(true),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };

    let pod_spec = PodSpec {
        containers: vec![container],
        volumes: Some(vec![
            Volume {
                name: "token".to_string(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(token_secret_name.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Volume {
                name: "config".to_string(),
                config_map: Some(k8s_openapi::api::core::v1::ConfigMapVolumeSource {
                    name: config_map_name.to_string(),
                    items: Some(vec![KeyToPath {
                        key: CONFIG_KEY.to_string(),
                        path: CONFIG_KEY.to_string(),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]),
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

pub async fn cleanup_mail_sender_resources(kanidm: &Kanidm, ctx: &Context) -> Result<()> {
    let namespace = kanidm.namespace().unwrap();
    let kanidm_name = kanidm.name_any();

    let deployment_name = mail_sender_deployment_name(&kanidm_name);
    let config_map_name = mail_sender_config_map_name(&kanidm_name);
    let token_secret_name = mail_sender_token_secret_name(&kanidm_name);

    let deployment_api: Api<Deployment> =
        Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
    let config_map_api: Api<ConfigMap> = Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
    let secret_api: Api<k8s_openapi::api::core::v1::Secret> =
        Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);

    let dp = DeleteParams::default();

    let _ = deployment_api
        .delete(&deployment_name, &dp)
        .await
        .map_err(|e| {
            debug!(msg = "failed to delete deployment, may not exist", ?e);
        });
    let _ = config_map_api
        .delete(&config_map_name, &dp)
        .await
        .map_err(|e| {
            debug!(msg = "failed to delete config map, may not exist", ?e);
        });
    let _ = secret_api
        .delete(&token_secret_name, &dp)
        .await
        .map_err(|e| {
            debug!(msg = "failed to delete token secret, may not exist", ?e);
        });

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
    let _ = kanidm_client
        .idm_group_remove_members(MESSAGE_SENDERS_GROUP, &[&sa_name])
        .await
        .map_err(|e| {
            debug!(msg = "failed to remove from group, may not exist", ?e);
        });

    debug!(msg = "deleting mail sender service account", sa_name);
    let _ = kanidm_client
        .idm_service_account_delete(&sa_name)
        .await
        .map_err(|e| {
            debug!(msg = "failed to delete service account, may not exist", ?e);
        });

    Ok(())
}

fn is_already_exists_error(e: &kanidm_client::ClientError) -> bool {
    match e {
        kanidm_client::ClientError::Http(_, _, body) => body.contains("already exists"),
        _ => false,
    }
}

fn is_already_member_error(e: &kanidm_client::ClientError) -> bool {
    match e {
        kanidm_client::ClientError::Http(_, _, body) => body.contains("already a member"),
        _ => false,
    }
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
