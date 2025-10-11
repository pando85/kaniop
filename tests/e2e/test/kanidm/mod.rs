mod replication;

use crate::kanidm::get_dependency_version;
use crate::test::wait_for;

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::reconcile::secret::SecretExt;
use kaniop_operator::kanidm::reconcile::statefulset::StatefulSetExt;

use futures::future::JoinAll;
use futures::{AsyncBufReadExt, TryStreamExt, join};
use json_patch::merge;
use k8s_openapi::ByteString;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Secret};
use kube::ResourceExt;
use kube::api::{Api, LogParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::client::Client;
use kube::runtime::wait::{Condition, conditions};
use serde_json::json;

const CERT: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIIChDCCAiugAwIBAgIBAjAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDEwMTgyMDQz\nMjhaMIGAMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xGDAWBgNVBAMMD2lkbS5leGFtcGxlLmNvbTE4MDYGA1UECwwvRGV2ZWxvcG1l\nbnQgYW5kIEV2YWx1YXRpb24gLSBOT1QgRk9SIFBST0RVQ1RJT04wWTATBgcqhkjO\nPQIBBggqhkjOPQMBBwNCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bKfAAu4GmY8Qhf\nBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaIo4GPMIGMMAkGA1UdEwQC\nMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsGAQUFBwMBMB0GA1UdDgQW\nBBQaarHTRm4Yj6TMPzvduAB7nODKHzAfBgNVHSMEGDAWgBTaOaPuXmtLDTJVv++V\nYBiQr9gHCTAaBgNVHREEEzARgg9pZG0uZXhhbXBsZS5jb20wCgYIKoZIzj0EAwID\nRwAwRAIgQpLs9MZvBRUpR15wvSwIq/QyWotvVg/3vZl8D1mTFz8CIEVbm+/+z4JL\nLYwNXnerv9Nc+anGtz+9beT4bkS4CpJS\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICPjCCAeSgAwIBAgIBATAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDExMTIyMDQz\nMjhaMIGEMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xHDAaBgNVBAMME0thbmlkbSBHZW5lcmF0ZWQgQ0ExODA2BgNVBAsML0RldmVs\nb3BtZW50IGFuZCBFdmFsdWF0aW9uIC0gTk9UIEZPUiBQUk9EVUNUSU9OMFkwEwYH\nKoZIzj0CAQYIKoZIzj0DAQcDQgAEiz5mqHozpsj5iGCDH8uSJy8TFqNIGnIw8U/L\nswyeFTGHT4S2HwBb7QAouYVuXdwL8hZGMtzAqoYMFhCt1epXjqNFMEMwEgYDVR0T\nAQH/BAgwBgEB/wIBADAOBgNVHQ8BAf8EBAMCAQYwHQYDVR0OBBYEFNo5o+5ea0sN\nMlW/75VgGJCv2AcJMAoGCCqGSM49BAMCA0gAMEUCIGyZjBs4pp1HAlFdk0mdVBz4\n440t8pRHh8/SOY5ZtMcSAiEA6qOf9aQbWwEXLj0jajX9lHgdqlwRk7wnnyLMGF5/\nlz8=\n-----END CERTIFICATE-----\n";
const KEY: &[u8] = b"-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgHLZoMTUMadxOKMlt\nTq/kDnuN38GCJkwj8Y2kqyGlcf+hRANCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bK\nfAAu4GmY8QhfBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaI\n-----END PRIVATE KEY-----\n";

const DEFAULT_REPLICA_GROUP_NAME: &str = "default";
static KANIDM_DEFAULT_SPEC_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "domain": "idm.example.com",
        "image": format!("kanidm/server:{}", get_dependency_version().unwrap()),
        "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1}],
    })
});

const WAIT_FOR_REPLICATION_READY_SECONDS: u64 = 60;

static STORAGE_VOLUME_CLAIM_TEMPLATE_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "storage": {
            "volumeClaimTemplate": {
                "spec": {
                    "accessModes": [
                        "ReadWriteOnce"
                    ],
                    "resources": {
                        "requests": {
                            "storage": "1Gi"
                        },
                    }
                }
            }
        }
    })
});

fn check_kanidm_condition(cond: &str, status: String) -> impl Condition<Kanidm> + '_ {
    move |obj: Option<&Kanidm>| {
        obj.and_then(|kanidm| kanidm.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == cond && c.status == status)
            })
    }
}

pub fn is_kanidm(cond: &str) -> impl Condition<Kanidm> + '_ {
    check_kanidm_condition(cond, "True".to_string())
}

pub fn is_kanidm_false(cond: &str) -> impl Condition<Kanidm> + '_ {
    check_kanidm_condition(cond, "False".to_string())
}

fn is_statefulset_ready(obj: Option<&StatefulSet>) -> bool {
    obj.and_then(|statefulset| statefulset.status.as_ref())
        .is_some_and(|s| s.ready_replicas == Some(s.replicas))
}

async fn create_secret(client: &Client, name: &str) {
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");

    let mut data = BTreeMap::new();
    data.insert("tls.crt".to_string(), ByteString(CERT.to_vec()));
    data.insert("tls.key".to_string(), ByteString(KEY.to_vec()));

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(format!("{name}-tls")),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        data: Some(data),
        type_: Some("kubernetes.io/tls".to_string()),
        ..Default::default()
    };

    secret_api
        .create(&PostParams::default(), &secret)
        .await
        .unwrap();
}

async fn create_kanidm(
    client: &Client,
    name: &str,
    kanidm_spec_patch: Option<serde_json::Value>,
) -> (Kanidm, Api<Kanidm>) {
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    if let Some(patch) = kanidm_spec_patch {
        merge(&mut kanidm_spec_json, &patch);
    };

    let kanidm = Kanidm::new(name, serde_json::from_value(kanidm_spec_json).unwrap());

    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    kanidm_api
        .create(&PostParams::default(), &kanidm)
        .await
        .unwrap();
    (kanidm, kanidm_api)
}

fn validate_admin_passwords(admin_passwords: Secret) -> (String, String) {
    let admin_passwords_data = admin_passwords.data.clone().unwrap();
    assert_eq!(admin_passwords_data.len(), 4);

    let admin_password = String::from_utf8(
        admin_passwords_data
            .get("ADMIN_PASSWORD")
            .unwrap()
            .clone()
            .0,
    )
    .unwrap();
    let idm_admin_password = String::from_utf8(
        admin_passwords_data
            .get("IDM_ADMIN_PASSWORD")
            .unwrap()
            .clone()
            .0,
    )
    .unwrap();

    assert_eq!(admin_password.len(), 48);
    assert_eq!(idm_admin_password.len(), 48);
    assert!(admin_password.chars().all(char::is_alphanumeric));
    assert!(idm_admin_password.chars().all(char::is_alphanumeric));
    (admin_password, idm_admin_password)
}

pub struct SetupKanidm {
    pub client: Client,
    pub kanidm_api: Api<Kanidm>,
    pub statefulset_api: Api<StatefulSet>,
    pub secret_api: Api<Secret>,
    // TODO: remove this silent dead_code
    #[allow(dead_code)]
    pub admin_password: String,
    pub idm_admin_password: String,
}

pub async fn setup(name: &str, kanidm_spec_patch: Option<serde_json::Value>) -> SetupKanidm {
    let client = Client::try_default().await.unwrap();
    let (kanidm, kanidm_api) = create_kanidm(&client, name, kanidm_spec_patch).await;
    create_secret(&client, name).await;

    let statefulset_api = Api::<StatefulSet>::namespaced(client.clone(), "default");
    let sts_names_vec = kanidm
        .spec
        .replica_groups
        .iter()
        .map(|rg| kanidm.statefulset_name(&rg.name))
        .collect::<Vec<_>>();
    let sts_futures = sts_names_vec
        .iter()
        .map(|sts_name| wait_for(statefulset_api.clone(), sts_name, is_statefulset_ready))
        .collect::<JoinAll<_>>();
    join!(sts_futures);

    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_kanidm("Initialized")).await;

    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    let admin_passwords = secret_api
        .get(&format!("{name}-admin-passwords"))
        .await
        .unwrap();
    let (admin_password, idm_admin_password) = validate_admin_passwords(admin_passwords);

    SetupKanidm {
        client,
        kanidm_api,
        statefulset_api,
        secret_api,
        admin_password,
        idm_admin_password,
    }
}

#[tokio::test]
async fn kanidm_create() {
    let name = "test-create";
    setup(name, None).await;
}

#[tokio::test]
async fn kanidm_delete_statefulset() {
    let name = "test-delete-statefulset";
    let s = setup(name, None).await;

    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let sts = s.statefulset_api.get(&sts_name).await.unwrap();
    s.statefulset_api
        .delete(&sts_name, &Default::default())
        .await
        .unwrap();

    wait_for(
        s.statefulset_api.clone(),
        &sts_name,
        conditions::is_deleted(&sts.uid().unwrap()),
    )
    .await;
    wait_for(s.statefulset_api.clone(), &sts_name, is_statefulset_ready).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

    let check_sts_deleted = s.statefulset_api.get(&sts_name).await.unwrap();

    s.kanidm_api
        .delete(name, &Default::default())
        .await
        .unwrap();

    wait_for(
        s.statefulset_api,
        &sts_name,
        conditions::is_deleted(&check_sts_deleted.uid().unwrap()),
    )
    .await;
}

#[tokio::test]
async fn kanidm_delete_kanidm() {
    let name = "test-delete-kanidm";
    let s = setup(name, None).await;

    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let sts = s.statefulset_api.get(&sts_name).await.unwrap();
    let kanidm = s.kanidm_api.get(name).await.unwrap();
    s.kanidm_api
        .delete(name, &Default::default())
        .await
        .unwrap();

    wait_for(
        s.kanidm_api.clone(),
        name,
        conditions::is_deleted(&kanidm.uid().unwrap()),
    )
    .await;

    wait_for(
        s.statefulset_api.clone(),
        &sts_name,
        conditions::is_deleted(&sts.uid().unwrap()),
    )
    .await;
}

#[tokio::test]
async fn kanidm_change_statefulset() {
    let name = "test-change-statefulset";
    let s = setup(name, None).await;

    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let mut sts = s.statefulset_api.get(&sts_name).await.unwrap();
    sts.spec.as_mut().unwrap().replicas = Some(2);
    sts.metadata.managed_fields = None;
    sts.metadata.resource_version = None;
    sts.metadata.uid = None;
    sts.metadata.creation_timestamp = None;
    s.statefulset_api
        .patch(
            &sts_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&sts),
        )
        .await
        .unwrap();

    wait_for(
        s.statefulset_api.clone(),
        &sts_name,
        |obj: Option<&StatefulSet>| {
            obj.and_then(|statefulset| statefulset.status.as_ref())
                .is_some_and(|status| status.replicas == 2)
        },
    )
    .await;

    wait_for(s.statefulset_api.clone(), &sts_name, is_statefulset_ready).await;

    let check_sts_replica_0 = s.statefulset_api.get(&sts_name).await.unwrap();

    assert_eq!(check_sts_replica_0.spec.unwrap().replicas.unwrap(), 1);
}

#[tokio::test]
async fn kanidm_change_kanidm_replicas() {
    let name = "test-change-kanidm-replicas";
    let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups[0].replicas = 2;
    kanidm.spec.replica_groups[0].primary_node = true;
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, |obj: Option<&Kanidm>| {
        obj.and_then(|kanidm| kanidm.status.as_ref())
            .is_none_or(|status| status.updated_replicas == 2)
    })
    .await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let check_sts = s
        .statefulset_api
        .get(&format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"))
        .await
        .unwrap();

    assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 2);
    let sts_name = check_sts.name_any();
    // wait for restarts
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    // wait for restarts and readiness
    // TODO: replace with proper wait_for
    tokio::time::sleep(Duration::from_secs(WAIT_FOR_REPLICATION_READY_SECONDS)).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    for i in 0..2 {
        let pod_name = format!("{sts_name}-{i}");
        let secret_name = kanidm.replica_secret_name(&pod_name);
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        assert_eq!(secret.data.unwrap().len(), 1);
        let mut logs = pod_api
            .log_stream(&pod_name, &LogParams::default())
            .await
            .unwrap()
            .lines();
        let mut lines = Vec::new();
        while let Some(line) = logs.try_next().await.unwrap() {
            lines.push(line);
        }
        dbg!(&lines);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Incremental Replication Success"))
        );
    }
}

#[tokio::test]
async fn kanidm_statefulset_already_exists() {
    let name = "test-statefulset-already-exists";
    let statefulset = json!({
        "apiVersion": "apps/v1",
        "kind": "StatefulSet",
        "metadata": {
            "name": format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}")
        },
        "spec": {
            "replicas": 1,
            "selector": {
                "matchLabels": {
                    "app": name
                }
            },
            "template": {
                "metadata": {
                    "labels": {
                        "app": name
                    }
                },
                "spec": {
                    "containers": [
                        {
                            "name": name,
                            "image": "kanidm/server:latest"
                        }
                    ]
                }
            }
        }
    });
    let statefulset_api =
        Api::<StatefulSet>::namespaced(Client::try_default().await.unwrap(), "default");
    statefulset_api
        .create(
            &PostParams::default(),
            &serde_json::from_value(statefulset).unwrap(),
        )
        .await
        .unwrap();

    setup(name, None).await;
}

#[tokio::test]
async fn kanidm_change_domain() {
    let name = "test-change-kanidm-domain";
    let s = setup(name, None).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.domain = "changed.example.com".to_string();
    kanidm.metadata.managed_fields = None;
    let result = s
        .kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Domain cannot be changed.")
    );
}

#[tokio::test]
async fn kanidm_donwscale_to_zero() {
    let name = "test-downscale-to-zero";
    let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;
    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups[0].replicas = 0;
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    let sts_name = kanidm.statefulset_name(&kanidm.spec.replica_groups.first().unwrap().name);
    let sts = s.statefulset_api.get(&sts_name).await.unwrap();
    let kanidm = s.kanidm_api.get(name).await.unwrap();
    s.kanidm_api
        .delete(name, &Default::default())
        .await
        .unwrap();

    wait_for(
        s.kanidm_api.clone(),
        name,
        conditions::is_deleted(&kanidm.uid().unwrap()),
    )
    .await;

    wait_for(
        s.statefulset_api.clone(),
        &sts_name,
        conditions::is_deleted(&sts.uid().unwrap()),
    )
    .await;

    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(s.client.clone(), "default");
    pvc_api
        .get(&format!(
            "kanidm-data-{name}-{DEFAULT_REPLICA_GROUP_NAME}-0"
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn kanidm_recreate_admin_passwords() {
    let name = "test-recreate-admin-passwords";
    let s = setup(name, None).await;

    let secret_name = format!("{name}-admin-passwords");
    let secret = s.secret_api.get(&secret_name).await.unwrap();
    s.secret_api
        .delete(&secret_name, &Default::default())
        .await
        .unwrap();
    wait_for(
        s.secret_api.clone(),
        &secret_name,
        conditions::is_deleted(&secret.uid().unwrap()),
    )
    .await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Initialized")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Initialized")).await;
    let new_secret = s.secret_api.get(&secret_name).await.unwrap();
    validate_admin_passwords(new_secret);
}

#[tokio::test]
async fn kanidm_invalid_chars_on_name() {
    let client = Client::try_default().await.unwrap();

    let kanidm = Kanidm::new(
        "test-invalid.name",
        serde_json::from_value(KANIDM_DEFAULT_SPEC_JSON.clone()).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid name. Only lowercase alphanumeric characters and '-' are allowed.")
    );
}

#[tokio::test]
async fn kanidm_invalid_long_names() {
    let client = Client::try_default().await.unwrap();

    let kanidm = Kanidm::new(
        "test-invalid-too-long-name-above-63-characters-for-sure",
        serde_json::from_value(KANIDM_DEFAULT_SPEC_JSON.clone()).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains(
        "Invalid name. Too long name, subresource names must no more than 63 characters."
    ));

    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch = json!({
        "replicaGroups": [{"name": "both-names-together-are-more-than-61", "replicas": 1}],
    });

    merge(&mut kanidm_spec_json, &patch);
    let kanidm = Kanidm::new(
        "test-invalid-too-long-name-with-rg",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains(
        "Invalid name. Too long name, subresource names must no more than 63 characters."
    ));
}

#[tokio::test]
async fn kanidm_renew_certificates() {
    let name = "test-renew-certificates";
    let replicas = 2;
    let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups[0].replicas = replicas;
    kanidm.spec.replica_groups[0].primary_node = true;
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, |obj: Option<&Kanidm>| {
        obj.and_then(|kanidm| kanidm.status.as_ref())
            .is_none_or(|status| status.updated_replicas == 2)
    })
    .await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let check_sts = s
        .statefulset_api
        .get(&format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"))
        .await
        .unwrap();

    assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 2);
    let sts_name = check_sts.name_any();
    // wait for restarts
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    // wait for restarts and readiness
    // TODO: replace with proper wait_for
    tokio::time::sleep(Duration::from_secs(WAIT_FOR_REPLICATION_READY_SECONDS)).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    for i in 0..replicas {
        let pod_name = format!("{sts_name}-{i}");
        let secret_name = kanidm.replica_secret_name(&pod_name);
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        assert_eq!(secret.data.unwrap().len(), 1);
        let mut logs = pod_api
            .log_stream(&pod_name, &LogParams::default())
            .await
            .unwrap()
            .lines();
        let mut lines = Vec::new();
        while let Some(line) = logs.try_next().await.unwrap() {
            lines.push(line);
        }
        dbg!(&lines);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Incremental Replication Success"))
        );
    }

    // patch secret to trigger certificate renewal
    for i in 0..replicas {
        let pod_name = format!("{sts_name}-{i}");
        let secret_name = kanidm.replica_secret_name(&pod_name);
        let mut secret = s.secret_api.get(&secret_name).await.unwrap();
        let data = secret.data.as_mut().unwrap();
        // Change secret for an expired certificate
        data.insert("tls.der.b64url".to_string(), ByteString(b"MIICAzCCAamgAwIBAgIUabYGR1vKncj22sN2DpTmWocmfuswCgYIKoZIzj0EAwIwTDEbMBkGA1UECgwSS2FuaWRtIFJlcGxpY2F0aW9uMS0wKwYDVQQDDCQyYmE4MzE2YS1lYmFhLTRiYzEtODQ5My01Zjg2ZmFmYWU1OTQwHhcNMjUxMDEyMTExOTQxWhcNMjUxMDEzMTExOTQxWjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABKPMz0fox2HAsE8PM2hT0aWV8r7sIa3v6R6azORc4HMzs6JilLacJVfMm97Kerzcdx6VlTaQaapScFkGQNVfGv6jaTBnMB0GA1UdDgQWBBSqpOBYyTNyBhQRIAe9UvjqJZ3_nDAfBgNVHSMEGDAWgBSqpOBYyTNyBhQRIAe9UvjqJZ3_nDAPBgNVHRMBAf8EBTADAQH_MBQGA1UdEQQNMAuCCWxvY2FsaG9zdDAKBggqhkjOPQQDAgNIADBFAiEA7_2p0-7uMsT02kOX5u0Bd32u6691fo9071QfZdvcVgcCIC-noe1886tavYc3xYd_nZWIsM4HM2CM33gXggYgVwgw".to_vec()));
        secret.metadata.managed_fields = None;
        s.secret_api
            .patch(
                &secret_name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&secret),
            )
            .await
            .unwrap();
    }

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    // wait for restarts and readiness
    // TODO: replace with proper wait_for
    tokio::time::sleep(Duration::from_secs(WAIT_FOR_REPLICATION_READY_SECONDS)).await;

    for i in 0..replicas {
        let pod_name = format!("{sts_name}-{i}");
        let secret_name = kanidm.replica_secret_name(&pod_name);
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        assert_eq!(secret.data.unwrap().len(), 1);
        let mut logs = pod_api
            .log_stream(&pod_name, &LogParams::default())
            .await
            .unwrap()
            .lines();
        let mut lines = Vec::new();
        while let Some(line) = logs.try_next().await.unwrap() {
            lines.push(line);
        }
        dbg!(&lines);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Incremental Replication Success"))
        );
    }
}
