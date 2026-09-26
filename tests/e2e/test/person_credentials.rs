use super::{
    create_fresh_authenticated_client, poll_until, setup_kanidm_connection, stabilization_delay,
    wait_for,
};

use kaniop_person::crd::{CredentialBootstrapState, CredentialState, KanidmPersonAccount};

use kanidm_client::{ClientError, KanidmClient};
use kanidm_proto::internal::CURegState;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_authenticator_rs::softtoken::{self, SoftToken};
use webauthn_rs::prelude::AttestationCaListBuilder;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Event;
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::wait::Condition;
use kube::{Api, Client, ResourceExt};
use serde_json::json;

const KANIDM_NAME: &str = "test-person";

fn has_credential_state(state: CredentialState) -> impl Condition<KanidmPersonAccount> {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|person| person.status.as_ref())
            .is_some_and(|status| status.credential_state == state)
    }
}

fn has_bootstrap_state(state: CredentialBootstrapState) -> impl Condition<KanidmPersonAccount> {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|person| person.status.as_ref())
            .is_some_and(|status| status.credential_bootstrap.state == state)
    }
}

async fn force_person_reconcile(api: &Api<KanidmPersonAccount>, name: &str) {
    api.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(&json!({
            "metadata": {
                "annotations": {
                    "kaniop/e2e-force-credential-reconcile": Timestamp::now().to_string()
                }
            }
        })),
    )
    .await
    .unwrap();
}

async fn token_event_count(client: Client, namespace: &str, uid: &str) -> usize {
    let event_api = Api::<Event>::namespaced(client, namespace);
    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={uid}"
    ));
    event_api
        .list(&opts)
        .await
        .unwrap()
        .items
        .iter()
        .filter(|event| event.reason.as_deref() == Some("TokenCreated"))
        .count()
}

async fn create_person_cr_for_kanidm(
    api: &Api<KanidmPersonAccount>,
    name: &str,
    displayname: &str,
    kanidm_name: &str,
) -> String {
    let person_spec = json!({
        "kanidmRef": {"name": kanidm_name},
        "personAttributes": {"displayname": displayname},
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    api.create(&PostParams::default(), &person)
        .await
        .unwrap()
        .uid()
        .unwrap()
}

async fn create_person_cr(api: &Api<KanidmPersonAccount>, name: &str, displayname: &str) -> String {
    create_person_cr_for_kanidm(api, name, displayname, KANIDM_NAME).await
}

async fn create_existing_person(client: &KanidmClient, name: &str, displayname: &str) {
    client
        .idm_person_account_create(name, displayname)
        .await
        .unwrap();
}

async fn setup_password(client: &KanidmClient, name: &str, password: &str) {
    let intent = client
        .idm_person_account_credential_update_intent(name, Some(1234))
        .await
        .unwrap();
    let session_client = client.new_session().unwrap();
    let (session_token, _) = session_client
        .idm_account_credential_update_exchange(intent.token)
        .await
        .unwrap();
    session_client
        .idm_account_credential_update_set_password(&session_token, password)
        .await
        .unwrap();
    session_client
        .idm_account_credential_update_commit(&session_token)
        .await
        .unwrap();
}

async fn setup_passkey(client: &KanidmClient, name: &str) {
    let intent = client
        .idm_person_account_credential_update_intent(name, Some(1234))
        .await
        .unwrap();
    let session_client = client.new_session().unwrap();
    let (session_token, _) = session_client
        .idm_account_credential_update_exchange(intent.token)
        .await
        .unwrap();
    let status = session_client
        .idm_account_credential_update_passkey_init(&session_token)
        .await
        .unwrap();
    let challenge = match status.mfaregstate {
        CURegState::Passkey(challenge) => challenge,
        other => panic!("unexpected passkey registration state: {other:?}"),
    };
    let mut authenticator = SoftPasskey::new(true);
    let response = authenticator
        .do_registration(session_client.get_origin().clone(), challenge)
        .unwrap();
    session_client
        .idm_account_credential_update_passkey_finish(
            &session_token,
            "e2e-passkey".to_string(),
            response,
        )
        .await
        .unwrap();
    session_client
        .idm_account_credential_update_commit(&session_token)
        .await
        .unwrap();
}

async fn setup_attested_passkey(client: &KanidmClient, name: &str) -> String {
    let group = format!("{name}-attested-policy");
    client.idm_group_create(&group, None).await.unwrap();
    client.group_account_policy_enable(&group).await.unwrap();
    client.idm_group_add_members(&group, &[name]).await.unwrap();

    let (mut authenticator, ca_root) = SoftToken::new(true).unwrap();
    let mut builder = AttestationCaListBuilder::new();
    builder
        .insert_device_x509(
            ca_root,
            softtoken::AAGUID,
            "e2e-softtoken".to_string(),
            Default::default(),
        )
        .unwrap();
    let attestation = serde_json::to_string(&builder.build()).unwrap();
    client
        .group_account_policy_webauthn_attestation_set(&group, &attestation)
        .await
        .unwrap();

    let intent = client
        .idm_person_account_credential_update_intent(name, Some(1234))
        .await
        .unwrap();
    let session_client = client.new_session().unwrap();
    let (session_token, _) = session_client
        .idm_account_credential_update_exchange(intent.token)
        .await
        .unwrap();
    let status = session_client
        .idm_account_credential_update_attested_passkey_init(&session_token)
        .await
        .unwrap();
    let challenge = match status.mfaregstate {
        CURegState::AttestedPasskey(challenge) => challenge,
        other => panic!("unexpected attested passkey registration state: {other:?}"),
    };
    let response = authenticator
        .do_registration(session_client.get_origin().clone(), challenge)
        .unwrap();
    session_client
        .idm_account_credential_update_attested_passkey_finish(
            &session_token,
            "e2e-attested-passkey".to_string(),
            response,
        )
        .await
        .unwrap();
    session_client
        .idm_account_credential_update_commit(&session_token)
        .await
        .unwrap();

    group
}

async fn restart_operator(client: Client) {
    let deployments = Api::<Deployment>::namespaced(client, "kaniop");
    let deployment = deployments.get("kaniop").await.unwrap();
    let previous_generation = deployment.metadata.generation.unwrap_or_default();
    deployments
        .patch(
            "kaniop",
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "spec": {
                    "template": {
                        "metadata": {
                            "annotations": {
                                "kaniop/e2e-restarted-at": Timestamp::now().to_string()
                            }
                        }
                    }
                }
            })),
        )
        .await
        .unwrap();
    poll_until("operator deployment rollout", || {
        let deployments = deployments.clone();
        async move {
            let deployment = deployments.get("kaniop").await.ok()?;
            let status = deployment.status?;
            let generation = deployment.metadata.generation.unwrap_or_default();
            (generation > previous_generation
                && status.observed_generation.unwrap_or_default() >= generation
                && status.updated_replicas == status.replicas
                && status.available_replicas == status.replicas)
                .then_some(())
        }
    })
    .await;
}

async fn delete_person_cr(api: &Api<KanidmPersonAccount>, name: &str) {
    api.delete(name, &DeleteParams::default()).await.unwrap();
}

e2e_test!(person_credential_bootstrap_persists_state, {
    let name = "test-cred-bootstrap-state";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_cr(&person_api, name, "Credential Bootstrap").await;

    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Absent),
    )
    .await;
    wait_for(
        person_api.clone(),
        name,
        has_bootstrap_state(CredentialBootstrapState::TokenIssued),
    )
    .await;

    let status = person_api.get(name).await.unwrap().status.unwrap();
    let expiry = status
        .credential_bootstrap
        .token_expires_at
        .clone()
        .expect("token expiry must be persisted");
    assert_eq!(
        token_event_count(s.client.clone(), "default", &uid).await,
        1
    );

    for _ in 0..2 {
        force_person_reconcile(&person_api, name).await;
        tokio::time::sleep(stabilization_delay()).await;
    }

    let status = person_api.get(name).await.unwrap().status.unwrap();
    assert_eq!(
        status.credential_bootstrap.state,
        CredentialBootstrapState::TokenIssued
    );
    assert_eq!(status.credential_bootstrap.token_expires_at, Some(expiry));
    assert_eq!(
        token_event_count(s.client.clone(), "default", &uid).await,
        1
    );

    delete_person_cr(&person_api, name).await;
});

e2e_test!(
    person_existing_password_is_present_without_bootstrap_token,
    {
        let name = "test-cred-existing-password";
        let s = setup_kanidm_connection(KANIDM_NAME).await;
        create_existing_person(&s.kanidm_client, name, "Existing Password").await;
        setup_password(&s.kanidm_client, name, "e2e-test-password-123").await;

        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        let uid = create_person_cr(&person_api, name, "Existing Password").await;
        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Present),
        )
        .await;
        wait_for(
            person_api.clone(),
            name,
            has_bootstrap_state(CredentialBootstrapState::Complete),
        )
        .await;

        let status = person_api.get(name).await.unwrap().status.unwrap();
        assert!(status.credential_bootstrap.token_expires_at.is_none());
        assert_eq!(
            token_event_count(s.client.clone(), "default", &uid).await,
            0
        );

        delete_person_cr(&person_api, name).await;
    }
);

e2e_test!(
    person_existing_passkey_is_present_without_bootstrap_token,
    {
        let name = "test-cred-existing-passkey";
        let s = setup_kanidm_connection(KANIDM_NAME).await;
        create_existing_person(&s.kanidm_client, name, "Existing Passkey").await;
        setup_passkey(&s.kanidm_client, name).await;

        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        let uid = create_person_cr(&person_api, name, "Existing Passkey").await;
        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Present),
        )
        .await;
        wait_for(
            person_api.clone(),
            name,
            has_bootstrap_state(CredentialBootstrapState::Complete),
        )
        .await;

        assert_eq!(
            token_event_count(s.client.clone(), "default", &uid).await,
            0
        );

        delete_person_cr(&person_api, name).await;
    }
);

e2e_test!(
    person_existing_attested_passkey_is_present_without_bootstrap_token,
    {
        let name = "test-cred-existing-attested";
        let s = setup_kanidm_connection(KANIDM_NAME).await;
        create_existing_person(&s.kanidm_client, name, "Existing Attested Passkey").await;
        let policy_group = setup_attested_passkey(&s.kanidm_client, name).await;

        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        let uid = create_person_cr(&person_api, name, "Existing Attested Passkey").await;
        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Present),
        )
        .await;
        wait_for(
            person_api.clone(),
            name,
            has_bootstrap_state(CredentialBootstrapState::Complete),
        )
        .await;

        assert_eq!(
            token_event_count(s.client.clone(), "default", &uid).await,
            0
        );

        delete_person_cr(&person_api, name).await;
        s.kanidm_client
            .idm_group_delete(&policy_group)
            .await
            .unwrap();
    }
);

e2e_test!(
    person_reconcile_preserves_active_credential_update_session,
    {
        let name = "test-cred-active-session";
        let s = setup_kanidm_connection(KANIDM_NAME).await;
        create_existing_person(&s.kanidm_client, name, "Active Credential Session").await;
        setup_passkey(&s.kanidm_client, name).await;

        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        let uid = create_person_cr(&person_api, name, "Active Credential Session").await;
        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Present),
        )
        .await;
        assert_eq!(
            token_event_count(s.client.clone(), "default", &uid).await,
            0
        );

        let client = create_fresh_authenticated_client(KANIDM_NAME).await;
        let (session_token, _) = client
            .idm_account_credential_update_begin(name)
            .await
            .unwrap();

        force_person_reconcile(&person_api, name).await;
        tokio::time::sleep(stabilization_delay()).await;

        client
            .idm_account_credential_update_status(&session_token)
            .await
            .expect("reconciliation must not invalidate an active credential update session");
        let _: std::result::Result<(), ClientError> = client
            .perform_post_request("/v1/credential/_cancel", &session_token)
            .await;

        delete_person_cr(&person_api, name).await;
    }
);

e2e_test!(
    person_existing_credentials_survive_operator_restart_without_token,
    {
        let name = "test-cred-restart-present";
        let s = setup_kanidm_connection(KANIDM_NAME).await;
        create_existing_person(&s.kanidm_client, name, "Credential Restart Present").await;
        setup_passkey(&s.kanidm_client, name).await;

        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        let uid = create_person_cr(&person_api, name, "Credential Restart Present").await;
        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Present),
        )
        .await;
        wait_for(
            person_api.clone(),
            name,
            has_bootstrap_state(CredentialBootstrapState::Complete),
        )
        .await;
        assert_eq!(
            token_event_count(s.client.clone(), "default", &uid).await,
            0
        );

        restart_operator(s.client.clone()).await;
        force_person_reconcile(&person_api, name).await;
        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Present),
        )
        .await;
        tokio::time::sleep(stabilization_delay()).await;

        assert_eq!(
            token_event_count(s.client.clone(), "default", &uid).await,
            0
        );
        let status = person_api.get(name).await.unwrap().status.unwrap();
        assert_eq!(
            status.credential_bootstrap.state,
            CredentialBootstrapState::Complete
        );

        delete_person_cr(&person_api, name).await;
    }
);

e2e_test!(person_bootstrap_state_survives_operator_restart, {
    let name = "test-cred-restart-bootstrap";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_cr(&person_api, name, "Credential Restart Bootstrap").await;

    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Absent),
    )
    .await;
    wait_for(
        person_api.clone(),
        name,
        has_bootstrap_state(CredentialBootstrapState::TokenIssued),
    )
    .await;
    let expiry_before = person_api
        .get(name)
        .await
        .unwrap()
        .status
        .unwrap()
        .credential_bootstrap
        .token_expires_at
        .expect("bootstrap expiry must be persisted");
    assert_eq!(
        token_event_count(s.client.clone(), "default", &uid).await,
        1
    );

    restart_operator(s.client.clone()).await;
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_bootstrap_state(CredentialBootstrapState::TokenIssued),
    )
    .await;
    tokio::time::sleep(stabilization_delay()).await;

    let status_after = person_api.get(name).await.unwrap().status.unwrap();
    assert_eq!(status_after.credential_state, CredentialState::Absent);
    assert_eq!(
        status_after.credential_bootstrap.state,
        CredentialBootstrapState::TokenIssued
    );
    assert_eq!(
        status_after.credential_bootstrap.token_expires_at,
        Some(expiry_before)
    );
    assert_eq!(
        token_event_count(s.client.clone(), "default", &uid).await,
        1
    );

    delete_person_cr(&person_api, name).await;
});

e2e_test!(
    person_unreadable_credential_attributes_are_unknown_without_token,
    {
        const RESTRICTED_KANIDM_NAME: &str = "test-person-credential-unknown";
        let name = "test-cred-unreadable";
        let s = setup_kanidm_connection(RESTRICTED_KANIDM_NAME).await;
        create_existing_person(&s.kanidm_client, name, "Unreadable Credentials").await;

        s.kanidm_client
            .idm_group_add_members("idm_account_mail_read", &["idm_admins"])
            .await
            .unwrap();
        s.kanidm_client
            .idm_group_remove_members("idm_people_admins", &["idm_admins"])
            .await
            .unwrap();

        // Re-authenticate the operator after changing effective permissions.
        restart_operator(s.client.clone()).await;

        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        let uid = create_person_cr_for_kanidm(
            &person_api,
            name,
            "Unreadable Credentials",
            RESTRICTED_KANIDM_NAME,
        )
        .await;

        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Unknown),
        )
        .await;
        wait_for(
            person_api.clone(),
            name,
            has_bootstrap_state(CredentialBootstrapState::Pending),
        )
        .await;
        tokio::time::sleep(stabilization_delay()).await;

        let status = person_api.get(name).await.unwrap().status.unwrap();
        let credential_condition = status
            .conditions
            .as_ref()
            .unwrap()
            .iter()
            .find(|condition| condition.type_ == "Credential")
            .unwrap();
        assert_eq!(credential_condition.status, "Unknown");
        assert_eq!(
            token_event_count(s.client.clone(), "default", &uid).await,
            0
        );

        // Restore built-in ACL topology and make the account credentialed before cleanup.
        s.kanidm_client
            .idm_group_add_members("idm_people_admins", &["idm_admins"])
            .await
            .unwrap();
        s.kanidm_client
            .idm_group_remove_members("idm_account_mail_read", &["idm_admins"])
            .await
            .unwrap();
        setup_password(
            &s.kanidm_client,
            name,
            "e2e-unknown-cleanup-password-123",
        )
        .await;
        restart_operator(s.client.clone()).await;
        wait_for(
            person_api.clone(),
            name,
            has_credential_state(CredentialState::Present),
        )
        .await;

        delete_person_cr(&person_api, name).await;
    }
);
