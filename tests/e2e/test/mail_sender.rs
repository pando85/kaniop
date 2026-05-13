use super::{kanidm::is_kanidm, poll_until, setup_kanidm_connection, wait_for};

use kaniop_operator::kanidm::crd::{Kanidm, MailSenderSpec, MailSenderStatus};
use kaniop_operator::kanidm::reconcile::mail_sender::{
    mail_sender_config_map_name, mail_sender_deployment_name, mail_sender_service_account_name,
    mail_sender_token_secret_name,
};

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::{ObjectMeta, Patch, PatchParams, PostParams};
use kube::runtime::{conditions, wait::Condition};
use kube::{Api, Client, ResourceExt};
use serde_json::json;

fn is_mail_sender_ready() -> impl Condition<Kanidm> {
    move |obj: Option<&Kanidm>| {
        obj.and_then(|kanidm| kanidm.status.as_ref())
            .and_then(|status| status.mail_sender.as_ref())
            .is_some_and(|ms| ms.ready)
    }
}

fn get_mail_sender_status(kanidm: &Kanidm) -> Option<MailSenderStatus> {
    kanidm.status.as_ref().and_then(|s| s.mail_sender.clone())
}

async fn create_smtp_secret(client: &Client, name: &str, username: &str, password: &str) {
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");

    let mut string_data = BTreeMap::new();
    string_data.insert("username".to_string(), username.to_string());
    string_data.insert("password".to_string(), password.to_string());

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        string_data: Some(string_data),
        type_: Some("Opaque".to_string()),
        ..Default::default()
    };

    secret_api
        .create(&PostParams::default(), &secret)
        .await
        .unwrap();
}

async fn create_smtp_secret_with_keys(
    client: &Client,
    name: &str,
    username_key: &str,
    username: &str,
    password_key: &str,
    password: &str,
) {
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");

    let mut string_data = BTreeMap::new();
    string_data.insert(username_key.to_string(), username.to_string());
    string_data.insert(password_key.to_string(), password.to_string());

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        string_data: Some(string_data),
        type_: Some("Opaque".to_string()),
        ..Default::default()
    };

    secret_api
        .create(&PostParams::default(), &secret)
        .await
        .unwrap();
}

async fn cleanup_mail_sender(kanidm_api: &Api<Kanidm>, kanidm_name: &str) {
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(json!({"spec": {"mailSender": null}})),
        )
        .await
        .unwrap();
}

const KANIDM_NAME: &str = "test-mail-sender";

#[tokio::test]
async fn mail_sender_create() {
    let name = "test-mail-sender-create";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name,
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            KANIDM_NAME,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), KANIDM_NAME, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    assert!(mail_sender_status.ready);
    assert_eq!(
        mail_sender_status.service_account_name,
        mail_sender_service_account_name(KANIDM_NAME)
    );
    assert_eq!(
        mail_sender_status.token_secret_name,
        mail_sender_token_secret_name(KANIDM_NAME)
    );
    assert_eq!(
        mail_sender_status.deployment_name,
        mail_sender_deployment_name(KANIDM_NAME)
    );
    assert_eq!(
        mail_sender_status.config_map_name,
        mail_sender_config_map_name(KANIDM_NAME)
    );

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    assert_eq!(deployment.spec.unwrap().replicas.unwrap(), 1);

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let token_secret = secret_api
        .get(&mail_sender_status.token_secret_name)
        .await
        .unwrap();
    assert!(token_secret.string_data.is_some() || token_secret.data.is_some());

    let config_map_api = Api::<ConfigMap>::namespaced(s.client.clone(), "default");
    let config_map = config_map_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    assert!(config_map.data.is_some());
    let config_data = config_map.data.unwrap();
    assert!(config_data.contains_key("server.toml"));

    let service_account_name = mail_sender_status.service_account_name;
    let kanidm_sa = s
        .kanidm_client
        .idm_service_account_get(&service_account_name)
        .await
        .unwrap();
    assert!(kanidm_sa.is_some());

    cleanup_mail_sender(&kanidm_api, KANIDM_NAME).await;
}

#[tokio::test]
async fn mail_sender_disable() {
    let name = "test-mail-sender-disable";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name,
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            KANIDM_NAME,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), KANIDM_NAME, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    let deployment_uid = deployment.uid().unwrap();

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let token_secret = secret_api
        .get(&mail_sender_status.token_secret_name)
        .await
        .unwrap();
    let token_secret_uid = token_secret.uid().unwrap();

    let config_map_api = Api::<ConfigMap>::namespaced(s.client.clone(), "default");
    let config_map = config_map_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    let config_map_uid = config_map.uid().unwrap();

    cleanup_mail_sender(&kanidm_api, KANIDM_NAME).await;

    wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;

    poll_until("mail sender status to be removed", || async {
        kanidm_api
            .get(KANIDM_NAME)
            .await
            .ok()
            .and_then(|k| k.status)
            .and_then(|s| s.mail_sender)
            .is_none()
            .then_some(())
    })
    .await;

    wait_for(
        deployment_api.clone(),
        &mail_sender_status.deployment_name,
        conditions::is_deleted(&deployment_uid),
    )
    .await;

    wait_for(
        secret_api.clone(),
        &mail_sender_status.token_secret_name,
        conditions::is_deleted(&token_secret_uid),
    )
    .await;

    wait_for(
        config_map_api.clone(),
        &mail_sender_status.config_map_name,
        conditions::is_deleted(&config_map_uid),
    )
    .await;
}

#[tokio::test]
async fn mail_sender_update() {
    let name = "test-mail-sender-update";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name.clone(),
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        reply_to_address: Some("admin@example.com".to_string()),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            KANIDM_NAME,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), KANIDM_NAME, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let config_map_api = Api::<ConfigMap>::namespaced(s.client.clone(), "default");
    let config_map = config_map_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    let config_data = config_map.data.unwrap();
    let server_toml = config_data.get("server.toml").unwrap();
    assert!(server_toml.contains("reply_to_address = \"admin@example.com\""));

    let mut kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtp://smtp-new.example.com:587".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name,
            ..Default::default()
        },
        from_address: "updated@example.com".to_string(),
        queue_poll_interval_seconds: Some(10),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            KANIDM_NAME,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), KANIDM_NAME, is_mail_sender_ready()).await;

    let updated_config_map = config_map_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    let updated_config_data = updated_config_map.data.unwrap();
    let updated_server_toml = updated_config_data.get("server.toml").unwrap();
    assert!(updated_server_toml.contains("relay = \"smtp://smtp-new.example.com:587\""));
    assert!(updated_server_toml.contains("from_address = \"updated@example.com\""));
    assert!(!updated_server_toml.contains("reply_to_address"));
    assert!(updated_server_toml.contains("schedule = \"*/10 * * * * * * *\""));

    cleanup_mail_sender(&kanidm_api, KANIDM_NAME).await;
}

#[tokio::test]
async fn mail_sender_custom_keys() {
    let name = "test-mail-sender-custom-keys";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret_with_keys(
        &s.client,
        &smtp_secret_name,
        "smtp-user",
        "custom-user",
        "smtp-pass",
        "custom-password",
    )
    .await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name,
            username_key: "smtp-user".to_string(),
            password_key: "smtp-pass".to_string(),
        },
        from_address: "kanidm@example.com".to_string(),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            KANIDM_NAME,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), KANIDM_NAME, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    let deployment_spec = deployment.spec.unwrap();
    let pod_spec = deployment_spec.template.spec.unwrap();
    let container = pod_spec.containers.first().unwrap();
    let env_vars = container.env.as_ref().unwrap();

    let username_env = env_vars
        .iter()
        .find(|e| e.name == "KANIDM_MAIL_USERNAME")
        .unwrap();
    assert_eq!(
        username_env
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap()
            .key,
        "smtp-user"
    );

    let password_env = env_vars
        .iter()
        .find(|e| e.name == "KANIDM_MAIL_PASSWORD")
        .unwrap();
    assert_eq!(
        password_env
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap()
            .key,
        "smtp-pass"
    );

    cleanup_mail_sender(&kanidm_api, KANIDM_NAME).await;
}

#[tokio::test]
async fn mail_sender_custom_image() {
    let name = "test-mail-sender-custom-image";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let custom_image = "custom-registry.example.com/kanidm-tools:v1.5.0";
    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name,
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        image: Some(custom_image.to_string()),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            KANIDM_NAME,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), KANIDM_NAME, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(KANIDM_NAME).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    let deployment_spec = deployment.spec.unwrap();
    let pod_spec = deployment_spec.template.spec.unwrap();
    let container = pod_spec.containers.first().unwrap();
    assert_eq!(container.image.as_deref().unwrap(), custom_image);

    cleanup_mail_sender(&kanidm_api, KANIDM_NAME).await;
}
