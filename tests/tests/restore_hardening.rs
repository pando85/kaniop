#![cfg(feature = "e2e-test")]

use std::collections::BTreeMap;
use std::sync::Once;
use std::time::Duration;

use e2e::kanidm::get_dependency_version;
use k8s_openapi::ByteString;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, PersistentVolumeClaim, Pod, Secret};
use kaniop_backup_core::crd::{
    AuthMethod, BackupKanidmRef, BackupRepositoryRef, KanidmBackup, KanidmBackupPhase,
    KanidmBackupRepository, KanidmBackupRepositorySpec, KanidmBackupSpec, RepositoryAuthentication,
    SecretRef,
};
use kaniop_k8s_util::client::get_output;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::{
    BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION, KanidmRestore,
    KanidmRestoreBackupRefSource, KanidmRestoreLocalSource, KanidmRestorePhase,
    KanidmRestoreSource, KanidmRestoreSpec, KanidmRestoreTargetRef, SafetyBackupConfig,
    SafetyBackupRepositoryRef,
};
use kube::api::{Api, AttachParams, DeleteParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::{Client, ResourceExt};
use rustls::crypto::aws_lc_rs::default_provider;
use serde_json::json;
use serial_test::serial;
use tokio::time::{Instant, sleep};

const NAMESPACE: &str = "default";
const REPLICA_GROUP: &str = "default";
const RESTORE_ANNOTATION: &str = "kanidm.kaniop.rs/restore-in-progress";
const DOMAIN_INFO_UUID: &str = "00000000-0000-0000-0000-ffffff000025";
const BUSYBOX_IMAGE: &str = "busybox:1.37.0";
const WAIT_TIMEOUT: Duration = Duration::from_secs(300);
const REPLICATION_TIMEOUT: Duration = Duration::from_secs(45 * 60);

const CERT: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIICGjCCAb+gAwIBAgIUHpT08nqX951u//GR+v8XT79r9SUwCgYIKoZIzj0EAwIw\nRDELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRYw\nFAYDVQQDDA1LYW5pb3AgRTJFIENBMB4XDTI2MDgwODA2MTkxOVoXDTM2MDgwNTA2\nMTkxOVowRjELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2Fu\naWRtMRgwFgYDVQQDDA9pZG0uZXhhbXBsZS5jb20wWTATBgcqhkjOPQIBBggqhkjO\nPQMBBwNCAAQvppDjypVndfeojNUQ4o1r0v/+ry6an9tRRgdaqpAWycCsHHwqzxRG\nvQmGifZQ5dsBle7+3df8YBfXmikDRTEeo4GMMIGJMAkGA1UdEwQCMAAwCwYDVR0P\nBAQDAgWgMBMGA1UdJQQMMAoGCCsGAQUFBwMBMBoGA1UdEQQTMBGCD2lkbS5leGFt\ncGxlLmNvbTAdBgNVHQ4EFgQU08vzk3TPxjTZYSarIJ/X8483q5MwHwYDVR0jBBgw\nFoAU/oFjdY0iaHDwDEsG9K2kLqnKaCswCgYIKoZIzj0EAwIDSQAwRgIhAOAaimcS\nz/IUkI03CYbicyGIQDmXBruN584Uk0wLmOxBAiEA1T6y7HbX3F1oyftd5wABZPDB\nCpREB0kqGwMUURezf4w=\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIIB3DCCAYOgAwIBAgIUfsv6cZIgDmqN1h4xCuD9CjVRLkgwCgYIKoZIzj0EAwIw\nRDELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRYw\nFAYDVQQDDA1LYW5pb3AgRTJFIENBMB4XDTI2MDgwODA2MTkxOVoXDTM2MDgwNTA2\nMTkxOVowRDELMAkGA1UEBhMCQVUxDDAKBgNVBAgMA1FMRDEPMA0GA1UECgwGS2Fu\naWRtMRYwFAYDVQQDDA1LYW5pb3AgRTJFIENBMFkwEwYHKoZIzj0CAQYIKoZIzj0D\nAQcDQgAEy84lnsJddCODwnayK4yoqLf6jVGTWIT0mpUh01Ghoq8GrXSrvYGIjxZ0\nYFPEwstiso8GJP15JKXzoGJTUs4a6aNTMFEwHQYDVR0OBBYEFP6BY3WNImhw8AxL\nBvStpC6pymgrMB8GA1UdIwQYMBaAFP6BY3WNImhw8AxLBvStpC6pymgrMA8GA1Ud\nEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDRwAwRAIgLqbXmVvrEP9zjuMcU0j+R79Z\nFzsMIBS59ZhCJVTa3NACIG2rT7suWcwoc2Wkv7y0AWdpRoZcpLwL0kGNzN5yidHS\n-----END CERTIFICATE-----\n";
const KEY: &[u8] = b"-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg1H0PlmChG8z25SO9\nuhAEmbDIisdcSmYzbWotQL+sb5WhRANCAAQvppDjypVndfeojNUQ4o1r0v/+ry6a\nn9tRRgdaqpAWycCsHHwqzxRGvQmGifZQ5dsBle7+3df8YBfXmikDRTEe\n-----END PRIVATE KEY-----\n";

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        default_provider().install_default().unwrap();
    });
}

async fn wait_until<T, F, Fut>(description: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {description}"
        );
        sleep(Duration::from_secs(2)).await;
    }
}

fn sts_name(name: &str) -> String {
    format!("{name}-{REPLICA_GROUP}")
}

fn pod_name(name: &str, ordinal: i32) -> String {
    format!("{}-{ordinal}", sts_name(name))
}

fn pvc_name(name: &str, ordinal: i32) -> String {
    format!("kanidm-data-{}-{ordinal}", sts_name(name))
}

async fn create_tls_secret(client: &Client, name: &str) {
    let api = Api::<Secret>::namespaced(client.clone(), NAMESPACE);
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(format!("{name}-tls")),
            namespace: Some(NAMESPACE.to_string()),
            ..Default::default()
        },
        data: Some(BTreeMap::from([
            ("tls.crt".to_string(), ByteString(CERT.to_vec())),
            ("tls.key".to_string(), ByteString(KEY.to_vec())),
        ])),
        type_: Some("kubernetes.io/tls".to_string()),
        ..Default::default()
    };
    api.create(&PostParams::default(), &secret).await.unwrap();
}

async fn create_kanidm(client: &Client, name: &str, replicas: i32) -> Kanidm {
    create_tls_secret(client, name).await;
    let image = format!(
        "kanidm/server:{}",
        get_dependency_version().expect("Kanidm dependency version")
    );
    let spec = serde_json::from_value(json!({
        "domain": "idm.example.com",
        "image": image,
        "replicaGroups": [{
            "name": REPLICA_GROUP,
            "replicas": replicas,
            "primaryNode": true
        }],
        "storage": {
            "volumeClaimTemplate": {
                "spec": {
                    "accessModes": ["ReadWriteOnce"],
                    "storageClassName": "standard",
                    "resources": {"requests": {"storage": "1Gi"}}
                }
            }
        }
    }))
    .unwrap();
    let api = Api::<Kanidm>::namespaced(client.clone(), NAMESPACE);
    let kanidm = api
        .create(&PostParams::default(), &Kanidm::new(name, spec))
        .await
        .unwrap();

    let sts_api = Api::<StatefulSet>::namespaced(client.clone(), NAMESPACE);
    let statefulset_name = sts_name(name);
    wait_until("Kanidm StatefulSet readiness", || {
        let api = sts_api.clone();
        let statefulset_name = statefulset_name.clone();
        async move {
            let sts = api.get(&statefulset_name).await.ok()?;
            let status = sts.status.as_ref()?;
            (status.ready_replicas == Some(replicas)).then_some(())
        }
    })
    .await;
    kanidm
}

async fn trigger_backup(client: &Client, name: &str) -> String {
    let backup_name = format!("backup-{}.json.gz", uuid::Uuid::new_v4());
    let backup_path = format!("/data/backups/{backup_name}");
    let pod_api = Api::<Pod>::namespaced(client.clone(), NAMESPACE);
    let _ = pod_api
        .exec(
            &pod_name(name, 0),
            vec![
                "mkdir".to_string(),
                "-p".to_string(),
                "/data/backups".to_string(),
            ],
            &AttachParams::default().container("kanidm"),
        )
        .await
        .unwrap();
    let attached = pod_api
        .exec(
            &pod_name(name, 0),
            vec![
                "kanidmd".to_string(),
                "database".to_string(),
                "backup".to_string(),
                backup_path,
            ],
            &AttachParams::default().container("kanidm"),
        )
        .await
        .unwrap();
    get_output(attached).await.unwrap();
    backup_name
}

fn create_restore(name: &str, kanidm: &Kanidm, backup_name: &str) -> KanidmRestore {
    let mut restore = KanidmRestore::new(
        name,
        KanidmRestoreSpec {
            target_ref: KanidmRestoreTargetRef {
                name: kanidm.name_any(),
                uid: kanidm.uid().unwrap(),
            },
            source: KanidmRestoreSource {
                local: Some(KanidmRestoreLocalSource {
                    file_name: backup_name.to_string(),
                }),
                backup_ref: None,
            },
            restore_image: kanidm.spec.image.clone(),
            safety_backup: Some(SafetyBackupConfig {
                repository_ref: None,
                skip: true,
            }),
        },
    );
    restore.metadata.annotations = Some(BTreeMap::from([
        (
            BREAK_GLASS_REASON_ANNOTATION.to_string(),
            "restore hardening e2e".to_string(),
        ),
        (
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "e2e-test-runner".to_string(),
        ),
    ]));
    restore
}

async fn wait_restore_phase(
    api: &Api<KanidmRestore>,
    name: &str,
    phase: KanidmRestorePhase,
) -> KanidmRestore {
    wait_until(&format!("restore phase {phase:?}"), || {
        let api = api.clone();
        let name = name.to_string();
        async move {
            let restore = api.get(&name).await.ok()?;
            restore
                .status
                .as_ref()
                .is_some_and(|status| status.phase == phase)
                .then_some(restore)
        }
    })
    .await
}

async fn wait_job_succeeded(api: &Api<Job>, name: &str) {
    wait_until("helper Job completion", || {
        let api = api.clone();
        let name = name.to_string();
        async move {
            api.get(&name)
                .await
                .ok()?
                .status
                .as_ref()
                .is_some_and(|status| status.succeeded == Some(1))
                .then_some(())
        }
    })
    .await;
}

async fn force_delete_configmap(api: &Api<ConfigMap>, name: &str) {
    api.delete(name, &DeleteParams::default()).await.ok();
    wait_until(&format!("ConfigMap {name} deletion"), || {
        let api = api.clone();
        let name = name.to_string();
        async move { api.get(&name).await.is_err().then_some(()) }
    })
    .await;
}

async fn force_delete_job(api: &Api<Job>, name: &str) {
    api.delete(name, &DeleteParams::default()).await.ok();
    wait_until(&format!("Job {name} deletion"), || {
        let api = api.clone();
        let name = name.to_string();
        async move { api.get(&name).await.is_err().then_some(()) }
    })
    .await;
}

async fn write_wrong_domain_backup(client: &Client, name: &str, backup_name: &str) {
    let pod_api = Api::<Pod>::namespaced(client.clone(), NAMESPACE);
    let primary = pod_api.get(&pod_name(name, 0)).await.unwrap();
    let node_name = primary.spec.unwrap().node_name.unwrap();

    let configmap_name = format!("{name}-wrong-domain-fixture");
    let cm_api = Api::<ConfigMap>::namespaced(client.clone(), NAMESPACE);
    force_delete_configmap(&cm_api, &configmap_name).await;
    let payload = json!({
        "version": "1.11.1",
        "entries": [{
            "ent": {"V3": {
                "changestate": {},
                "attrs": {
                    "uuid": {"UU": [DOMAIN_INFO_UUID]},
                    "domain_name": {"N8": ["wrong.example.com"]}
                }
            }}
        }]
    })
    .to_string();
    cm_api
        .create(
            &PostParams::default(),
            &ConfigMap {
                metadata: ObjectMeta {
                    name: Some(configmap_name.clone()),
                    namespace: Some(NAMESPACE.to_string()),
                    ..Default::default()
                },
                data: Some(BTreeMap::from([("backup.json".to_string(), payload)])),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let job_name = format!("{name}-wrong-domain-writer");
    let job_api = Api::<Job>::namespaced(client.clone(), NAMESPACE);
    force_delete_job(&job_api, &job_name).await;
    let job: Job = serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {"name": job_name, "namespace": NAMESPACE},
        "spec": {
            "backoffLimit": 0,
            "template": {
                "spec": {
                    "nodeName": node_name,
                    "restartPolicy": "Never",
                    "containers": [{
                        "name": "writer",
                        "image": BUSYBOX_IMAGE,
                        "command": ["sh", "-c", format!("mkdir -p /data/backups && gzip -c /fixture/backup.json > /data/backups/{backup_name}")],
                        "volumeMounts": [
                            {"name": "data", "mountPath": "/data"},
                            {"name": "fixture", "mountPath": "/fixture"}
                        ]
                    }],
                    "volumes": [
                        {"name": "data", "persistentVolumeClaim": {"claimName": pvc_name(name, 0)}},
                        {"name": "fixture", "configMap": {"name": configmap_name}}
                    ]
                }
            }
        }
    }))
    .unwrap();
    job_api.create(&PostParams::default(), &job).await.unwrap();
    wait_job_succeeded(&job_api, &job_name).await;
    job_api
        .delete(&job_name, &DeleteParams::default())
        .await
        .ok();
    cm_api
        .delete(&configmap_name, &DeleteParams::default())
        .await
        .ok();
}

async fn create_completed_pvc_holder(client: &Client, name: &str) -> String {
    let pod_api = Api::<Pod>::namespaced(client.clone(), NAMESPACE);
    let secondary = pod_api.get(&pod_name(name, 1)).await.unwrap();
    let node_name = secondary.spec.unwrap().node_name.unwrap();
    let holder_name = format!("{name}-secondary-pvc-holder");
    let holder: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": holder_name, "namespace": NAMESPACE},
        "spec": {
            "nodeName": node_name,
            "restartPolicy": "Never",
            "containers": [{
                "name": "holder",
                "image": BUSYBOX_IMAGE,
                "command": ["sh", "-c", "true"],
                "volumeMounts": [{"name": "data", "mountPath": "/data"}]
            }],
            "volumes": [{
                "name": "data",
                "persistentVolumeClaim": {"claimName": pvc_name(name, 1)}
            }]
        }
    }))
    .unwrap();
    pod_api
        .create(&PostParams::default(), &holder)
        .await
        .unwrap();
    wait_until("completed PVC holder", || {
        let api = pod_api.clone();
        let holder_name = holder_name.clone();
        async move {
            let pod = api.get(&holder_name).await.ok()?;
            (pod.status.as_ref()?.phase.as_deref() == Some("Succeeded")).then_some(())
        }
    })
    .await;
    holder_name
}

async fn wait_replication_success(client: &Client, name: &str, replicas: i32) {
    let pod_api = Api::<Pod>::namespaced(client.clone(), NAMESPACE);
    let deadline = Instant::now() + REPLICATION_TIMEOUT;
    let mut observed = vec![false; replicas as usize];
    while !observed.iter().all(|success| *success) {
        for ordinal in 0..replicas {
            if observed[ordinal as usize] {
                continue;
            }
            let logs = pod_api
                .logs(&pod_name(name, ordinal), &Default::default())
                .await
                .unwrap_or_default();
            if logs.contains("Incremental Replication Success") {
                observed[ordinal as usize] = true;
            }
        }
        assert!(
            Instant::now() < deadline,
            "replication success not observed on all restored replicas"
        );
        sleep(Duration::from_secs(10)).await;
    }
}

#[tokio::test]
#[serial(restore)]
async fn restore_local_domain_mismatch_fails_before_mutation() {
    init_crypto();
    let client = Client::try_default().await.unwrap();
    let name = "test-restore-domain-mismatch-hardening";
    let kanidm = create_kanidm(&client, name, 1).await;
    let backup_name = "wrong-domain.json.gz";
    write_wrong_domain_backup(&client, name, backup_name).await;

    let restore_name = format!("{name}-restore");
    let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), NAMESPACE);
    restore_api
        .create(
            &PostParams::default(),
            &create_restore(&restore_name, &kanidm, backup_name),
        )
        .await
        .unwrap();

    let failed = wait_restore_phase(&restore_api, &restore_name, KanidmRestorePhase::Failed).await;
    let status = failed.status.unwrap();
    assert!(!status.database_mutation_started);
    assert!(
        status
            .message
            .as_deref()
            .is_some_and(|message| message.contains("local backup preflight failed"))
    );

    let sts_api = Api::<StatefulSet>::namespaced(client.clone(), NAMESPACE);
    let statefulset_name = sts_name(name);
    wait_until("service resumed after preflight rejection", || {
        let api = sts_api.clone();
        let statefulset_name = statefulset_name.clone();
        async move {
            let sts = api.get(&statefulset_name).await.ok()?;
            (sts.status.as_ref()?.ready_replicas == Some(1)).then_some(())
        }
    })
    .await;
}

#[tokio::test]
#[serial(restore)]
async fn restore_secondary_pvc_blocker_is_reported_and_times_out() {
    init_crypto();
    let client = Client::try_default().await.unwrap();
    let name = "test-restore-pvc-blocker-hardening";
    let kanidm = create_kanidm(&client, name, 2).await;
    let backup_name = trigger_backup(&client, name).await;
    let holder_name = create_completed_pvc_holder(&client, name).await;

    let restore_name = format!("{name}-restore");
    let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), NAMESPACE);
    let created = restore_api
        .create(
            &PostParams::default(),
            &create_restore(&restore_name, &kanidm, &backup_name),
        )
        .await
        .unwrap();
    let restore_uid = created.uid().unwrap();

    let blocked = wait_until("ReplicaCleanupBlocked condition", || {
        let api = restore_api.clone();
        let restore_name = restore_name.clone();
        let holder_name = holder_name.clone();
        async move {
            let restore = api.get(&restore_name).await.ok()?;
            let status = restore.status.as_ref()?;
            let condition = status.conditions.iter().find(|condition| {
                condition.type_ == "ReplicaCleanupBlocked" && condition.status == "True"
            })?;
            (condition.message.contains(&holder_name)
                && condition.message.contains(&pvc_name(name, 1)))
            .then_some(restore)
        }
    })
    .await;
    assert!(blocked.status.unwrap().database_mutation_started);

    restore_api
        .patch_status(
            &restore_name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "status": {
                    "phaseTimestamps": {
                        "RebuildingReplicas": "2000-01-01T00:00:00Z"
                    }
                }
            })),
        )
        .await
        .unwrap();

    let failed = wait_restore_phase(&restore_api, &restore_name, KanidmRestorePhase::Failed).await;
    assert!(
        failed
            .status
            .as_ref()
            .and_then(|status| status.message.as_deref())
            .is_some_and(|message| message.contains("timed out waiting for secondary PVC deletion"))
    );

    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), NAMESPACE);
    let target = kanidm_api.get(name).await.unwrap();
    assert_eq!(
        target.annotations().get(RESTORE_ANNOTATION),
        Some(&restore_uid),
        "post-mutation timeout must retain the restore lock"
    );

    Api::<Pod>::namespaced(client.clone(), NAMESPACE)
        .delete(&holder_name, &DeleteParams::default())
        .await
        .ok();
}

#[tokio::test]
#[serial(restore)]
async fn restore_completed_implies_ha_topology_and_replication_ready() {
    init_crypto();
    let client = Client::try_default().await.unwrap();
    let name = "test-restore-ha-completion-hardening";
    let kanidm = create_kanidm(&client, name, 2).await;
    wait_replication_success(&client, name, 2).await;

    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), NAMESPACE);
    let secondary_pvc = pvc_name(name, 1);
    let old_secondary_uid = pvc_api.get(&secondary_pvc).await.unwrap().uid().unwrap();
    let backup_name = trigger_backup(&client, name).await;

    let restore_name = format!("{name}-restore");
    let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), NAMESPACE);
    restore_api
        .create(
            &PostParams::default(),
            &create_restore(&restore_name, &kanidm, &backup_name),
        )
        .await
        .unwrap();

    wait_restore_phase(&restore_api, &restore_name, KanidmRestorePhase::Completed).await;

    let sts_api = Api::<StatefulSet>::namespaced(client.clone(), NAMESPACE);
    let sts = sts_api.get(&sts_name(name)).await.unwrap();
    let spec = sts.spec.as_ref().unwrap();
    let status = sts.status.as_ref().unwrap();
    assert_eq!(spec.replicas, Some(2));
    assert_eq!(status.ready_replicas, Some(2));
    assert_eq!(status.updated_replicas, Some(2));
    assert_eq!(status.current_revision, status.update_revision);

    let new_secondary_uid = pvc_api.get(&secondary_pvc).await.unwrap().uid().unwrap();
    assert_ne!(
        old_secondary_uid, new_secondary_uid,
        "secondary PVC must be reprovisioned from empty state during restore"
    );

    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), NAMESPACE);
    let target = kanidm_api.get(name).await.unwrap();
    assert!(target.annotations().get(RESTORE_ANNOTATION).is_none());

    wait_replication_success(&client, name, 2).await;
}

const MINIO_ENDPOINT: &str = "https://minio.default.svc:9000";
const MINIO_BUCKET: &str = "kaniop-backups";
const MINIO_REGION: &str = "us-east-1";
const MINIO_CA_CM: &str = "minio-ca";
const MINIO_CREDS_SECRET: &str = "minio-creds";

fn data_mover_image() -> String {
    std::env::var("DATA_MOVER_IMAGE").unwrap_or_else(|_| {
        format!(
            "ghcr.io/pando85/kaniop-data-mover:{}",
            option_env!("GIT_SHA").unwrap_or("aed6d7e")
        )
    })
}

fn minio_s3_config(prefix: &str) -> kaniop_backup_core::crd::S3Config {
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

fn minio_auth(secret_name: &str) -> kaniop_backup_core::crd::RepositoryAuthentication {
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

async fn create_backup_repository(client: &Client, name: &str, prefix: &str) {
    let api = Api::<KanidmBackupRepository>::namespaced(client.clone(), NAMESPACE);
    api.delete(name, &DeleteParams::default()).await.ok();
    let repo = KanidmBackupRepository::new(
        name,
        KanidmBackupRepositorySpec {
            s3: minio_s3_config(prefix),
            authentication: minio_auth(MINIO_CREDS_SECRET),
            encryption: None,
            limits: None,
        },
    );
    api.create(&PostParams::default(), &repo).await.unwrap();
    wait_until("repository ready", || {
        let api = api.clone();
        let name = name.to_string();
        async move {
            let repo = api.get(&name).await.ok()?;
            repo.status
                .as_ref()
                .is_some_and(|status| {
                    status
                        .conditions
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "True" && c.reason == "Accepted")
                })
                .then_some(())
        }
    })
    .await;
}

async fn upload_backup_to_s3_remote(
    client: &Client,
    kanidm_name: &str,
    prefix: &str,
    backup_name: &str,
    backup_id: &str,
    kanidm_uid: &str,
    domain: &str,
) -> String {
    let namespace_api: Api<Namespace> = Api::all(client.clone());
    let default_ns = namespace_api.get("default").await.unwrap();
    let namespace_uid = default_ns.metadata.uid.unwrap();

    let manifest_key = format!(
        "{prefix}/v1/tenants/{namespace_uid}/clusters/{kanidm_uid}/backups/{backup_id}/manifest.json"
    );

    let operation_doc = serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "upload",
        "payloadPath": format!("/data/backups/{backup_name}"),
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

    let sts = format!("{kanidm_name}-{REPLICA_GROUP}");
    let pvc = format!("kanidm-data-{sts}-0");
    let op_cm_name = format!("{kanidm_name}-upload-op");

    let cm_api = Api::<ConfigMap>::namespaced(client.clone(), NAMESPACE);
    force_delete_configmap(&cm_api, &op_cm_name).await;
    let op_cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(op_cm_name.clone()),
            namespace: Some(NAMESPACE.to_string()),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            "operation.json".to_string(),
            serde_json::to_string(&operation_doc).unwrap(),
        )])),
        ..Default::default()
    };
    cm_api.create(&PostParams::default(), &op_cm).await.unwrap();

    let job_api = Api::<Job>::namespaced(client.clone(), NAMESPACE);
    let upload_job_name = format!("{kanidm_name}-upload");
    force_delete_job(&job_api, &upload_job_name).await;

    let env_vars: Vec<serde_json::Value> = vec![
        serde_json::from_value(serde_json::json!({"name": "AWS_ACCESS_KEY_ID", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_ACCESS_KEY_ID"}}})).unwrap(),
        serde_json::from_value(serde_json::json!({"name": "AWS_SECRET_ACCESS_KEY", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_SECRET_ACCESS_KEY"}}})).unwrap(),
        serde_json::from_value(serde_json::json!({"name": "RUST_LOG", "value": "info"})).unwrap(),
        serde_json::from_value(serde_json::json!({"name": "SSL_CERT_FILE", "value": "/run/kaniop-ca-bundle/ca-bundle.pem"})).unwrap(),
    ];

    let upload_job: Job = serde_json::from_value(serde_json::json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": upload_job_name,
            "namespace": NAMESPACE
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
                        {"name": "data", "persistentVolumeClaim": {"claimName": pvc}},
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

    wait_job_succeeded(&job_api, &upload_job_name).await;

    job_api
        .delete(&upload_job_name, &DeleteParams::default())
        .await
        .ok();
    cm_api
        .delete(&op_cm_name, &DeleteParams::default())
        .await
        .ok();

    manifest_key
}

async fn create_backup_cr_and_wait_remote(
    client: &Client,
    backup_id: &str,
    kanidm_name: &str,
    kanidm_uid: &str,
    repo_name: &str,
    manifest_key: &str,
) -> String {
    let backup_cr_name = format!("kb-{}", &backup_id[..8]);
    let backup_cr = KanidmBackup {
        metadata: ObjectMeta {
            name: Some(backup_cr_name.clone()),
            namespace: Some(NAMESPACE.to_string()),
            labels: Some(BTreeMap::from([
                ("kaniop.rs/backup-id".to_string(), backup_id.to_string()),
                ("kaniop.rs/repository".to_string(), repo_name.to_string()),
            ])),
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
    let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), NAMESPACE);
    backup_api
        .create(&PostParams::default(), &backup_cr)
        .await
        .unwrap();
    wait_until("backup CR ready", || {
        let api = backup_api.clone();
        let name = backup_cr_name.clone();
        async move {
            let backup = api.get(&name).await.ok()?;
            backup
                .status
                .as_ref()
                .is_some_and(|s| s.phase == KanidmBackupPhase::Ready)
                .then_some(())
        }
    })
    .await;
    backup_cr_name
}

fn create_remote_restore(
    name: &str,
    kanidm: &Kanidm,
    backup_cr_name: &str,
    repo_name: &str,
) -> KanidmRestore {
    let mut restore = KanidmRestore::new(
        name,
        KanidmRestoreSpec {
            target_ref: KanidmRestoreTargetRef {
                name: kanidm.name_any(),
                uid: kanidm.uid().unwrap(),
            },
            source: KanidmRestoreSource {
                local: None,
                backup_ref: Some(KanidmRestoreBackupRefSource {
                    name: backup_cr_name.to_string(),
                }),
            },
            restore_image: kanidm.spec.image.clone(),
            safety_backup: Some(SafetyBackupConfig {
                repository_ref: Some(SafetyBackupRepositoryRef {
                    name: repo_name.to_string(),
                }),
                skip: false,
            }),
        },
    );
    restore.metadata.annotations = Some(BTreeMap::from([
        (
            BREAK_GLASS_REASON_ANNOTATION.to_string(),
            "restore hardening remote HA e2e".to_string(),
        ),
        (
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "e2e-test-runner".to_string(),
        ),
    ]));
    restore
}

#[tokio::test]
#[serial(restore)]
async fn restore_remote_ha_round_trip() {
    init_crypto();
    let client = Client::try_default().await.unwrap();
    let name = "test-restore-remote-ha-hardening";
    let repo_name = format!("{name}-repo");

    let kanidm = create_kanidm(&client, name, 2).await;
    wait_replication_success(&client, name, 2).await;

    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), NAMESPACE);
    let secondary_pvc = pvc_name(name, 1);
    let old_secondary_uid = pvc_api.get(&secondary_pvc).await.unwrap().uid().unwrap();

    let backup_name = trigger_backup(&client, name).await;
    let kanidm_uid = kanidm.uid().unwrap();
    let domain = kanidm.spec.domain.clone();

    create_backup_repository(&client, &repo_name, "e2e-remote-ha-hardening").await;

    let backup_id = uuid::Uuid::new_v4().to_string();
    let manifest_key = upload_backup_to_s3_remote(
        &client,
        name,
        "e2e-remote-ha-hardening",
        &backup_name,
        &backup_id,
        &kanidm_uid,
        &domain,
    )
    .await;

    let backup_cr_name = create_backup_cr_and_wait_remote(
        &client,
        &backup_id,
        name,
        &kanidm_uid,
        &repo_name,
        &manifest_key,
    )
    .await;

    let restore_name = format!("{name}-restore");
    let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), NAMESPACE);
    restore_api
        .create(
            &PostParams::default(),
            &create_remote_restore(&restore_name, &kanidm, &backup_cr_name, &repo_name),
        )
        .await
        .unwrap();

    wait_restore_phase(&restore_api, &restore_name, KanidmRestorePhase::Completed).await;

    let sts_api = Api::<StatefulSet>::namespaced(client.clone(), NAMESPACE);
    let sts = sts_api.get(&sts_name(name)).await.unwrap();
    let spec = sts.spec.as_ref().unwrap();
    let status = sts.status.as_ref().unwrap();
    assert_eq!(spec.replicas, Some(2));
    assert_eq!(status.ready_replicas, Some(2));
    assert_eq!(status.updated_replicas, Some(2));
    assert_eq!(status.current_revision, status.update_revision);

    let new_secondary_uid = pvc_api.get(&secondary_pvc).await.unwrap().uid().unwrap();
    assert_ne!(
        old_secondary_uid, new_secondary_uid,
        "secondary PVC must be reprovisioned from empty state during remote restore"
    );

    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), NAMESPACE);
    let target = kanidm_api.get(name).await.unwrap();
    assert!(
        target.annotations().get(RESTORE_ANNOTATION).is_none(),
        "restore annotation should be cleared after remote HA restore"
    );

    wait_replication_success(&client, name, 2).await;

    let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), NAMESPACE);
    backup_api
        .delete(&backup_cr_name, &DeleteParams::default())
        .await
        .ok();
    let repo_api = Api::<KanidmBackupRepository>::namespaced(client.clone(), NAMESPACE);
    repo_api
        .delete(&repo_name, &DeleteParams::default())
        .await
        .ok();
}
