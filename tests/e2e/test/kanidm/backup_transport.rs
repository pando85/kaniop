use serial_test::serial;

use super::{
    DEFAULT_REPLICA_GROUP_NAME, KANIDM_DEFAULT_SPEC_JSON, STORAGE_VOLUME_CLAIM_TEMPLATE_JSON,
    is_kanidm,
};
use crate::test::{init_crypto_provider, poll_until, wait_for as test_wait_for};

use kaniop_backup_core::crd::{
    AuthMethod, KanidmBackup, KanidmBackupRepository, KanidmBackupRepositorySpec,
    KanidmBackupSchedule, KanidmBackupScheduleSpec, RepositoryAuthentication, S3Config,
    ScheduleKanidmRef, ScheduleRepositoryRef, SecretRef,
};
use kaniop_operator::kanidm::crd::Kanidm;

use json_patch::merge;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;
use kube::api::{Api, LogParams, PostParams};
use kube::client::Client;
use serde_json::json;
use std::time::Duration;

const MINIO_ENDPOINT: &str = "https://minio.default.svc:9000";
const MINIO_BUCKET: &str = "kaniop-backups";
const MINIO_REGION: &str = "us-east-1";
const MINIO_CA_CM: &str = "minio-ca";
const MINIO_CREDS_SECRET: &str = "minio-creds";

const TRANSPORT_SIDECAR_NAME: &str = "data-mover-transport";

fn minio_s3_config(prefix: &str) -> S3Config {
    S3Config {
        bucket: MINIO_BUCKET.to_string(),
        prefix: prefix.to_string(),
        region: Some(MINIO_REGION.to_string()),
        endpoint: Some(MINIO_ENDPOINT.to_string()),
        force_path_style: true,
        insecure: false,
        ca_bundle_ref: Some(MINIO_CA_CM.to_string()),
    }
}

fn minio_auth(secret_name: &str) -> RepositoryAuthentication {
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

async fn force_delete_and_wait<K>(api: Api<K>, name: &str)
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
    poll_until(&format!("{name} deleted"), || {
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

async fn cleanup_transport_resources(client: &Client, kanidm_name: &str, repo_name: &str) {
    let ns = "default";

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

    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), ns);
    force_delete_and_wait(kanidm_api, kanidm_name).await;
}

fn is_repo_ready() -> impl kube::runtime::wait::Condition<KanidmBackupRepository> {
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

e2e_test!(
    #[serial(backup)]
    backup_transport_sidecar_uploads_and_discovery_creates_backup,
    {
        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        let kanidm_name = "test-transport-kanidm";
        let repo_name = "test-transport-repo";
        let schedule_name = "test-transport-schedule";

        cleanup_transport_resources(&client, kanidm_name, repo_name).await;

        let mut spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        merge(&mut spec_json, &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone());
        merge(
            &mut spec_json,
            &json!({"replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            }),
        );
        let kanidm = Kanidm::new(kanidm_name, serde_json::from_value(spec_json).unwrap());
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        kanidm_api
            .create(&PostParams::default(), &kanidm)
            .await
            .unwrap();

        let secret_api =
            Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), "default");
        let mut data = std::collections::BTreeMap::new();
        data.insert(
            "tls.crt".to_string(),
            k8s_openapi::ByteString(super::CERT.to_vec()),
        );
        data.insert(
            "tls.key".to_string(),
            k8s_openapi::ByteString(super::KEY.to_vec()),
        );
        let secret = k8s_openapi::api::core::v1::Secret {
            metadata: kube::api::ObjectMeta {
                name: Some(format!("{kanidm_name}-tls")),
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

        test_wait_for(kanidm_api.clone(), kanidm_name, is_kanidm("Available")).await;

        let repo_api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
        force_delete_and_wait(repo_api.clone(), repo_name).await;
        let repo = KanidmBackupRepository::new(
            repo_name,
            KanidmBackupRepositorySpec {
                s3: minio_s3_config("e2e-transport"),
                authentication: minio_auth(MINIO_CREDS_SECRET),
                encryption: None,
                limits: None,
            },
        );
        repo_api
            .create(&PostParams::default(), &repo)
            .await
            .unwrap();
        test_wait_for(repo_api.clone(), repo_name, is_repo_ready()).await;

        let schedule_api = Api::<KanidmBackupSchedule>::namespaced(client.clone(), "default");
        let schedule = KanidmBackupSchedule::new(
            schedule_name,
            KanidmBackupScheduleSpec {
                kanidm_ref: ScheduleKanidmRef {
                    name: kanidm_name.to_string(),
                },
                repository_ref: ScheduleRepositoryRef {
                    name: repo_name.to_string(),
                },
                schedule: "*/2 * * * *".to_string(),
                time_zone: "UTC".to_string(),
                suspend: false,
                concurrency_policy: "Forbid".to_string(),
                jitter_seconds: None,
                local_versions: 3,
                retention: None,
            },
        );
        schedule_api
            .create(&PostParams::default(), &schedule)
            .await
            .unwrap();

        let sts_api = Api::<StatefulSet>::namespaced(client.clone(), "default");
        let sts_name = format!("{kanidm_name}-{DEFAULT_REPLICA_GROUP_NAME}");

        poll_until("transport sidecar appears in StatefulSet", || {
            let sts_api = sts_api.clone();
            let sts_name = sts_name.clone();
            async move {
                let sts = sts_api.get(&sts_name).await.ok()?;
                let has_sidecar = sts
                    .spec
                    .as_ref()?
                    .template
                    .spec
                    .as_ref()?
                    .containers
                    .iter()
                    .any(|c| c.name == TRANSPORT_SIDECAR_NAME);
                if has_sidecar { Some(()) } else { None }
            }
        })
        .await;

        let sts = sts_api.get(&sts_name).await.unwrap();
        let sidecar = sts
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers
            .iter()
            .find(|c| c.name == TRANSPORT_SIDECAR_NAME)
            .expect("transport sidecar should be present");
        assert!(
            sidecar
                .image
                .as_deref()
                .is_some_and(|img| img.contains("kaniop-data-mover")),
            "sidecar image should contain kaniop-data-mover, got: {:?}",
            sidecar.image
        );
        assert!(
            sidecar
                .args
                .as_ref()
                .is_some_and(|args| args.iter().any(|a| a == "transport")),
            "sidecar args should include 'transport', got: {:?}",
            sidecar.args
        );

        let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), "default");
        let discovery_timeout = Duration::from_secs(600);
        let start = std::time::Instant::now();
        loop {
            let list = backup_api.list(&Default::default()).await.unwrap();
            let found = list.items.iter().any(|b| {
                b.spec.kanidm_ref.name == kanidm_name
                    && b.spec.manifest_key.contains("e2e-transport/")
                    && b.spec.manifest_key.ends_with("/manifest.json")
            });
            if found {
                break;
            }
            if start.elapsed() > discovery_timeout {
                panic!("Timeout waiting for discovery to create KanidmBackup CR");
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }

        let schedule_after = schedule_api.get(schedule_name).await.unwrap();
        let first_scan_time = schedule_after
            .status
            .as_ref()
            .and_then(|s| s.discovery.as_ref())
            .and_then(|d| d.last_scan_time.clone());
        assert!(
            first_scan_time.is_some(),
            "schedule status.discovery.lastScanTime should be set"
        );

        tokio::time::sleep(Duration::from_secs(90)).await;

        let schedule_after2 = schedule_api.get(schedule_name).await.unwrap();
        let second_scan_time = schedule_after2
            .status
            .as_ref()
            .and_then(|s| s.discovery.as_ref())
            .and_then(|d| d.last_scan_time.clone());
        assert!(
            second_scan_time.is_some(),
            "schedule status.discovery.lastScanTime should still be set after wait"
        );
        assert_ne!(
            first_scan_time, second_scan_time,
            "lastScanTime should advance between observations"
        );

        let pod_api = Api::<Pod>::namespaced(client.clone(), "default");
        let primary_pod = format!("{sts_name}-0");
        poll_until("transport sidecar logs successful upload", || {
            let pod_api = pod_api.clone();
            let primary_pod = primary_pod.clone();
            async move {
                let logs = pod_api
                    .logs(
                        &primary_pod,
                        &LogParams {
                            container: Some(TRANSPORT_SIDECAR_NAME.to_string()),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap_or_default();
                if logs.contains("backup uploaded successfully") {
                    Some(())
                } else {
                    None
                }
            }
        })
        .await;

        cleanup_transport_resources(&client, kanidm_name, repo_name).await;
        let secret_api_cleanup =
            Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), "default");
        force_delete_and_wait(secret_api_cleanup, &format!("{kanidm_name}-tls")).await;
    }
);
