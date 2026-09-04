use serial_test::serial;

use super::{
    DEFAULT_REPLICA_GROUP_NAME, KANIDM_DEFAULT_SPEC_JSON, MINIO_CREDS_SECRET,
    STORAGE_VOLUME_CLAIM_TEMPLATE_JSON, create_repository, force_delete_and_wait, is_kanidm,
    is_repo_ready,
};
use crate::test::{init_crypto_provider, poll_until, wait_for as test_wait_for};

use kaniop_backup_core::crd::{
    KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule, KanidmBackupScheduleSpec,
    ScheduleKanidmRef, ScheduleRepositoryRef,
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

const TRANSPORT_SIDECAR_NAME: &str = "data-mover-transport";

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
        create_repository(&client, repo_name, "e2e-transport", MINIO_CREDS_SECRET).await;
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

        poll_until("transport native sidecar appears in StatefulSet", || {
            let sts_api = sts_api.clone();
            let sts_name = sts_name.clone();
            async move {
                let sts = sts_api.get(&sts_name).await.ok()?;
                let pod_spec = sts.spec.as_ref()?.template.spec.as_ref()?;
                let has_sidecar = pod_spec
                    .init_containers
                    .as_ref()?
                    .iter()
                    .any(|c| c.name == TRANSPORT_SIDECAR_NAME);
                if has_sidecar { Some(()) } else { None }
            }
        })
        .await;

        let sts = sts_api.get(&sts_name).await.unwrap();
        let pod_spec = sts
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap();
        assert!(
            pod_spec
                .containers
                .iter()
                .all(|c| c.name != TRANSPORT_SIDECAR_NAME),
            "transport must not be a regular application container"
        );
        let sidecar = pod_spec
            .init_containers
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.name == TRANSPORT_SIDECAR_NAME)
            .expect("transport native sidecar should be present");
        assert_eq!(sidecar.restart_policy.as_deref(), Some("Always"));
        assert!(
            sidecar.readiness_probe.is_none(),
            "transport sidecar readiness must not gate Kanidm Pod readiness"
        );
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

e2e_test!(
    #[serial(backup)]
    backup_transport_non_primary_pod_idles_without_restarts,
    {
        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        let kanidm_name = "test-transport-np";
        let repo_name = "test-transport-np-repo";
        let schedule_name = "test-transport-np-schedule";

        cleanup_transport_resources(&client, kanidm_name, repo_name).await;

        let mut spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        merge(&mut spec_json, &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone());
        merge(
            &mut spec_json,
            &json!({"replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 2, "primaryNode": true}]
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
        create_repository(&client, repo_name, "e2e-transport-np", MINIO_CREDS_SECRET).await;
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
                schedule: "*/5 * * * *".to_string(),
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

        poll_until("transport native sidecar appears in StatefulSet", || {
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
                    .init_containers
                    .as_ref()?
                    .iter()
                    .any(|c| c.name == TRANSPORT_SIDECAR_NAME);
                if has_sidecar { Some(()) } else { None }
            }
        })
        .await;

        let pod_api = Api::<Pod>::namespaced(client.clone(), "default");
        let non_primary_pod = format!("{sts_name}-1");

        let timeout = Duration::from_secs(300);
        let start = std::time::Instant::now();
        loop {
            let pod = pod_api.get(&non_primary_pod).await.unwrap();
            let sidecar_status = pod
                .status
                .as_ref()
                .and_then(|s| s.init_container_statuses.as_ref())
                .and_then(|statuses| statuses.iter().find(|cs| cs.name == TRANSPORT_SIDECAR_NAME));
            let pod_ready = pod
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .is_some_and(|conditions| {
                    conditions
                        .iter()
                        .any(|condition| condition.type_ == "Ready" && condition.status == "True")
                });

            if let Some(cs) = sidecar_status {
                if cs.ready && cs.restart_count == 0 && pod_ready {
                    break;
                }
            }

            if start.elapsed() > timeout {
                let logs = pod_api
                    .logs(
                        &non_primary_pod,
                        &LogParams {
                            container: Some(TRANSPORT_SIDECAR_NAME.to_string()),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap_or_default();
                panic!(
                    "Timeout waiting for non-primary native sidecar and Ready Kanidm Pod. Logs:\n{logs}"
                );
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        let primary_pod = format!("{sts_name}-0");
        let primary_sidecar_ok = poll_until("primary native sidecar and Pod are ready", || {
            let pod_api = pod_api.clone();
            let primary_pod = primary_pod.clone();
            async move {
                let pod = pod_api.get(&primary_pod).await.ok()?;
                let status = pod.status.as_ref()?;
                let cs = status
                    .init_container_statuses
                    .as_ref()?
                    .iter()
                    .find(|cs| cs.name == TRANSPORT_SIDECAR_NAME)?;
                let pod_ready = status.conditions.as_ref()?.iter().any(|condition| {
                    condition.type_ == "Ready" && condition.status == "True"
                });
                if cs.ready && cs.restart_count == 0 && pod_ready {
                    Some(())
                } else {
                    None
                }
            }
        });
        tokio::time::timeout(Duration::from_secs(300), primary_sidecar_ok)
            .await
            .expect("primary native sidecar and Kanidm Pod should become ready");

        cleanup_transport_resources(&client, kanidm_name, repo_name).await;
        let secret_api_cleanup =
            Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), "default");
        force_delete_and_wait(secret_api_cleanup, &format!("{kanidm_name}-tls")).await;
    }
);
