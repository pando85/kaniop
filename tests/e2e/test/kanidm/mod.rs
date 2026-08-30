mod backup;
mod backup_transport;
mod domain_appearance;
mod replication;
mod restore;
mod upgrade;

pub struct UploadOptions<'a> {
    pub kanidm_name: &'a str,
    pub prefix: &'a str,
    pub backup_name: &'a str,
    pub backup_id: &'a str,
    pub kanidm_uid: &'a str,
    pub domain: &'a str,
    pub encryption: Option<&'a str>,
    pub extra_operation_fields: Option<serde_json::Value>,
}

use crate::kanidm::get_dependency_version;
use crate::test::{init_crypto_provider, wait_for};

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use kaniop_backup_core::crd::KanidmBackupRepository;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::reconcile::secret::SecretExt;
use kaniop_operator::kanidm::reconcile::statefulset::{StatefulSetExt, TLS_SECRET_HASH_ANNOTATION};

use futures::future::JoinAll;
use futures::join;
use json_patch::merge;
use k8s_openapi::ByteString;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Secret, Service};
use kube::ResourceExt;
use kube::api::{Api, LogParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::client::Client;
use kube::runtime::wait::{Condition, conditions};
use serde_json::json;
use tokio::time::{Instant, sleep};

const CERT: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIICGjCCAb+gAwIBAgIUHpT08nqX951u//GR+v8XT79r9SUwCgYIKoZIzj0EAwIw\nRDELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRYw\nFAYDVQQDDA1LYW5pb3AgRTJFIENBMB4XDTI2MDgwODA2MTkxOVoXDTM2MDgwNTA2\nMTkxOVowRjELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2Fu\naWRtMRgwFgYDVQQDDA9pZG0uZXhhbXBsZS5jb20wWTATBgcqhkjOPQIBBggqhkjO\nPQMBBwNCAAQvppDjypVndfeojNUQ4o1r0v/+ry6an9tRRgdaqpAWycCsHHwqzxRG\nvQmGifZQ5dsBle7+3df8YBfXmikDRTEeo4GMMIGJMAkGA1UdEwQCMAAwCwYDVR0P\nBAQDAgWgMBMGA1UdJQQMMAoGCCsGAQUFBwMBMBoGA1UdEQQTMBGCD2lkbS5leGFt\ncGxlLmNvbTAdBgNVHQ4EFgQU08vzk3TPxjTZYSarIJ/X8483q5MwHwYDVR0jBBgw\nFoAU/oFjdY0iaHDwDEsG9K2kLqnKaCswCgYIKoZIzj0EAwIDSQAwRgIhAOAaimcS\nz/IUkI03CYbicyGIQDmXBruN584Uk0wLmOxBAiEA1T6y7HbX3F1oyftd5wABZPDB\nCpREB0kqGwMUURezf4w=\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIIB3DCCAYOgAwIBAgIUfsv6cZIgDmqN1h4xCuD9CjVRLkgwCgYIKoZIzj0EAwIw\nRDELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRYw\nFAYDVQQDDA1LYW5pb3AgRTJFIENBMB4XDTI2MDgwODA2MTkxOVoXDTM2MDgwNTA2\nMTkxOVowRDELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2Fu\naWRtMRYwFAYDVQQDDA1LYW5pb3AgRTJFIENBMFkwEwYHKoZIzj0CAQYIKoZIzj0D\nAQcDQgAEy84lnsJddCODwnayK4yoqLf6jVGTWIT0mpUh01Ghoq8GrXSrvYGIjxZ0\nYFPEwstiso8GJP15JKXzoGJTUs4a6aNTMFEwHQYDVR0OBBYEFP6BY3WNImhw8AxL\nBvStpC6pymgrMB8GA1UdIwQYMBaAFP6BY3WNImhw8AxLBvStpC6pymgrMA8GA1Ud\nEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDRwAwRAIgLqbXmVvrEP9zjuMcU0j+R79Z\nFzsMIBS59ZhCJVTa3NACIG2rT7suWcwoc2Wkv7y0AWdpRoZcpLwL0kGNzN5yidHS\n-----END CERTIFICATE-----\n";
const KEY: &[u8] = b"-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg1H0PlmChG8z25SO9\nuhAEmbDIisdcSmYzbWotQL+sb5WhRANCAAQvppDjypVndfeojNUQ4o1r0v/+ry6a\nn9tRRgdaqpAWycCsHHwqzxRGvQmGifZQ5dsBle7+3df8YBfXmikDRTEe\n-----END PRIVATE KEY-----\n";

const DEFAULT_REPLICA_GROUP_NAME: &str = "default";
static KANIDM_DEFAULT_SPEC_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "domain": "idm.example.com",
        "image": format!("kanidm/server:{}", get_dependency_version().unwrap()),
        "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1}],
    })
});

const DEFAULT_WAIT_FOR_REPLICATION_READY_SECONDS: u64 = 60 * 45;
const REPLICATION_POLL_INTERVAL_SECONDS: u64 = 10;

fn wait_for_replication_ready_seconds() -> u64 {
    std::env::var("E2E_REPLICATION_TIMEOUT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_WAIT_FOR_REPLICATION_READY_SECONDS)
}
const CERTIFICATE_RENEWAL_DELAY_SECONDS: u64 = 60 * 2;

static STORAGE_VOLUME_CLAIM_TEMPLATE_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "storage": {
            "volumeClaimTemplate": {
                "spec": {
                    "accessModes": [
                        "ReadWriteOnce"
                    ],
                    "storageClassName": "standard",
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

static STORAGE_VOLUME_CLAIM_TEMPLATE_DEFAULT_CLASS_JSON: LazyLock<serde_json::Value> =
    LazyLock::new(|| {
        json!({
            "storage": {
                "volumeClaimTemplate": {
                    "spec": {
                        "accessModes": ["ReadWriteOnce"],
                        "resources": {
                            "requests": {
                                "storage": "1Gi"
                            }
                        }
                    }
                }
            }
        })
    });

static INIT_CONTAINERS_SECURITY_CONTEXT_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "initContainers": [
            {
                "name": "kanidm-generate-replication-config",
                "securityContext": {
                    "allowPrivilegeEscalation": false,
                    "capabilities": {
                        "drop": ["ALL"]
                    }
                }
            }
        ]
    })
});

e2e_test!(kanidm_init_containers_without_replication, {
    let name = "test-init-containers-no-repl";
    let s = setup(name, Some(INIT_CONTAINERS_SECURITY_CONTEXT_JSON.clone())).await;

    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let sts = s.statefulset_api.get(&sts_name).await.unwrap();

    let init_containers = sts
        .spec
        .as_ref()
        .unwrap()
        .template
        .spec
        .as_ref()
        .unwrap()
        .init_containers
        .as_ref();

    if let Some(containers) = init_containers {
        assert!(
            !containers
                .iter()
                .any(|c| c.name == "kanidm-generate-replication-config"),
            "init container kanidm-generate-replication-config should be filtered out when replication is disabled"
        );
    }
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

fn has_observed_generation_after(
    generation: i64,
    resource_version: String,
) -> impl Condition<Kanidm> {
    move |obj: Option<&Kanidm>| {
        obj.is_some_and(|kanidm| {
            kanidm.metadata.resource_version.as_deref() != Some(resource_version.as_str())
                && kanidm
                    .status
                    .as_ref()
                    .and_then(|status| status.conditions.as_ref())
                    .is_some_and(|conditions| {
                        conditions
                            .iter()
                            .any(|condition| condition.observed_generation == Some(generation))
                    })
        })
    }
}

pub fn is_statefulset_ready(obj: Option<&StatefulSet>) -> bool {
    obj.and_then(|statefulset| statefulset.status.as_ref())
        .is_some_and(|s| s.ready_replicas == Some(s.replicas))
}

pub fn has_statefulset_ready_replicas(expected: i32) -> impl Fn(Option<&StatefulSet>) -> bool {
    move |obj: Option<&StatefulSet>| {
        obj.and_then(|statefulset| statefulset.status.as_ref())
            .is_some_and(|s| s.ready_replicas == Some(expected))
    }
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
    }

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
    #[allow(dead_code)]
    pub admin_password: String,
    pub idm_admin_password: String,
}

pub async fn setup(name: &str, kanidm_spec_patch: Option<serde_json::Value>) -> SetupKanidm {
    init_crypto_provider();

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

async fn wait_for_replication_success_with_timeout(pod_api: &Api<Pod>, pod_names: &[String]) {
    let start = Instant::now();
    let mut success = vec![false; pod_names.len()];
    loop {
        for (idx, pod_name) in pod_names.iter().enumerate() {
            if success[idx] {
                continue;
            }
            let logs = pod_api
                .logs(pod_name, &LogParams::default())
                .await
                .unwrap_or_default();
            if logs.contains("Incremental Replication Success") {
                success[idx] = true;
            }
        }
        if success.iter().all(|&x| x) {
            return;
        }
        if start.elapsed() > Duration::from_secs(wait_for_replication_ready_seconds()) {
            panic!("Replication success not observed in all pods within timeout");
        }
        sleep(Duration::from_secs(REPLICATION_POLL_INTERVAL_SECONDS)).await;
    }
}

pub const MINIO_ENDPOINT: &str = "https://minio.default.svc:9000";
pub const MINIO_BUCKET: &str = "kaniop-backups";
pub const MINIO_REGION: &str = "us-east-1";
pub const MINIO_CA_CM: &str = "minio-ca";
pub const MINIO_CREDS_SECRET: &str = "minio-creds";
pub const MINIO_CREDS_INVALID_SECRET: &str = "minio-creds-invalid";

pub fn minio_s3_config(prefix: &str) -> kaniop_backup_core::crd::S3Config {
    kaniop_backup_core::crd::S3Config {
        bucket: MINIO_BUCKET.to_string(),
        prefix: prefix.to_string(),
        region: MINIO_REGION.to_string(),
        endpoint: MINIO_ENDPOINT.to_string(),
        force_path_style: true,
        insecure: false,
        ca_bundle_ref: Some(MINIO_CA_CM.to_string()),
    }
}

pub fn minio_auth(secret_name: &str) -> kaniop_backup_core::crd::RepositoryAuthentication {
    use kaniop_backup_core::crd::{AuthMethod, RepositoryAuthentication, SecretRef};
    let method = AuthMethod {
        workload_identity: None,
        secret_ref: Some(SecretRef {
            name: secret_name.to_string(),
        }),
    };
    RepositoryAuthentication {
        writer: method.clone(),
        reader: method.clone(),
        deleter: method,
    }
}

pub async fn force_delete_and_wait<K>(api: Api<K>, name: &str)
where
    K: kube::Resource
        + Clone
        + std::fmt::Debug
        + for<'de> k8s_openapi::serde::Deserialize<'de>
        + 'static
        + Send,
{
    api.delete(name, &Default::default()).await.ok();
    api.patch(
        name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(json!({"metadata": {"finalizers": null}})),
    )
    .await
    .ok();
    crate::test::poll_until(&format!("{name} deleted"), || {
        let api = api.clone();
        let name = name.to_string();
        async move {
            if api.get(&name).await.is_err() {
                Some(())
            } else {
                None
            }
        }
    })
    .await;
}

pub fn is_repo_ready() -> impl Condition<KanidmBackupRepository> {
    move |obj: Option<&KanidmBackupRepository>| {
        obj.and_then(|repo| repo.status.as_ref())
            .is_some_and(|status| {
                status
                    .conditions
                    .iter()
                    .any(|c| c.type_ == "Ready" && c.status == "True" && c.reason == "Accepted")
            })
    }
}

pub async fn create_repository(client: &Client, name: &str, prefix: &str, secret: &str) {
    use kaniop_backup_core::crd::{KanidmBackupRepository, KanidmBackupRepositorySpec};
    let api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
    force_delete_and_wait(api.clone(), name).await;
    let repo = KanidmBackupRepository::new(
        name,
        KanidmBackupRepositorySpec {
            s3: minio_s3_config(prefix),
            authentication: minio_auth(secret),
            encryption: None,
            limits: None,
        },
    );
    api.create(&PostParams::default(), &repo).await.unwrap();
}

pub async fn create_repository_with_encryption(
    client: &Client,
    name: &str,
    prefix: &str,
    secret: &str,
    encryption: Option<kaniop_backup_core::crd::RepositoryEncryption>,
) {
    use kaniop_backup_core::crd::{KanidmBackupRepository, KanidmBackupRepositorySpec};
    let api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
    force_delete_and_wait(api.clone(), name).await;
    let repo = KanidmBackupRepository::new(
        name,
        KanidmBackupRepositorySpec {
            s3: minio_s3_config(prefix),
            authentication: minio_auth(secret),
            encryption,
            limits: None,
        },
    );
    api.create(&PostParams::default(), &repo).await.unwrap();
}

pub async fn create_kek_secret(client: &Client, name: &str, key_value: &[u8]) {
    let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
    force_delete_and_wait(secret_api.clone(), name).await;
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        string_data: Some(std::collections::BTreeMap::from([(
            "encryption-key".to_string(),
            String::from_utf8(key_value.to_vec()).unwrap(),
        )])),
        ..Default::default()
    };
    secret_api
        .create(&PostParams::default(), &secret)
        .await
        .unwrap();
}

pub fn data_mover_image() -> String {
    std::env::var("DATA_MOVER_IMAGE").unwrap_or_else(|_| {
        format!(
            "ghcr.io/pando85/kaniop-data-mover:{}",
            option_env!("GIT_SHA").unwrap_or("aed6d7e")
        )
    })
}

pub async fn trigger_backup_on_primary(client: &Client, kanidm_name: &str) -> String {
    let pod_api = Api::<Pod>::namespaced(client.clone(), "default");
    let primary_pod = format!("{kanidm_name}-{DEFAULT_REPLICA_GROUP_NAME}-0");
    let backup_name = format!("backup-{}.json.gz", uuid::Uuid::new_v4());
    let backup_path = format!("/data/{backup_name}");
    let exec_result = pod_api
        .exec(
            &primary_pod,
            vec![
                "kanidmd".to_string(),
                "database".to_string(),
                "backup".to_string(),
                backup_path.clone(),
            ],
            &kube::api::AttachParams::default().container("kanidm"),
        )
        .await
        .unwrap();
    kaniop_k8s_util::client::get_output(exec_result)
        .await
        .expect("backup command should succeed");
    backup_name
}

impl<'a> UploadOptions<'a> {
    pub fn new(
        kanidm_name: &'a str,
        prefix: &'a str,
        backup_name: &'a str,
        backup_id: &'a str,
        kanidm_uid: &'a str,
        domain: &'a str,
    ) -> Self {
        Self {
            encryption: None,
            extra_operation_fields: None,
            kanidm_name,
            prefix,
            backup_name,
            backup_id,
            kanidm_uid,
            domain,
        }
    }

    pub fn with_encryption(self, kek_secret_name: &'a str) -> Self {
        Self {
            encryption: Some(kek_secret_name),
            ..self
        }
    }

    pub fn with_extra_fields(self, extra_operation_fields: Option<serde_json::Value>) -> Self {
        Self {
            extra_operation_fields,
            ..self
        }
    }
}

pub async fn upload_backup_to_s3(client: &Client, options: UploadOptions<'_>) -> String {
    upload_backup_to_s3_internal(client, options).await
}

pub async fn upload_backup_to_s3_with_encryption_key(
    client: &Client,
    options: UploadOptions<'_>,
) -> String {
    upload_backup_to_s3_internal(client, options).await
}

async fn upload_backup_to_s3_internal(
    client: &Client,
    UploadOptions {
        kanidm_name,
        prefix,
        backup_name,
        backup_id,
        kanidm_uid,
        domain,
        encryption: encryption_secret,
        extra_operation_fields,
    }: UploadOptions<'_>,
) -> String {
    use k8s_openapi::api::batch::v1::Job;
    use k8s_openapi::api::core::v1::{ConfigMap, Namespace};

    let namespace_api: Api<Namespace> = Api::all(client.clone());
    let default_ns = namespace_api.get("default").await.unwrap();
    let namespace_uid = default_ns.metadata.uid.unwrap();

    let manifest_key = format!(
        "{prefix}/v1/tenants/{namespace_uid}/clusters/{kanidm_uid}/backups/{backup_id}/manifest.json"
    );

    let mut operation_doc = json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "upload",
        "payloadPath": format!("/data/{backup_name}"),
        "bucket": MINIO_BUCKET,
        "prefix": prefix,
        "endpoint": MINIO_ENDPOINT,
        "region": MINIO_REGION,
        "forcePathStyle": true,
        "caBundlePath": "/run/kaniop-ca-bundle/ca-bundle.pem",
        "backupId": backup_id,
        "namespaceUid": namespace_uid,
        "kanidmUid": kanidm_uid,
        "kanidmName": kanidm_name,
        "domain": domain,
        "kanidmVersion": "e2e",
        "consistency": "kanidm-offline",
        "reason": "e2e-test",
        "resultPath": "/run/kaniop-result/result.json",
        "maxConcurrentParts": 4,
        "maxRetries": 3,
    });
    if let Some(extra) = extra_operation_fields {
        json_patch::merge(&mut operation_doc, &extra);
    }

    let sts_name = format!("{kanidm_name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let pvc_name = format!("kanidm-data-{sts_name}-0");
    let op_cm_name = format!("{kanidm_name}-upload-op");

    let cm_api = Api::<ConfigMap>::namespaced(client.clone(), "default");
    let op_cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(op_cm_name.clone()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        data: Some(
            [(
                "operation.json".to_string(),
                serde_json::to_string(&operation_doc).unwrap(),
            )]
            .into_iter()
            .collect(),
        ),
        ..Default::default()
    };
    cm_api.create(&PostParams::default(), &op_cm).await.unwrap();

    let job_api = Api::<Job>::namespaced(client.clone(), "default");
    let upload_job_name = format!("{kanidm_name}-upload");

    let mut env_vars: Vec<serde_json::Value> = vec![
        serde_json::from_value(json!({"name": "AWS_ACCESS_KEY_ID", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_ACCESS_KEY_ID"}}})).unwrap(),
        serde_json::from_value(json!({"name": "AWS_SECRET_ACCESS_KEY", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_SECRET_ACCESS_KEY"}}})).unwrap(),
        serde_json::from_value(json!({"name": "RUST_LOG", "value": "info"})).unwrap(),
        serde_json::from_value(json!({"name": "SSL_CERT_FILE", "value": "/run/kaniop-ca-bundle/ca-bundle.pem"})).unwrap(),
    ];

    if let Some(kek_name) = encryption_secret {
        env_vars.push(
            serde_json::from_value(json!({"name": "KANIOP_ENCRYPTION_KEY", "valueFrom": {"secretKeyRef": {"name": kek_name, "key": "encryption-key"}}})).unwrap(),
        );
    }

    let upload_job: Job = serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": upload_job_name,
            "namespace": "default"
        },
        "spec": {
            "backoffLimit": 1,
            "template": {
                "spec": {
                    "restartPolicy": "Never",
                    "containers": [{
                        "name": "data-mover",
                        "image": data_mover_image(),
                        "command": ["/bin/kaniop-data-mover", "upload"],
                        "env": env_vars,
                        "volumeMounts": [
                            {"name": "data", "mountPath": "/data"},
                            {"name": "operation", "mountPath": "/run/kaniop"},
                            {"name": "ca-bundle", "mountPath": "/run/kaniop-ca-bundle"},
                            {"name": "result", "mountPath": "/run/kaniop-result"}
                        ]
                    }],
                    "volumes": [
                        {"name": "data", "persistentVolumeClaim": {"claimName": pvc_name}},
                        {"name": "operation", "configMap": {"name": op_cm_name}},
                        {"name": "ca-bundle", "configMap": {"name": MINIO_CA_CM}},
                        {"name": "result", "emptyDir": {}}
                    ]
                }
            }
        }
    }))
    .unwrap();

    job_api
        .create(&PostParams::default(), &upload_job)
        .await
        .unwrap();

    crate::test::poll_until("upload job completes", || {
        let job_api = job_api.clone();
        let job_name = upload_job_name.clone();
        async move {
            let job = job_api.get(&job_name).await.ok()?;
            if job
                .status
                .as_ref()
                .is_some_and(|s| s.succeeded.is_some_and(|v| v > 0))
            {
                Some(())
            } else {
                None
            }
        }
    })
    .await;

    job_api
        .delete(&upload_job_name, &Default::default())
        .await
        .ok();
    cm_api.delete(&op_cm_name, &Default::default()).await.ok();

    manifest_key
}

pub async fn create_backup_cr_and_wait(
    client: &Client,
    backup_id: &str,
    kanidm_name: &str,
    kanidm_uid: &str,
    repo_name: &str,
    manifest_key: &str,
) -> String {
    use kaniop_backup_core::crd::{
        BackupKanidmRef, BackupRepositoryRef, KanidmBackup, KanidmBackupPhase, KanidmBackupSpec,
    };
    let backup_cr_name = format!("kb-{}", &backup_id[..8]);
    let backup_cr = KanidmBackup {
        metadata: kube::api::ObjectMeta {
            name: Some(backup_cr_name.clone()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: KanidmBackupSpec {
            backup_id: backup_id.to_string(),
            kanidm_ref: BackupKanidmRef {
                name: kanidm_name.to_string(),
                uid: kanidm_uid.to_string(),
            },
            repository_ref: BackupRepositoryRef {
                name: repo_name.to_string(),
            },
            manifest_key: manifest_key.to_string(),
        },
        status: None,
    };
    let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), "default");
    backup_api
        .create(&PostParams::default(), &backup_cr)
        .await
        .unwrap();
    wait_for(backup_api, &backup_cr_name, |obj: Option<&KanidmBackup>| {
        obj.and_then(|b| b.status.as_ref())
            .is_some_and(|s| s.phase == KanidmBackupPhase::Ready)
    })
    .await;
    backup_cr_name
}

pub async fn scale_statefulset_and_wait(client: &Client, sts_name: &str, replicas: i32) {
    use k8s_openapi::api::apps::v1::StatefulSet;
    let api = Api::<StatefulSet>::namespaced(client.clone(), "default");
    let mut sts = api.get(sts_name).await.unwrap();
    sts.spec.as_mut().unwrap().replicas = Some(replicas);
    sts.metadata.managed_fields = None;
    api.patch(
        sts_name,
        &kube::api::PatchParams::apply("e2e-test").force(),
        &kube::api::Patch::Apply(&sts),
    )
    .await
    .unwrap();
    crate::test::poll_until(&format!("statefulset scaled to {replicas}"), || {
        let api = api.clone();
        let sts_name = sts_name.to_string();
        async move {
            let sts = api.get(&sts_name).await.ok()?;
            let ready = sts
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0);
            if ready == replicas { Some(()) } else { None }
        }
    })
    .await;
}

pub async fn cleanup_test_resources(client: &Client, kanidm_name: &str, repo_name: &str) {
    use kaniop_backup_core::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule};
    use kaniop_operator::kanidm::restore::KanidmRestore;

    let ns = "default";

    let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), ns);
    force_delete_and_wait(restore_api, &format!("{kanidm_name}-restore")).await;

    let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), ns);
    if let Ok(list) = backup_api.list(&Default::default()).await {
        for backup in list.items {
            if backup.spec.kanidm_ref.name == kanidm_name {
                force_delete_and_wait(backup_api.clone(), &backup.name_any()).await;
            }
        }
    }

    let schedule_api = Api::<KanidmBackupSchedule>::namespaced(client.clone(), ns);
    if let Ok(list) = schedule_api.list(&Default::default()).await {
        for schedule in list.items {
            if schedule.spec.kanidm_ref.name == kanidm_name {
                force_delete_and_wait(schedule_api.clone(), &schedule.name_any()).await;
            }
        }
    }

    let repo_api = Api::<KanidmBackupRepository>::namespaced(client.clone(), ns);
    force_delete_and_wait(repo_api, repo_name).await;

    let job_api = Api::<k8s_openapi::api::batch::v1::Job>::namespaced(client.clone(), ns);
    for suffix in [
        "-upload",
        "-restore",
        "-verify",
        "-safety-backup",
        "-source-prep",
        "-discover-check",
        "-corrupt-trunc",
        "-corrupt-backup",
    ] {
        force_delete_and_wait(job_api.clone(), &format!("{kanidm_name}{suffix}")).await;
    }
    if let Ok(list) = job_api.list(&Default::default()).await {
        for job in list.items {
            let job_name = job.name_any();
            if job_name.starts_with(&format!("{kanidm_name}-upload-")) {
                force_delete_and_wait(job_api.clone(), &job_name).await;
            }
        }
    }

    let cm_api = Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(client.clone(), ns);
    force_delete_and_wait(cm_api.clone(), &format!("{kanidm_name}-upload-op")).await;
    force_delete_and_wait(cm_api.clone(), &format!("{kanidm_name}-discover-check")).await;
    if let Ok(list) = cm_api.list(&Default::default()).await {
        for cm in list.items {
            let cm_name = cm.name_any();
            if cm_name.starts_with(&format!("{kanidm_name}-upload-")) && cm_name.ends_with("-op") {
                force_delete_and_wait(cm_api.clone(), &cm_name).await;
            }
        }
    }

    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), ns);
    force_delete_and_wait(kanidm_api, kanidm_name).await;

    let secret_api = Api::<Secret>::namespaced(client.clone(), ns);
    force_delete_and_wait(secret_api, &format!("{kanidm_name}-tls")).await;
}

e2e_test!(kanidm_create, {
    let name = "test-create";
    setup(name, None).await;
});

e2e_test!(kanidm_delete_statefulset, {
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
});

e2e_test!(kanidm_delete_kanidm, {
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
});

e2e_test!(kanidm_change_statefulset, {
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
});

e2e_test!(kanidm_change_kanidm_replicas, {
    let name = "test-change-kanidm-replicas";
    let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;
    let service_api = Api::<Service>::namespaced(s.client.clone(), "default");
    let original_service = service_api.get(name).await.unwrap();
    let original_service_uid = original_service.uid().unwrap();
    assert_ne!(
        original_service
            .spec
            .as_ref()
            .and_then(|spec| spec.cluster_ip.as_deref()),
        Some("None")
    );

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

    wait_for(service_api.clone(), name, move |obj: Option<&Service>| {
        obj.is_some_and(|service| {
            service.metadata.uid.as_deref() != Some(original_service_uid.as_str())
                && service
                    .spec
                    .as_ref()
                    .and_then(|spec| spec.cluster_ip.as_deref())
                    == Some("None")
        })
    })
    .await;

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
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    let pod_names = (0..2)
        .map(|i| format!("{sts_name}-{i}"))
        .collect::<Vec<_>>();
    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
});

e2e_test!(
    kanidm_default_storage_class_does_not_recreate_statefulset,
    {
        init_crypto_provider();
        let name = "test-default-storage-class";
        let client = Client::try_default().await.unwrap();
        let statefulset_api = Api::<StatefulSet>::namespaced(client.clone(), "default");

        let mut spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        merge(
            &mut spec_json,
            &STORAGE_VOLUME_CLAIM_TEMPLATE_DEFAULT_CLASS_JSON,
        );
        let generated_kanidm = Kanidm::new(name, serde_json::from_value(spec_json).unwrap());
        let mut preexisting = generated_kanidm
            .create_statefulset(&generated_kanidm.spec.replica_groups[0], None, None)
            .unwrap();
        preexisting
            .spec
            .as_mut()
            .unwrap()
            .volume_claim_templates
            .as_mut()
            .unwrap()[0]
            .spec
            .as_mut()
            .unwrap()
            .storage_class_name = Some("standard".to_string());

        let original = statefulset_api
            .create(&PostParams::default(), &preexisting)
            .await
            .unwrap();
        let original_uid = original.uid().unwrap();

        let mut desired_patch = STORAGE_VOLUME_CLAIM_TEMPLATE_DEFAULT_CLASS_JSON.clone();
        merge(
            &mut desired_patch,
            &json!({
                "disableUpgradeChecks": true,
                "minReadySeconds": 1
            }),
        );
        let (_, kanidm_api) = create_kanidm(&client, name, Some(desired_patch)).await;
        create_secret(&client, name).await;

        wait_for(
            statefulset_api.clone(),
            &format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"),
            |obj: Option<&StatefulSet>| {
                obj.and_then(|sts| sts.spec.as_ref()).is_some_and(|spec| {
                    spec.min_ready_seconds == Some(1)
                        && spec
                            .volume_claim_templates
                            .as_ref()
                            .and_then(|templates| templates.first())
                            .and_then(|pvc| pvc.spec.as_ref())
                            .and_then(|spec| spec.storage_class_name.as_deref())
                            == Some("standard")
                })
            },
        )
        .await;

        let updated = statefulset_api
            .get(&format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"))
            .await
            .unwrap();
        assert_eq!(updated.uid().as_deref(), Some(original_uid.as_str()));
        assert_eq!(updated.spec.as_ref().unwrap().min_ready_seconds, Some(1));
        assert_eq!(
            updated
                .spec
                .as_ref()
                .unwrap()
                .volume_claim_templates
                .as_ref()
                .unwrap()[0]
                .spec
                .as_ref()
                .unwrap()
                .storage_class_name
                .as_deref(),
            Some("standard")
        );

        let current_kanidm = kanidm_api.get(name).await.unwrap();
        let generation = current_kanidm.metadata.generation.unwrap();
        let resource_version = current_kanidm.resource_version().unwrap();
        wait_for(
            kanidm_api,
            name,
            has_observed_generation_after(generation, resource_version),
        )
        .await;
    }
);

e2e_test!(kanidm_statefulset_already_exists, {
    init_crypto_provider();
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
});

e2e_test!(
    kanidm_statefulset_immutable_field_conflict_recreates_statefulset,
    {
        init_crypto_provider();
        let name = "test-sts-immutable-conflict";
        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let statefulset = json!({
            "apiVersion": "apps/v1",
            "kind": "StatefulSet",
            "metadata": {
                "name": sts_name
            },
            "spec": {
                "replicas": 1,
                "selector": {
                    "matchLabels": {
                        "app": "conflicting-selector"
                    }
                },
                "template": {
                    "metadata": {
                        "labels": {
                            "app": "conflicting-selector"
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
        let client = Client::try_default().await.unwrap();
        let statefulset_api = Api::<StatefulSet>::namespaced(client.clone(), "default");
        let original = statefulset_api
            .create(
                &PostParams::default(),
                &serde_json::from_value(statefulset).unwrap(),
            )
            .await
            .unwrap();
        let original_uid = original.uid().unwrap();

        let _ = create_kanidm(
            &client,
            name,
            Some(json!({
                "disableUpgradeChecks": true
            })),
        )
        .await;
        create_secret(&client, name).await;

        let expected_instance = name.to_string();
        wait_for(
            statefulset_api.clone(),
            &sts_name,
            move |obj: Option<&StatefulSet>| {
                obj.is_some_and(|sts| {
                    sts.metadata.uid.as_deref() != Some(original_uid.as_str())
                        && sts
                            .spec
                            .as_ref()
                            .and_then(|spec| spec.selector.match_labels.as_ref())
                            .and_then(|labels| labels.get("app.kubernetes.io/instance"))
                            == Some(&expected_instance)
                })
            },
        )
        .await;
    }
);

e2e_test!(kanidm_statefulset_non_immutable_422_is_non_destructive, {
    let name = "test-sts-non-immutable-422";
    let s = setup(name, None).await;
    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let original = s.statefulset_api.get(&sts_name).await.unwrap();
    let original_uid = original.uid().unwrap();

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.min_ready_seconds = Some(-1);
    kanidm.metadata.managed_fields = None;
    let patched = s
        .kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    let generation = patched.metadata.generation.unwrap();
    let resource_version = patched.resource_version().unwrap();
    wait_for(
        s.kanidm_api.clone(),
        name,
        has_observed_generation_after(generation, resource_version),
    )
    .await;

    let after_first_reconcile = s.kanidm_api.get(name).await.unwrap();
    let resource_version = after_first_reconcile.resource_version().unwrap();
    wait_for(
        s.kanidm_api.clone(),
        name,
        has_observed_generation_after(generation, resource_version),
    )
    .await;

    let current = s.statefulset_api.get(&sts_name).await.unwrap();
    assert_eq!(current.uid().as_deref(), Some(original_uid.as_str()));
    assert_ne!(
        current
            .spec
            .as_ref()
            .and_then(|spec| spec.min_ready_seconds),
        Some(-1)
    );
});

e2e_test!(kanidm_statefulset_mixed_422_does_not_cause_delete, {
    init_crypto_provider();
    let name = "test-mixed-422";
    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let statefulset = json!({
        "apiVersion": "apps/v1",
        "kind": "StatefulSet",
        "metadata": { "name": sts_name },
        "spec": {
            "replicas": 1,
            "selector": { "matchLabels": { "app": "conflicting-immutable-selector" } },
            "template": {
                "metadata": { "labels": { "app": "conflicting-immutable-selector" } },
                "spec": {
                    "containers": [{ "name": name, "image": "kanidm/server:latest" }]
                }
            }
        }
    });
    let client = Client::try_default().await.unwrap();
    let statefulset_api = Api::<StatefulSet>::namespaced(client.clone(), "default");
    let original = statefulset_api
        .create(
            &PostParams::default(),
            &serde_json::from_value(statefulset).unwrap(),
        )
        .await
        .unwrap();
    let original_uid = original.uid().unwrap();

    let (_, kanidm_api) = create_kanidm(
        &client,
        name,
        Some(json!({
            "disableUpgradeChecks": true,
            "minReadySeconds": -1
        })),
    )
    .await;
    create_secret(&client, name).await;

    // Require two distinct status writes after the desired generation exists. This proves the
    // controller has actually attempted reconciliation rather than merely observing creation.
    let before = kanidm_api.get(name).await.unwrap();
    let generation = before.metadata.generation.unwrap();
    let resource_version = before.resource_version().unwrap();
    wait_for(
        kanidm_api.clone(),
        name,
        has_observed_generation_after(generation, resource_version),
    )
    .await;
    let after_first = kanidm_api.get(name).await.unwrap();
    let resource_version = after_first.resource_version().unwrap();
    wait_for(
        kanidm_api,
        name,
        has_observed_generation_after(generation, resource_version),
    )
    .await;

    let current = statefulset_api.get(&sts_name).await.unwrap();
    assert_eq!(current.uid().as_deref(), Some(original_uid.as_str()));
    assert_ne!(
        current
            .spec
            .as_ref()
            .and_then(|spec| spec.min_ready_seconds),
        Some(-1)
    );
});

e2e_test!(kanidm_change_domain, {
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
});

e2e_test!(kanidm_donwscale_to_zero, {
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
});

e2e_test!(kanidm_recreate_admin_passwords, {
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
});

e2e_test!(kanidm_invalid_chars_on_name, {
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
});

e2e_test!(kanidm_invalid_long_names, {
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
});

e2e_test!(
    #[serial_test::serial(replication)]
    kanidm_renew_certificates,
    {
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
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
        let pod_names = (0..2)
            .map(|i| format!("{sts_name}-{i}"))
            .collect::<Vec<_>>();
        wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;

        for i in 0..replicas {
            let pod_name = format!("{sts_name}-{i}");
            let secret_name = kanidm.replica_secret_name(&pod_name);
            let mut secret = s.secret_api.get(&secret_name).await.unwrap();
            let data = secret.data.as_mut().unwrap();
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

        wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
    }
);

fn tls_hash_annotation(sts: &StatefulSet) -> Option<String> {
    sts.spec
        .as_ref()?
        .template
        .metadata
        .as_ref()?
        .annotations
        .as_ref()?
        .get(TLS_SECRET_HASH_ANNOTATION)
        .cloned()
}

e2e_test!(kanidm_tls_secret_renewal, {
    let name = "test-tls-secret-renewal";
    let s = setup(name, None).await;

    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let sts = s.statefulset_api.get(&sts_name).await.unwrap();
    let initial_hash = tls_hash_annotation(&sts).expect("TLS hash annotation on pod template");

    let secret_name = format!("{name}-tls");
    let mut secret = s.secret_api.get(&secret_name).await.unwrap();
    secret
        .data
        .as_mut()
        .unwrap()
        .insert("tls.crt".to_string(), ByteString([CERT, b"\n"].concat()));
    secret.metadata.managed_fields = None;
    s.secret_api
        .patch(
            &secret_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&secret),
        )
        .await
        .unwrap();

    wait_for(
        s.statefulset_api.clone(),
        &sts_name,
        move |obj: Option<&StatefulSet>| {
            obj.and_then(tls_hash_annotation)
                .is_some_and(|h| h != initial_hash)
        },
    )
    .await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
});

e2e_test!(kanidm_block_incompatible_version_upgrade, {
    use kaniop_operator::kanidm::crd::VersionCompatibilityResult;

    let name = "test-block-incompatible-version";
    let s = setup(name, None).await;

    let current_sts = s
        .statefulset_api
        .get(&format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"))
        .await
        .unwrap();
    let original_image = current_sts
        .spec
        .as_ref()
        .unwrap()
        .template
        .spec
        .as_ref()
        .unwrap()
        .containers
        .first()
        .unwrap()
        .image
        .clone()
        .unwrap();

    let incompatible_image = "kanidm/server:99.0.0";
    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.image = incompatible_image.to_string();
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
            .and_then(|status| status.version.as_ref())
            .is_some_and(|v| v.compatibility_result == VersionCompatibilityResult::Incompatible)
    })
    .await;

    let updated_sts = s
        .statefulset_api
        .get(&format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"))
        .await
        .unwrap();
    let updated_image = updated_sts
        .spec
        .as_ref()
        .unwrap()
        .template
        .spec
        .as_ref()
        .unwrap()
        .containers
        .first()
        .unwrap()
        .image
        .clone()
        .unwrap();

    assert_eq!(
        updated_image, original_image,
        "StatefulSet image should not be updated to incompatible version"
    );
    assert_ne!(
        updated_image, incompatible_image,
        "StatefulSet should not have incompatible image"
    );
});

e2e_test!(kanidm_block_incompatible_version_initial_creation, {
    use kaniop_operator::kanidm::crd::VersionCompatibilityResult;

    let name = "test-block-incompatible-initial";
    let incompatible_image = "kanidm/server:99.0.0";

    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    kanidm_spec_json["image"] = incompatible_image.into();

    init_crypto_provider();

    let client = Client::try_default().await.unwrap();
    create_secret(&client, name).await;

    let kanidm = Kanidm::new(name, serde_json::from_value(kanidm_spec_json).unwrap());
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    kanidm_api
        .create(&PostParams::default(), &kanidm)
        .await
        .unwrap();

    wait_for(kanidm_api.clone(), name, |obj: Option<&Kanidm>| {
        obj.and_then(|kanidm| kanidm.status.as_ref())
            .and_then(|status| status.version.as_ref())
            .is_some_and(|v| v.compatibility_result == VersionCompatibilityResult::Incompatible)
    })
    .await;

    let statefulset_api = Api::<StatefulSet>::namespaced(client.clone(), "default");
    let sts = statefulset_api
        .get(&format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"))
        .await;

    assert!(
        sts.is_err() || sts.unwrap().spec.is_none(),
        "StatefulSet should not be created with incompatible version"
    );
});
