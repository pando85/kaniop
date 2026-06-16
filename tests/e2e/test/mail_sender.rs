use super::{kanidm::is_kanidm, poll_until, setup_kanidm_connection, wait_for};

use backon::{ExponentialBuilder, Retryable};
use kaniop_operator::kanidm::crd::{Kanidm, MailSenderSpec, MailSenderStatus};
use kaniop_operator::kanidm::reconcile::mail_sender::{
    mail_sender_config_map_name, mail_sender_deployment_name, mail_sender_service_account_name,
};

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{DeleteParams, ObjectMeta, Patch, PatchParams};
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
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&secret),
        )
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
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&secret),
        )
        .await
        .unwrap();
}

async fn cleanup_smtp_secret(client: &Client, name: &str) {
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    match secret_api.delete(name, &DeleteParams::default()).await {
        Ok(_) => {}
        Err(kube::Error::Api(e)) if e.code == 404 => {}
        Err(e) => panic!("failed to delete SMTP credentials secret {name}: {e}"),
    }
}

async fn cleanup_mail_sender(
    client: &Client,
    kanidm_api: &Api<Kanidm>,
    kanidm_name: &str,
    smtp_secret_name: &str,
) {
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::default(),
            &Patch::Merge(json!({"spec": {"mailSender": null}})),
        )
        .await
        .unwrap();

    poll_until("mail sender status to be removed", || async {
        kanidm_api
            .get(kanidm_name)
            .await
            .ok()
            .and_then(|k| k.status)
            .and_then(|s| s.mail_sender)
            .is_none()
            .then_some(())
    })
    .await;

    cleanup_smtp_secret(client, smtp_secret_name).await;
}

e2e_test!(mail_sender_create, {
    let name = "test-mail-sender-create";
    let s = setup_kanidm_connection(name).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(name).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name.clone(),
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    assert!(mail_sender_status.ready);
    assert_eq!(
        mail_sender_status.service_account_name,
        mail_sender_service_account_name(name)
    );
    assert_eq!(
        mail_sender_status.token_secret_name,
        mail_sender_config_map_name(name)
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

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let config_secret = secret_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    assert!(config_secret.string_data.is_some() || config_secret.data.is_some());

    let service_account_name = mail_sender_status.service_account_name;
    let kanidm_sa = s
        .kanidm_client
        .idm_service_account_get(&service_account_name)
        .await
        .unwrap();
    assert!(kanidm_sa.is_some());

    cleanup_mail_sender(&s.client, &kanidm_api, name, &smtp_secret_name).await;
});

e2e_test!(mail_sender_disable, {
    let name = "test-mail-sender-disable";
    let s = setup_kanidm_connection(name).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(name).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name.clone(),
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let deployment_api = Api::<Deployment>::namespaced(s.client.clone(), "default");
    let deployment = deployment_api
        .get(&mail_sender_status.deployment_name)
        .await
        .unwrap();
    let deployment_uid = deployment.uid().unwrap();

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let config_secret = secret_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    let config_secret_uid = config_secret.uid().unwrap();

    cleanup_mail_sender(&s.client, &kanidm_api, name, &smtp_secret_name).await;

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;

    wait_for(
        deployment_api.clone(),
        &mail_sender_status.deployment_name,
        conditions::is_deleted(&deployment_uid),
    )
    .await;

    wait_for(
        secret_api.clone(),
        &mail_sender_status.config_map_name,
        conditions::is_deleted(&config_secret_uid),
    )
    .await;
});

e2e_test!(mail_sender_update, {
    let name = "test-mail-sender-update";
    let s = setup_kanidm_connection(name).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let kanidm_api_clone = kanidm_api.clone();
    let smtp_secret_name_clone = smtp_secret_name.clone();
    let retryable_patch = || async {
        let kanidm = kanidm_api_clone.get(name).await?;
        let mut patch_kanidm = kanidm.clone();
        patch_kanidm.spec.mail_sender = Some(MailSenderSpec {
            relay: "smtps://smtp.example.com".to_string(),
            credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
                name: smtp_secret_name_clone.clone(),
                ..Default::default()
            },
            from_address: "kanidm@example.com".to_string(),
            reply_to_address: Some("admin@example.com".to_string()),
            ..Default::default()
        });
        patch_kanidm.metadata.managed_fields = None;
        kanidm_api_clone
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&patch_kanidm),
            )
            .await
    };
    retryable_patch
        .retry(ExponentialBuilder::default().with_max_times(5))
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let config_secret = secret_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    let mail_config = config_secret
        .string_data
        .as_ref()
        .and_then(|d| d.get("mail-sender.toml").cloned())
        .or_else(|| {
            config_secret
                .data
                .as_ref()
                .and_then(|d| d.get("mail-sender.toml"))
                .map(|v| String::from_utf8_lossy(&v.0).to_string())
        })
        .unwrap();
    assert!(mail_config.contains("mail_reply_to_address = \"admin@example.com\""));

    let kanidm_api_clone = kanidm_api.clone();
    let smtp_secret_name_clone = smtp_secret_name.clone();
    let retryable_update = || async {
        let kanidm = kanidm_api_clone.get(name).await?;
        let mut patch_kanidm = kanidm.clone();
        patch_kanidm.spec.mail_sender = Some(MailSenderSpec {
            relay: "smtp://smtp-new.example.com:587".to_string(),
            credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
                name: smtp_secret_name_clone.clone(),
                ..Default::default()
            },
            from_address: "updated@example.com".to_string(),
            queue_poll_interval_seconds: Some(10),
            ..Default::default()
        });
        patch_kanidm.metadata.managed_fields = None;
        kanidm_api_clone
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&patch_kanidm),
            )
            .await
    };
    retryable_update
        .retry(ExponentialBuilder::default().with_max_times(5))
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let config_secret_name = mail_sender_status.config_map_name.clone();
    let updated_mail_config = poll_until("mail sender config to be updated", || {
        let secret_api = secret_api.clone();
        let config_secret_name = config_secret_name.clone();
        async move {
            let updated_secret = match secret_api.get(&config_secret_name).await {
                Ok(s) => s,
                Err(kube::Error::Api(e)) if e.code == 404 => return None,
                Err(e) => panic!("failed to get config secret: {e}"),
            };
            let config = updated_secret
                .string_data
                .as_ref()
                .and_then(|d| d.get("mail-sender.toml").cloned())
                .or_else(|| {
                    updated_secret
                        .data
                        .as_ref()
                        .and_then(|d| d.get("mail-sender.toml"))
                        .map(|v| String::from_utf8_lossy(&v.0).to_string())
                })
                .unwrap();
            config
                .contains("mail_from_address = \"updated@example.com\"")
                .then_some(config)
        }
    })
    .await;
    assert!(updated_mail_config.contains("mail_relay = \"smtp-new.example.com:587\""));
    assert!(updated_mail_config.contains("mail_from_address = \"updated@example.com\""));
    assert!(updated_mail_config.contains("mail_reply_to_address = \"updated@example.com\""));
    assert!(updated_mail_config.contains("schedule = \"0 */10 * * * *\""));

    cleanup_mail_sender(&s.client, &kanidm_api, name, &smtp_secret_name).await;
});

e2e_test!(mail_sender_custom_keys, {
    let name = "test-mail-sender-custom-keys";
    let s = setup_kanidm_connection(name).await;

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
    let mut kanidm = kanidm_api.get(name).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name.clone(),
            username_key: "smtp-user".to_string(),
            password_key: "smtp-pass".to_string(),
        },
        from_address: "kanidm@example.com".to_string(),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let config_secret = secret_api
        .get(&mail_sender_status.config_map_name)
        .await
        .unwrap();
    let mail_config = config_secret
        .string_data
        .as_ref()
        .and_then(|d| d.get("mail-sender.toml").cloned())
        .or_else(|| {
            config_secret
                .data
                .as_ref()
                .and_then(|d| d.get("mail-sender.toml"))
                .map(|v| String::from_utf8_lossy(&v.0).to_string())
        })
        .unwrap();
    assert!(mail_config.contains("mail_username = \"custom-user\""));
    assert!(mail_config.contains("mail_password = \"custom-password\""));

    cleanup_mail_sender(&s.client, &kanidm_api, name, &smtp_secret_name).await;
});

e2e_test!(mail_sender_custom_image, {
    let name = "test-mail-sender-custom-image";
    let s = setup_kanidm_connection(name).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let custom_image = "kanidm/tools:1.10.0";
    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(name).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name.clone(),
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        image: Some(custom_image.to_string()),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
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

    cleanup_mail_sender(&s.client, &kanidm_api, name, &smtp_secret_name).await;
});

e2e_test!(mail_sender_idempotent_reconcile, {
    let name = "test-mail-sender-idempotent";
    let s = setup_kanidm_connection(name).await;

    let smtp_secret_name = format!("{name}-smtp-credentials");
    create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(name).await.unwrap();
    kanidm.spec.mail_sender = Some(MailSenderSpec {
        relay: "smtps://smtp.example.com".to_string(),
        credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
            name: smtp_secret_name.clone(),
            ..Default::default()
        },
        from_address: "kanidm@example.com".to_string(),
        ..Default::default()
    });
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();
    let original_token_id = mail_sender_status.token_id.clone();
    let service_account_name = mail_sender_status.service_account_name.clone();

    assert!(original_token_id.is_some());

    let kanidm_sa = s
        .kanidm_client
        .idm_service_account_get(&service_account_name)
        .await
        .unwrap();
    assert!(kanidm_sa.is_some());

    let kanidm_api_clone = kanidm_api.clone();
    let smtp_secret_name_clone = smtp_secret_name.clone();
    let retryable_patch = || async {
        let kanidm = kanidm_api_clone.get(name).await?;
        let mut patch_kanidm = kanidm.clone();
        patch_kanidm.spec.mail_sender = Some(MailSenderSpec {
            relay: "smtps://smtp.example.com".to_string(),
            credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
                name: smtp_secret_name_clone.clone(),
                ..Default::default()
            },
            from_address: "kanidm@example.com".to_string(),
            queue_poll_interval_seconds: Some(30),
            ..Default::default()
        });
        patch_kanidm.metadata.managed_fields = None;
        kanidm_api_clone
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&patch_kanidm),
            )
            .await
    };
    retryable_patch
        .retry(ExponentialBuilder::default().with_max_times(5))
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_mail_sender_ready()).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
    let mail_sender_status = get_mail_sender_status(&kanidm).unwrap();
    let updated_token_id = mail_sender_status.token_id.clone();

    assert_eq!(original_token_id, updated_token_id);

    let kanidm_sa = s
        .kanidm_client
        .idm_service_account_get(&service_account_name)
        .await
        .unwrap();
    assert!(kanidm_sa.is_some());

    cleanup_mail_sender(&s.client, &kanidm_api, name, &smtp_secret_name).await;
});
