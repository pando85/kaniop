use super::{
    kanidm::{is_kanidm, is_kanidm_false, setup},
    poll_until, wait_for,
};

use kaniop_operator::kanidm::crd::{Kanidm, MailSenderStatus};
use kaniop_operator::kanidm::reconcile::mail_sender::{
    mail_sender_config_map_name, mail_sender_deployment_name, mail_sender_service_account_name,
    mail_sender_token_secret_name,
};

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::{DeleteParams, ListParams, Patch, PatchParams};
use kube::{Api, Client, ResourceExt};
use kube::runtime::{conditions, wait::Condition};
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

#[tokio::test]
async fn mail_sender_create() {
    let name = "test-mail-sender-create";
    let client = Client::try_default().await.unwrap();

    let smtp_secret_name = format!("{name}-smtp-credentials");
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    let smtp_secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": smtp_secret_name,
        },
        "stringData": {
            "username": "smtp-user",
            "password": "smtp-password",
        },
    });
    secret_api
        .create(
            &PatchParams::default(),
            &Patch::Apply(&serde_json::from_value::<Secret>(smtp_secret).unwrap()),
        )
        .await
        .unwrap();

    let kanidm_spec = json!({
        "mailSender": {
            "relay": "smtps://smtp.example.com",
            "credentialsSecret": {
                "name": smtp_secret_name,
            },
            "fromAddress": "kanidm@example.com",
        },
    });

    let s = setup(name, Some(kanidm_spec)).await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = s.kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    assert!(mail_sender_status.ready);
    assert_eq!(
        mail_sender_status.service_account_name,
        mail_sender_service_account_name(name)
    );
    assert_eq!(
        mail_sender_status.token_secret_name,
        mail_sender_token_secret_name(name)
    );
    assert_eq!(
        mail_sender_status.deployment_name,
        mail_sender_deployment_name(name)
    );
    assert_eq!(
        mail_sender_status.config_map_name,
        mail_sender_config_map_name(name)
    );

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    assert_eq!(deployment.spec.unwrap().replicas.unwrap(), 1);

    let token_secret = s
        .secret_api
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
    let kanidm_sa_result = s
        .kanidm_client
        .idm_service_account_get(&service_account_name)
        .await;
    assert!(kanidm_sa_result.is_ok_and(|r| r.is_some()));

    s.kanidm_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    let kanidm_uid = kanidm.uid().unwrap();
    wait_for(
        s.kanidm_api.clone(),
        name,
        conditions::is_deleted(&kanidm_uid),
    )
    .await;

    wait_for(
        deployment_api.clone(),
        &mail_sender_status.deployment_name,
        conditions::is_deleted(&deployment.uid().unwrap()),
    )
    .await;

    wait_for(
        s.secret_api.clone(),
        &mail_sender_status.token_secret_name,
        conditions::is_deleted(&token_secret.uid().unwrap()),
    )
    .await;

    wait_for(
        config_map_api.clone(),
        &mail_sender_status.config_map_name,
        conditions::is_deleted(&config_map.uid().unwrap()),
    )
    .await;
}

#[tokio::test]
async fn mail_sender_disable() {
    let name = "test-mail-sender-disable";
    let client = Client::try_default().await.unwrap();

    let smtp_secret_name = format!("{name}-smtp-credentials");
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    let smtp_secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": smtp_secret_name,
        },
        "stringData": {
            "username": "smtp-user",
            "password": "smtp-password",
        },
    });
    secret_api
        .create(
            &PatchParams::default(),
            &Patch::Apply(&serde_json::from_value::<Secret>(smtp_secret).unwrap()),
        )
        .await
        .unwrap();

    let kanidm_spec = json!({
        "mailSender": {
            "relay": "smtps://smtp.example.com",
            "credentialsSecret": {
                "name": smtp_secret_name,
            },
            "fromAddress": "kanidm@example.com",
        },
    });

    let s = setup(name, Some(kanidm_spec)).await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = s.kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    let deployment_uid = deployment.uid().unwrap();

    let token_secret = s
        .secret_api
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

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.mail_sender = None;
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

    poll_until("mail sender status to be removed", || async {
        s.kanidm_api
            .get(name)
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
        s.secret_api.clone(),
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

    s.kanidm_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn mail_sender_update() {
    let name = "test-mail-sender-update";
    let client = Client::try_default().await.unwrap();

    let smtp_secret_name = format!("{name}-smtp-credentials");
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    let smtp_secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": smtp_secret_name,
        },
        "stringData": {
            "username": "smtp-user",
            "password": "smtp-password",
        },
    });
    secret_api
        .create(
            &PatchParams::default(),
            &Patch::Apply(&serde_json::from_value::<Secret>(smtp_secret).unwrap()),
        )
        .await
        .unwrap();

    let kanidm_spec = json!({
        "mailSender": {
            "relay": "smtps://smtp.example.com",
            "credentialsSecret": {
                "name": smtp_secret_name,
            },
            "fromAddress": "kanidm@example.com",
            "replyToAddress": "admin@example.com",
        },
    });

    let s = setup(name, Some(kanidm_spec)).await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = s.kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let config_map_api = Api::<ConfigMap>::namespaced(s.client.clone(), "default");
    let config_map = config_map_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    let config_data = config_map.data.unwrap();
    let server_toml = config_data.get("server.toml").unwrap();
    assert!(server_toml.contains("reply_to_address = \"admin@example.com\""));

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.mail_sender = Some(serde_json::from_value(json!({
        "relay": "smtp://smtp-new.example.com:587",
        "credentialsSecret": {
            "name": smtp_secret_name,
        },
        "fromAddress": "updated@example.com",
        "queuePollIntervalSeconds": 10,
    }))
    .unwrap());
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_mail_sender_ready()).await;

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

    s.kanidm_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn mail_sender_custom_keys() {
    let name = "test-mail-sender-custom-keys";
    let client = Client::try_default().await.unwrap();

    let smtp_secret_name = format!("{name}-smtp-credentials");
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    let smtp_secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": smtp_secret_name,
        },
        "stringData": {
            "smtp-user": "custom-user",
            "smtp-pass": "custom-password",
        },
    });
    secret_api
        .create(
            &PatchParams::default(),
            &Patch::Apply(&serde_json::from_value::<Secret>(smtp_secret).unwrap()),
        )
        .await
        .unwrap();

    let kanidm_spec = json!({
        "mailSender": {
            "relay": "smtps://smtp.example.com",
            "credentialsSecret": {
                "name": smtp_secret_name,
                "usernameKey": "smtp-user",
                "passwordKey": "smtp-pass",
            },
            "fromAddress": "kanidm@example.com",
        },
    });

    let s = setup(name, Some(kanidm_spec)).await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let kanidm = s.kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    let container = deployment
        .spec
        .unwrap()
        .template
        .spec
        .unwrap()
        .containers
        .first()
        .unwrap();
    let env_vars = container.env.as_ref().unwrap();

    let username_env = env_vars.iter().find(|e| e.name == "KANIDM_MAIL_USERNAME").unwrap();
    assert_eq!(
        username_env.value_from.as_ref().unwrap().secret_key_ref.as_ref().unwrap().key,
        "smtp-user"
    );

    let password_env = env_vars.iter().find(|e| e.name == "KANIDM_MAIL_PASSWORD").unwrap();
    assert_eq!(
        password_env.value_from.as_ref().unwrap().secret_key_ref.as_ref().unwrap().key,
        "smtp-pass"
    );

    s.kanidm_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn mail_sender_missing_credentials_secret() {
    let name = "test-mail-sender-missing-secret";
    let client = Client::try_default().await.unwrap();

    let kanidm_spec = json!({
        "mailSender": {
            "relay": "smtps://smtp.example.com",
            "credentialsSecret": {
                "name": "non-existent-secret",
            },
            "fromAddress": "kanidm@example.com",
        },
    });

    let s = setup(name, Some(kanidm_spec)).await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    poll_until("mail sender status to not be ready", || async {
        s.kanidm_api
            .get(name)
            .await
            .ok()
            .and_then(|k| k.status)
            .and_then(|s| s.mail_sender)
            .is_some_and(|ms| !ms.ready)
            .then_some(())
    })
    .await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=Kanidm,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
    ));
    let event_api = Api::<k8s_openapi::api::core::v1::Event>::namespaced(client.clone(), "default");
    let event_list = event_api.list(&opts).await.unwrap();

    assert!(event_list.items.iter().any(|e| {
        e.reason.as_deref().unwrap_or("") == "Error"
            || e.message.as_deref().unwrap_or("").contains("non-existent-secret")
    }));

    s.kanidm_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn mail_sender_custom_image() {
    let name = "test-mail-sender-custom-image";
    let client = Client::try_default().await.unwrap();

    let smtp_secret_name = format!("{name}-smtp-credentials");
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    let smtp_secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": smtp_secret_name,
        },
        "stringData": {
            "username": "smtp-user",
            "password": "smtp-password",
        },
    });
    secret_api
        .create(
            &PatchParams::default(),
            &Patch::Apply(&serde_json::from_value::<Secret>(smtp_secret).unwrap()),
        )
        .await
        .unwrap();

    let custom_image = "custom-registry.example.com/kanidm-tools:v1.5.0";
    let kanidm_spec = json!({
        "mailSender": {
            "relay": "smtps://smtp.example.com",
            "credentialsSecret": {
                "name": smtp_secret_name,
            },
            "fromAddress": "kanidm@example.com",
            "image": custom_image,
        },
    });

    let s = setup(name, Some(kanidm_spec)).await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let kanidm = s.kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    let container = deployment
        .spec
        .unwrap()
        .template
        .spec
        .unwrap()
        .containers
        .first()
        .unwrap();
    assert_eq!(container.image.as_deref().unwrap(), custom_image);

    s.kanidm_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
}