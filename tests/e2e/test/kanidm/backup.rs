use serial_test::serial;

use super::{
    DEFAULT_REPLICA_GROUP_NAME, KANIDM_DEFAULT_SPEC_JSON, STORAGE_VOLUME_CLAIM_TEMPLATE_JSON,
    is_kanidm, setup,
};
use crate::test::{init_crypto_provider, poll_until, wait_for as test_wait_for};

use kaniop_backup_core::crd::{
    AuthMethod, BackupKanidmRef, BackupRepositoryRef, KanidmBackup, KanidmBackupPhase,
    KanidmBackupRepository, KanidmBackupRepositorySpec, KanidmBackupSchedule,
    KanidmBackupScheduleSpec, KanidmBackupSpec, RepositoryAuthentication, S3Config,
    ScheduleKanidmRef, ScheduleRepositoryRef, SecretRef,
};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::{
    BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION, KanidmRestore,
    KanidmRestoreBackupRefSource, KanidmRestoreLocalSource, KanidmRestorePhase,
    KanidmRestoreSource, KanidmRestoreSpec, KanidmRestoreTargetRef, SafetyBackupConfig,
    SafetyBackupRepositoryRef,
};

use json_patch::merge;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;
use kube::api::{Api, PostParams};
use kube::client::Client;
use serde_json::json;
use std::time::Duration;

const MINIO_ENDPOINT: &str = "https://minio.default.svc:9000";
const MINIO_BUCKET: &str = "kaniop-backups";
const MINIO_REGION: &str = "us-east-1";
const MINIO_CA_CM: &str = "minio-ca";
const MINIO_CREDS_SECRET: &str = "minio-creds";
const MINIO_CREDS_INVALID_SECRET: &str = "minio-creds-invalid";

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

async fn cleanup_test_resources(client: &Client, test_name: &str, repo_name: &str) {
    let ns = "default";

    let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), ns);
    force_delete_and_wait(restore_api, &format!("{test_name}-restore")).await;

    let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), ns);
    if let Ok(list) = backup_api.list(&Default::default()).await {
        for backup in list.items {
            if backup.spec.kanidm_ref.name == test_name {
                force_delete_and_wait(backup_api.clone(), &backup.name_any()).await;
            }
        }
    }

    let schedule_api = Api::<KanidmBackupSchedule>::namespaced(client.clone(), ns);
    if let Ok(list) = schedule_api.list(&Default::default()).await {
        for schedule in list.items {
            if schedule.spec.kanidm_ref.name == test_name {
                force_delete_and_wait(schedule_api.clone(), &schedule.name_any()).await;
            }
        }
    }

    let repo_api = Api::<KanidmBackupRepository>::namespaced(client.clone(), ns);
    force_delete_and_wait(repo_api, repo_name).await;

    let job_api = Api::<Job>::namespaced(client.clone(), ns);
    force_delete_and_wait(job_api.clone(), &format!("{test_name}-upload")).await;
    force_delete_and_wait(job_api.clone(), &format!("{test_name}-restore")).await;
    force_delete_and_wait(job_api.clone(), &format!("{test_name}-verify")).await;
    force_delete_and_wait(job_api.clone(), &format!("{test_name}-safety-backup")).await;
    force_delete_and_wait(job_api.clone(), &format!("{test_name}-source-prep")).await;

    let cm_api = Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(client.clone(), ns);
    force_delete_and_wait(cm_api, &format!("{test_name}-upload-op")).await;

    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), ns);
    force_delete_and_wait(kanidm_api, test_name).await;

    let secret_api = Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), ns);
    force_delete_and_wait(secret_api, &format!("{test_name}-tls")).await;
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

fn is_backup_phase(phase: KanidmBackupPhase) -> impl kube::runtime::wait::Condition<KanidmBackup> {
    move |obj: Option<&KanidmBackup>| {
        obj.and_then(|backup| backup.status.as_ref())
            .is_some_and(|status| status.phase == phase)
    }
}

fn is_restore_phase(
    phase: KanidmRestorePhase,
) -> impl kube::runtime::wait::Condition<KanidmRestore> {
    move |obj: Option<&KanidmRestore>| {
        obj.and_then(|restore| restore.status.as_ref())
            .is_some_and(|status| status.phase == phase)
    }
}

#[allow(dead_code)]
fn has_database_mutation_started() -> impl kube::runtime::wait::Condition<KanidmRestore> {
    move |obj: Option<&KanidmRestore>| {
        obj.and_then(|restore| restore.status.as_ref())
            .is_some_and(|status| status.database_mutation_started)
    }
}

fn is_schedule_suspended() -> impl kube::runtime::wait::Condition<KanidmBackupSchedule> {
    move |obj: Option<&KanidmBackupSchedule>| {
        obj.and_then(|schedule| schedule.status.as_ref())
            .is_some_and(|status| {
                status
                    .conditions
                    .iter()
                    .any(|c| c.type_ == "Suspended" && c.status == "True")
            })
    }
}

fn has_break_glass_condition() -> impl kube::runtime::wait::Condition<KanidmRestore> {
    move |obj: Option<&KanidmRestore>| {
        obj.and_then(|restore| restore.status.as_ref())
            .is_some_and(|status| {
                status
                    .conditions
                    .iter()
                    .any(|c| c.type_ == "BreakGlassOverride" && c.status == "True")
            })
    }
}

fn is_deployment_ready() -> impl kube::runtime::wait::Condition<Deployment> {
    move |obj: Option<&Deployment>| {
        obj.and_then(|d| d.status.as_ref()).is_some_and(|s| {
            let desired = s.replicas.unwrap_or(0);
            desired > 0
                && s.ready_replicas == Some(desired)
                && s.updated_replicas == Some(desired)
                && s.available_replicas == Some(desired)
        })
    }
}

async fn wait_for_operator_and_webhook_ready(client: &Client) {
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), "kaniop");
    test_wait_for(deploy_api.clone(), "kaniop", is_deployment_ready()).await;
    test_wait_for(deploy_api, "kaniop-webhook", is_deployment_ready()).await;
}

async fn create_repository(client: &Client, name: &str, prefix: &str, secret: &str) {
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

async fn trigger_backup_on_primary(name: &str, client: &Client) -> String {
    let pod_api = Api::<Pod>::namespaced(client.clone(), "default");
    let primary_pod = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}-0");

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

e2e_test!(
    #[serial(backup)]
    backup_repository_accepted_without_probe_job,
    {
        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        let repo_name = "test-repo-accepted-no-probe";

        cleanup_test_resources(&client, repo_name, repo_name).await;

        create_repository(
            &client,
            repo_name,
            "e2e-accepted-no-probe",
            MINIO_CREDS_SECRET,
        )
        .await;

        let api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
        test_wait_for(api.clone(), repo_name, is_repo_ready()).await;

        let repo = api.get(repo_name).await.unwrap();
        let status = repo.status.unwrap();
        let ready_cond = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready condition should exist");
        assert_eq!(ready_cond.status, "True", "repository should be Ready");
        assert_eq!(
            ready_cond.reason, "Accepted",
            "Ready condition reason should be Accepted"
        );

        let job_api = Api::<Job>::namespaced(client.clone(), "default");
        let jobs = job_api
            .list(&kube::api::ListParams::default())
            .await
            .unwrap();
        let synthetic_jobs: Vec<_> = jobs
            .items
            .iter()
            .filter(|job| {
                job.metadata.name.as_ref().is_some_and(|n| {
                    n.starts_with("kaniop-backup-discover-") && n.contains(repo_name)
                })
            })
            .collect();
        assert!(
            synthetic_jobs.is_empty(),
            "no synthetic probe Job should appear for a configuration-accepted repository, found: {:?}",
            synthetic_jobs
                .iter()
                .map(|j| j.metadata.name.as_ref())
                .collect::<Vec<_>>()
        );

        cleanup_test_resources(&client, repo_name, repo_name).await;
    }
);

e2e_test!(
    #[serial(backup)]
    backup_schedule_unique_per_kanidm,
    {
        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        let repo_name = "test-schedule-unique-repo";
        let kanidm_name = "test-schedule-unique-kanidm";

        cleanup_test_resources(&client, kanidm_name, repo_name).await;
        let schedule_api = Api::<KanidmBackupSchedule>::namespaced(client.clone(), "default");
        force_delete_and_wait(schedule_api.clone(), "test-schedule-1").await;
        force_delete_and_wait(schedule_api, "test-schedule-2").await;

        create_repository(
            &client,
            repo_name,
            "e2e-schedule-unique",
            MINIO_CREDS_SECRET,
        )
        .await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
        test_wait_for(repo_api, repo_name, is_repo_ready()).await;

        let mut spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        merge(&mut spec_json, &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone());
        spec_json["replicaGroups"] = json!([{
            "name": DEFAULT_REPLICA_GROUP_NAME,
            "replicas": 1,
            "primaryNode": true
        }]);
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

        let schedule_api = Api::<KanidmBackupSchedule>::namespaced(client.clone(), "default");

        let schedule1 = KanidmBackupSchedule::new(
            "test-schedule-1",
            KanidmBackupScheduleSpec {
                kanidm_ref: ScheduleKanidmRef {
                    name: kanidm_name.to_string(),
                },
                repository_ref: ScheduleRepositoryRef {
                    name: repo_name.to_string(),
                },
                schedule: "0 * * * *".to_string(),
                time_zone: "UTC".to_string(),
                suspend: true,
                concurrency_policy: "Forbid".to_string(),
                jitter_seconds: None,
                local_versions: 7,
                retention: None,
            },
        );
        schedule_api
            .create(&PostParams::default(), &schedule1)
            .await
            .unwrap();

        test_wait_for(
            schedule_api.clone(),
            "test-schedule-1",
            is_schedule_suspended(),
        )
        .await;

        tokio::time::sleep(Duration::from_secs(3)).await;

        let schedule2 = KanidmBackupSchedule::new(
            "test-schedule-2",
            KanidmBackupScheduleSpec {
                kanidm_ref: ScheduleKanidmRef {
                    name: kanidm_name.to_string(),
                },
                repository_ref: ScheduleRepositoryRef {
                    name: repo_name.to_string(),
                },
                schedule: "30 * * * *".to_string(),
                time_zone: "UTC".to_string(),
                suspend: true,
                concurrency_policy: "Forbid".to_string(),
                jitter_seconds: None,
                local_versions: 7,
                retention: None,
            },
        );
        let result = schedule_api
            .create(&PostParams::default(), &schedule2)
            .await;
        assert!(
            result.is_err(),
            "second schedule targeting same Kanidm should be rejected by webhook"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("only one KanidmBackupSchedule")
                || err_msg.contains("conflicting schedule"),
            "error should mention duplicate schedule target, got: {err_msg}"
        );

        cleanup_test_resources(&client, kanidm_name, repo_name).await;
        let schedule_api = Api::<KanidmBackupSchedule>::namespaced(client.clone(), "default");
        force_delete_and_wait(schedule_api, "test-schedule-1").await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_safety_backup_sets_ref_before_mutation,
    {
        let name = "test-safety-backup-ref";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        create_repository(
            &s.client,
            &repo_name,
            "e2e-safety-backup",
            MINIO_CREDS_SECRET,
        )
        .await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(s.client.clone(), "default");
        test_wait_for(repo_api, &repo_name, is_repo_ready()).await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();

        let backup_name = trigger_backup_on_primary(name, &s.client).await;

        let restore_name = format!("{name}-restore");
        let restore = KanidmRestore::new(
            &restore_name,
            KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                source: KanidmRestoreSource {
                    local: Some(KanidmRestoreLocalSource {
                        file_name: backup_name.clone(),
                    }),
                    backup_ref: None,
                },
                restore_image: image,
                safety_backup: Some(SafetyBackupConfig {
                    repository_ref: Some(SafetyBackupRepositoryRef {
                        name: repo_name.clone(),
                    }),
                    skip: false,
                }),
            },
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::SafetyBackup),
        )
        .await;

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            move |obj: Option<&KanidmRestore>| {
                obj.and_then(|restore| restore.status.as_ref())
                    .is_some_and(|status| {
                        status.safety_backup_ref.is_some() && !status.database_mutation_started
                    })
            },
        )
        .await;

        let restore_after_safety = restore_api.get(&restore_name).await.unwrap();
        let status = restore_after_safety.status.as_ref().unwrap();
        assert!(
            status.safety_backup_ref.is_some(),
            "safety_backup_ref should be set"
        );

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let final_status = final_restore.status.unwrap();
        assert_eq!(final_status.phase, KanidmRestorePhase::Completed);
        assert!(final_status.database_mutation_started);
        assert!(final_status.safety_backup_ref.is_some());

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_break_glass_without_annotations_rejected,
    {
        let name = "test-break-glass-no-annot";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();

        let backup_name = trigger_backup_on_primary(name, &s.client).await;

        let restore_name = format!("{name}-restore");
        let restore = KanidmRestore::new(
            &restore_name,
            KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                source: KanidmRestoreSource {
                    local: Some(KanidmRestoreLocalSource {
                        file_name: backup_name,
                    }),
                    backup_ref: None,
                },
                restore_image: image,
                safety_backup: Some(SafetyBackupConfig {
                    repository_ref: None,
                    skip: true,
                }),
            },
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            status
                .message
                .as_ref()
                .is_some_and(|m| m.contains("break-glass")),
            "failure message should mention break-glass"
        );

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_break_glass_with_annotations_succeeds,
    {
        let name = "test-break-glass-ok";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();

        let backup_name = trigger_backup_on_primary(name, &s.client).await;

        let restore_name = format!("{name}-restore");
        let mut restore = KanidmRestore::new(
            &restore_name,
            KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                source: KanidmRestoreSource {
                    local: Some(KanidmRestoreLocalSource {
                        file_name: backup_name,
                    }),
                    backup_ref: None,
                },
                restore_image: image,
                safety_backup: Some(SafetyBackupConfig {
                    repository_ref: None,
                    skip: true,
                }),
            },
        );
        restore.metadata.annotations = Some(
            [
                (
                    BREAK_GLASS_REASON_ANNOTATION.to_string(),
                    "emergency restore in e2e test".to_string(),
                ),
                (
                    BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
                    "e2e-test-runner".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            has_break_glass_condition(),
        )
        .await;

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Completed);
        assert!(
            status
                .conditions
                .iter()
                .any(|c| c.type_ == "BreakGlassOverride" && c.status == "True"),
            "BreakGlassOverride condition should be True"
        );

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_remote_round_trip,
    {
        let name = "test-remote-restore-rt";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        create_repository(&s.client, &repo_name, "e2e-remote-rt", MINIO_CREDS_SECRET).await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(s.client.clone(), "default");
        test_wait_for(repo_api, &repo_name, is_repo_ready()).await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();
        let domain = kanidm.spec.domain.clone();

        let backup_name = trigger_backup_on_primary(name, &s.client).await;

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let statefulset_api =
            Api::<k8s_openapi::api::apps::v1::StatefulSet>::namespaced(s.client.clone(), "default");
        let mut sts = statefulset_api.get(&sts_name).await.unwrap();
        sts.spec.as_mut().unwrap().replicas = Some(0);
        sts.metadata.managed_fields = None;
        statefulset_api
            .patch(
                &sts_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&sts),
            )
            .await
            .unwrap();

        poll_until("kanidm scaled to 0", || {
            let statefulset_api = statefulset_api.clone();
            let sts_name = sts_name.clone();
            async move {
                let sts = statefulset_api.get(&sts_name).await.ok()?;
                let ready = sts
                    .status
                    .as_ref()
                    .and_then(|s| s.ready_replicas)
                    .unwrap_or(0);
                if ready == 0 { Some(()) } else { None }
            }
        })
        .await;

        let backup_id = uuid::Uuid::new_v4().to_string();
        let namespace_uid = "default";
        let manifest_key = format!(
            "e2e-remote-rt/v1/tenants/{namespace_uid}/clusters/{kanidm_uid}/backups/{backup_id}/manifest.json"
        );

        let operation_doc = serde_json::json!({
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "OperationDocument",
            "operation": "upload",
            "payloadPath": format!("/data/{backup_name}"),
            "bucket": MINIO_BUCKET,
            "prefix": "e2e-remote-rt",
            "endpoint": MINIO_ENDPOINT,
            "region": MINIO_REGION,
            "forcePathStyle": true,
            "caBundlePath": "/run/kaniop-ca-bundle/ca-bundle.pem",
            "backupId": backup_id,
            "namespaceUid": namespace_uid,
            "kanidmUid": kanidm_uid,
            "kanidmName": name,
            "domain": domain,
            "kanidmVersion": "e2e",
            "consistency": "kanidm-offline",
            "reason": "e2e-test",
            "resultPath": "/run/kaniop-result/result.json",
            "maxConcurrentParts": 4,
            "maxRetries": 3,
        });

        let data_mover_image = std::env::var("DATA_MOVER_IMAGE").unwrap_or_else(|_| {
            format!(
                "ghcr.io/pando85/kaniop-data-mover:{}",
                option_env!("GIT_SHA").unwrap_or("aed6d7e")
            )
        });

        let op_cm_name = format!("{name}-upload-op");
        let cm_api =
            Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(s.client.clone(), "default");
        let op_cm = k8s_openapi::api::core::v1::ConfigMap {
            metadata: kube::api::ObjectMeta {
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

        let job_api = Api::<Job>::namespaced(s.client.clone(), "default");
        let upload_job_name = format!("{name}-upload");
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
                            "image": data_mover_image,
                            "command": ["/bin/kaniop-data-mover", "upload"],
                            "env": [
                                {"name": "AWS_ACCESS_KEY_ID", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_ACCESS_KEY_ID"}}},
                                {"name": "AWS_SECRET_ACCESS_KEY", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_SECRET_ACCESS_KEY"}}},
                                {"name": "RUST_LOG", "value": "info"},
                                {"name": "SSL_CERT_FILE", "value": "/run/kaniop-ca-bundle/ca-bundle.pem"}
                            ],
                            "volumeMounts": [
                                {"name": "data", "mountPath": "/data"},
                                {"name": "operation", "mountPath": "/run/kaniop"},
                                {"name": "ca-bundle", "mountPath": "/run/kaniop-ca-bundle"},
                                {"name": "result", "mountPath": "/run/kaniop-result"}
                            ]
                        }],
                        "volumes": [
                            {"name": "data", "persistentVolumeClaim": {"claimName": format!("kanidm-data-{sts_name}-0")}},
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

        poll_until("upload job completes", || {
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

        let backup_cr_name = format!("kb-{}", &backup_id[..8]);
        let backup_cr = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some(backup_cr_name.clone()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup_core::crd::KanidmBackupSpec {
                backup_id: backup_id.clone(),
                kanidm_ref: BackupKanidmRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: repo_name.clone(),
                },
                manifest_key: manifest_key.clone(),
            },
            status: None,
        };
        let backup_api = Api::<KanidmBackup>::namespaced(s.client.clone(), "default");
        backup_api
            .create(&PostParams::default(), &backup_cr)
            .await
            .unwrap();

        test_wait_for(
            backup_api.clone(),
            &backup_cr_name,
            is_backup_phase(KanidmBackupPhase::Ready),
        )
        .await;

        let mut sts = statefulset_api.get(&sts_name).await.unwrap();
        sts.spec.as_mut().unwrap().replicas = Some(1);
        sts.metadata.managed_fields = None;
        statefulset_api
            .patch(
                &sts_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&sts),
            )
            .await
            .unwrap();

        test_wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

        let restore_name = format!("{name}-restore");
        let restore = KanidmRestore::new(
            &restore_name,
            KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                source: KanidmRestoreSource {
                    local: None,
                    backup_ref: Some(KanidmRestoreBackupRefSource {
                        name: backup_cr_name.clone(),
                    }),
                },
                restore_image: image,
                safety_backup: Some(SafetyBackupConfig {
                    repository_ref: Some(SafetyBackupRepositoryRef {
                        name: repo_name.clone(),
                    }),
                    skip: false,
                }),
            },
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Completed);
        assert!(status.database_mutation_started);
        assert!(status.safety_backup_ref.is_some());

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_controller_restart_resilience,
    {
        let name = "test-ctrl-restart";

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &format!("{name}-unused")).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();

        let backup_name = trigger_backup_on_primary(name, &s.client).await;

        let restore_name = format!("{name}-restore");
        let mut restore = KanidmRestore::new(
            &restore_name,
            KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                source: KanidmRestoreSource {
                    local: Some(KanidmRestoreLocalSource {
                        file_name: backup_name,
                    }),
                    backup_ref: None,
                },
                restore_image: image,
                safety_backup: Some(SafetyBackupConfig {
                    repository_ref: None,
                    skip: true,
                }),
            },
        );
        restore.metadata.annotations = Some(
            [
                (
                    BREAK_GLASS_REASON_ANNOTATION.to_string(),
                    "controller restart resilience e2e test".to_string(),
                ),
                (
                    BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
                    "e2e-test-runner".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Quiescing),
        )
        .await;

        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "kaniop");
        let pods = pod_api
            .list(&kube::api::ListParams::default().labels("app.kubernetes.io/component=operator"))
            .await
            .unwrap();

        for pod in pods.items {
            if let Some(pod_name) = pod.metadata.name {
                eprintln!("Deleting operator pod: {pod_name}");
                pod_api.delete(&pod_name, &Default::default()).await.ok();
            }
        }

        let deploy_api = Api::<Deployment>::namespaced(s.client.clone(), "kaniop");
        test_wait_for(deploy_api.clone(), "kaniop", is_deployment_ready()).await;
        test_wait_for(deploy_api, "kaniop-webhook", is_deployment_ready()).await;

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Completed);

        wait_for_operator_and_webhook_ready(&s.client).await;
        cleanup_test_resources(&s.client, name, &format!("{name}-unused")).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_safety_backup_failure_resumes_service,
    {
        let name = "test-safety-fail-resume";
        let repo_name = format!("{name}-repo");
        let safety_repo_name = format!("{name}-safety-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
        force_delete_and_wait(repo_api.clone(), &safety_repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        create_repository(&s.client, &repo_name, "e2e-safety-fail", MINIO_CREDS_SECRET).await;
        test_wait_for(repo_api.clone(), &repo_name, is_repo_ready()).await;

        create_repository(
            &s.client,
            &safety_repo_name,
            "e2e-safety-fail-safety",
            MINIO_CREDS_INVALID_SECRET,
        )
        .await;
        test_wait_for(repo_api.clone(), &safety_repo_name, is_repo_ready()).await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();

        let backup_name = trigger_backup_on_primary(name, &s.client).await;

        let restore_name = format!("{name}-restore");
        let restore = KanidmRestore::new(
            &restore_name,
            KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                source: KanidmRestoreSource {
                    local: Some(KanidmRestoreLocalSource {
                        file_name: backup_name,
                    }),
                    backup_ref: None,
                },
                restore_image: image,
                safety_backup: Some(SafetyBackupConfig {
                    repository_ref: Some(SafetyBackupRepositoryRef {
                        name: safety_repo_name.clone(),
                    }),
                    skip: false,
                }),
            },
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        test_wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.as_ref().unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            !status.database_mutation_started,
            "database_mutation_started must be false after safety backup failure"
        );
        assert!(
            status
                .message
                .as_ref()
                .is_some_and(|m| m.contains("safety backup")),
            "failure message should mention safety backup, got: {:?}",
            status.message
        );

        test_wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

        cleanup_test_resources(&s.client, name, &repo_name).await;
        force_delete_and_wait(repo_api, &safety_repo_name).await;
    }
);

e2e_test!(
    #[serial(backup)]
    backup_spec_update_rejected_by_webhook,
    {
        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        let backup_name = "test-backup-immutable";

        let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), "default");
        backup_api
            .delete(backup_name, &Default::default())
            .await
            .ok();
        poll_until("backup deleted", || {
            let api = backup_api.clone();
            let name = backup_name.to_string();
            async move {
                if api.get(&name).await.is_err() {
                    Some(())
                } else {
                    None
                }
            }
        })
        .await;

        let backup = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some(backup_name.to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupSpec {
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                kanidm_ref: BackupKanidmRef {
                    name: "nonexistent".to_string(),
                    uid: "00000000-0000-0000-0000-000000000000".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "nonexistent".to_string(),
                },
                manifest_key: "e2e/test/manifest.json".to_string(),
            },
            status: None,
        };
        backup_api
            .create(&PostParams::default(), &backup)
            .await
            .unwrap();

        let mut updated_backup = backup_api.get(backup_name).await.unwrap();
        updated_backup.spec.manifest_key = "e2e/test/manifest-v2.json".to_string();
        updated_backup.metadata.managed_fields = None;
        let result = backup_api
            .patch(
                backup_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&updated_backup),
            )
            .await;
        assert!(result.is_err(), "spec update should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("immutable") || err_msg.contains("KanidmBackup spec"),
            "error should mention immutability, got: {err_msg}"
        );

        let mut backup_for_labels = backup_api.get(backup_name).await.unwrap();
        backup_for_labels.metadata.labels = Some(
            [("e2e-test".to_string(), "true".to_string())]
                .into_iter()
                .collect(),
        );
        backup_for_labels.metadata.managed_fields = None;
        backup_api
            .patch(
                backup_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&backup_for_labels),
            )
            .await
            .expect("metadata-only update should be allowed");

        let labeled = backup_api.get(backup_name).await.unwrap();
        assert_eq!(
            labeled
                .metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("e2e-test"))
                .map(|v| v.as_str()),
            Some("true"),
            "label should be applied"
        );

        backup_api
            .delete(backup_name, &Default::default())
            .await
            .ok();
    }
);

e2e_test!(
    #[serial(backup)]
    backup_repository_immutable_fields_after_use,
    {
        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        let repo_name = "test-repo-immutable-fields";

        cleanup_test_resources(&client, repo_name, repo_name).await;
        create_repository(&client, repo_name, "e2e-immutable", MINIO_CREDS_SECRET).await;

        let api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
        test_wait_for(api.clone(), repo_name, is_repo_ready()).await;

        let mut repo = api.get(repo_name).await.unwrap();
        repo.spec.s3.bucket = "different-bucket".to_string();
        repo.metadata.managed_fields = None;
        let result = api
            .patch(
                repo_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&repo),
            )
            .await;
        assert!(result.is_err(), "bucket change should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("immutable") || err_msg.contains("bucket"),
            "error should mention immutability or bucket, got: {err_msg}"
        );

        let mut repo = api.get(repo_name).await.unwrap();
        repo.spec.s3.prefix = "different-prefix".to_string();
        repo.metadata.managed_fields = None;
        let result = api
            .patch(
                repo_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&repo),
            )
            .await;
        assert!(result.is_err(), "prefix change should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("immutable") || err_msg.contains("prefix"),
            "error should mention immutability or prefix, got: {err_msg}"
        );

        let mut repo = api.get(repo_name).await.unwrap();
        repo.spec.s3.endpoint = Some("https://other.endpoint.example.com".to_string());
        repo.metadata.managed_fields = None;
        let result = api
            .patch(
                repo_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&repo),
            )
            .await;
        assert!(result.is_err(), "endpoint change should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("immutable") || err_msg.contains("endpoint"),
            "error should mention immutability or endpoint, got: {err_msg}"
        );

        cleanup_test_resources(&client, repo_name, repo_name).await;
    }
);

e2e_test!(
    #[serial(backup)]
    backup_retention_deletes_old_cr_and_s3_objects,
    {
        let name = "test-retention-delete";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        create_repository(&s.client, &repo_name, "e2e-retention", MINIO_CREDS_SECRET).await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(s.client.clone(), "default");
        test_wait_for(repo_api, &repo_name, is_repo_ready()).await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let namespace_uid = "default";

        let mut backup_ids = Vec::new();
        for _ in 0..3 {
            let backup_name = trigger_backup_on_primary(name, &s.client).await;
            let backup_id = uuid::Uuid::new_v4().to_string();
            let manifest_key = format!(
                "e2e-retention/v1/tenants/{namespace_uid}/clusters/{kanidm_uid}/backups/{backup_id}/manifest.json"
            );

            let operation_doc = serde_json::json!({
                "apiVersion": "backup.kaniop.rs/v1alpha1",
                "kind": "OperationDocument",
                "operation": "upload",
                "payloadPath": format!("/data/{backup_name}"),
                "bucket": MINIO_BUCKET,
                "prefix": "e2e-retention",
                "endpoint": MINIO_ENDPOINT,
                "region": MINIO_REGION,
                "forcePathStyle": true,
                "caBundlePath": "/run/kaniop-ca-bundle/ca-bundle.pem",
                "backupId": backup_id,
                "namespaceUid": namespace_uid,
                "kanidmUid": kanidm_uid,
                "kanidmName": name,
                "domain": kanidm.spec.domain.clone(),
                "kanidmVersion": "e2e",
                "consistency": "kanidm-offline",
                "reason": "e2e-test",
                "resultPath": "/run/kaniop-result/result.json",
                "maxConcurrentParts": 4,
                "maxRetries": 3,
            });

            let data_mover_image = std::env::var("DATA_MOVER_IMAGE").unwrap_or_else(|_| {
                format!(
                    "ghcr.io/pando85/kaniop-data-mover:{}",
                    option_env!("GIT_SHA").unwrap_or("aed6d7e")
                )
            });

            let op_cm_name = format!("{name}-upload-{backup_id}-op");
            let cm_api = Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(
                s.client.clone(),
                "default",
            );
            let op_cm = k8s_openapi::api::core::v1::ConfigMap {
                metadata: kube::api::ObjectMeta {
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

            let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
            let job_api = Api::<Job>::namespaced(s.client.clone(), "default");
            let upload_job_name = format!("{name}-upload-{backup_id}");
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
                                "image": data_mover_image,
                                "command": ["/bin/kaniop-data-mover", "upload"],
                                "env": [
                                    {"name": "AWS_ACCESS_KEY_ID", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_ACCESS_KEY_ID"}}},
                                    {"name": "AWS_SECRET_ACCESS_KEY", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_SECRET_ACCESS_KEY"}}},
                                    {"name": "RUST_LOG", "value": "info"},
                                    {"name": "SSL_CERT_FILE", "value": "/run/kaniop-ca-bundle/ca-bundle.pem"}
                                ],
                                "volumeMounts": [
                                    {"name": "data", "mountPath": "/data"},
                                    {"name": "operation", "mountPath": "/run/kaniop"},
                                    {"name": "ca-bundle", "mountPath": "/run/kaniop-ca-bundle"},
                                    {"name": "result", "mountPath": "/run/kaniop-result"}
                                ]
                            }],
                            "volumes": [
                                {"name": "data", "persistentVolumeClaim": {"claimName": format!("kanidm-data-{sts_name}-0")}},
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

            poll_until("upload job completes", || {
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

            let backup_cr_name = format!("kb-{}", &backup_id[..8]);
            let backup_cr = KanidmBackup {
                metadata: kube::api::ObjectMeta {
                    name: Some(backup_cr_name.clone()),
                    namespace: Some("default".to_string()),
                    ..Default::default()
                },
                spec: KanidmBackupSpec {
                    backup_id: backup_id.clone(),
                    kanidm_ref: BackupKanidmRef {
                        name: name.to_string(),
                        uid: kanidm_uid.to_string(),
                    },
                    repository_ref: BackupRepositoryRef {
                        name: repo_name.clone(),
                    },
                    manifest_key: manifest_key.clone(),
                },
                status: None,
            };
            let backup_api = Api::<KanidmBackup>::namespaced(s.client.clone(), "default");
            backup_api
                .create(&PostParams::default(), &backup_cr)
                .await
                .unwrap();

            test_wait_for(
                backup_api.clone(),
                &backup_cr_name,
                is_backup_phase(KanidmBackupPhase::Ready),
            )
            .await;

            backup_ids.push(backup_id);
        }

        assert!(
            backup_ids.len() >= 3,
            "should have created at least 3 backups"
        );

        let backup_api = Api::<KanidmBackup>::namespaced(s.client.clone(), "default");
        let old_backup_id = &backup_ids[0];
        let old_backup_cr_name = format!("kb-{}", &old_backup_id[..8]);

        backup_api
            .delete(&old_backup_cr_name, &Default::default())
            .await
            .ok();

        poll_until("old backup CR deleted", || {
            let api = backup_api.clone();
            let name = old_backup_cr_name.clone();
            async move {
                if api.get(&name).await.is_err() {
                    Some(())
                } else {
                    None
                }
            }
        })
        .await;

        let old_prefix = format!(
            "e2e-retention/v1/tenants/{namespace_uid}/clusters/{kanidm_uid}/backups/{old_backup_id}/"
        );

        let discover_op = serde_json::json!({
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "OperationDocument",
            "operation": "discover",
            "bucket": MINIO_BUCKET,
            "prefix": "e2e-retention",
            "endpoint": MINIO_ENDPOINT,
            "region": MINIO_REGION,
            "forcePathStyle": true,
            "caBundlePath": "/run/kaniop-ca-bundle/ca-bundle.pem",
            "namespaceUid": namespace_uid,
            "kanidmUid": kanidm_uid,
            "resultPath": "/run/kaniop-result/result.json",
            "maxResults": 1000,
            "maxRetries": 3,
        });

        let data_mover_image = std::env::var("DATA_MOVER_IMAGE").unwrap_or_else(|_| {
            format!(
                "ghcr.io/pando85/kaniop-data-mover:{}",
                option_env!("GIT_SHA").unwrap_or("aed6d7e")
            )
        });

        let discover_cm_name = format!("{name}-discover-check");
        let cm_api =
            Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(s.client.clone(), "default");
        let discover_cm = k8s_openapi::api::core::v1::ConfigMap {
            metadata: kube::api::ObjectMeta {
                name: Some(discover_cm_name.clone()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            data: Some(
                [(
                    "operation.json".to_string(),
                    serde_json::to_string(&discover_op).unwrap(),
                )]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        };
        cm_api
            .create(&PostParams::default(), &discover_cm)
            .await
            .unwrap();

        let job_api = Api::<Job>::namespaced(s.client.clone(), "default");
        let discover_job_name = format!("{name}-discover-check");
        let discover_job: Job = serde_json::from_value(json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {
                "name": discover_job_name,
                "namespace": "default"
            },
            "spec": {
                "backoffLimit": 1,
                "template": {
                    "spec": {
                        "restartPolicy": "Never",
                        "containers": [{
                            "name": "data-mover",
                            "image": data_mover_image,
                            "command": ["/bin/kaniop-data-mover", "discover"],
                            "env": [
                                {"name": "AWS_ACCESS_KEY_ID", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_ACCESS_KEY_ID"}}},
                                {"name": "AWS_SECRET_ACCESS_KEY", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_SECRET_ACCESS_KEY"}}},
                                {"name": "RUST_LOG", "value": "info"},
                                {"name": "SSL_CERT_FILE", "value": "/run/kaniop-ca-bundle/ca-bundle.pem"}
                            ],
                            "volumeMounts": [
                                {"name": "operation", "mountPath": "/run/kaniop"},
                                {"name": "ca-bundle", "mountPath": "/run/kaniop-ca-bundle"},
                                {"name": "result", "mountPath": "/run/kaniop-result"}
                            ]
                        }],
                        "volumes": [
                            {"name": "operation", "configMap": {"name": discover_cm_name}},
                            {"name": "ca-bundle", "configMap": {"name": MINIO_CA_CM}},
                            {"name": "result", "emptyDir": {}}
                        ]
                    }
                }
            }
        }))
        .unwrap();

        job_api
            .create(&PostParams::default(), &discover_job)
            .await
            .unwrap();

        poll_until("discover job completes", || {
            let job_api = job_api.clone();
            let job_name = discover_job_name.clone();
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

        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
        let pods = pod_api
            .list(
                &kube::api::ListParams::default().labels(&format!("job-name={discover_job_name}")),
            )
            .await
            .unwrap();
        let succeeded_pod = pods
            .items
            .iter()
            .find(|p| {
                p.status.as_ref().and_then(|s| s.phase.as_ref()) == Some(&"Succeeded".to_string())
            })
            .expect("discover job should have a succeeded pod");

        let container_status = succeeded_pod
            .status
            .as_ref()
            .unwrap()
            .container_statuses
            .as_ref()
            .unwrap()
            .iter()
            .find(|cs| cs.name == "data-mover")
            .unwrap();
        let termination_msg = container_status
            .state
            .as_ref()
            .and_then(|s| s.terminated.as_ref())
            .and_then(|t| t.message.as_ref())
            .expect("termination message should exist");

        let result: serde_json::Value = serde_json::from_str(termination_msg).unwrap();
        let manifest_keys = result["discovery"]["manifestKeys"]
            .as_array()
            .expect("discovery should have manifestKeys");

        for key in manifest_keys {
            let key_str = key.as_str().unwrap();
            assert!(
                !key_str.contains(old_prefix.as_str()),
                "old backup prefix should be deleted, found: {key_str}"
            );
        }

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);

e2e_test!(
    #[serial(backup)]
    backup_finalizer_holds_deletion_until_s3_cleanup,
    {
        let name = "test-finalizer-hold";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        create_repository(&s.client, &repo_name, "e2e-finalizer", MINIO_CREDS_SECRET).await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(s.client.clone(), "default");
        test_wait_for(repo_api, &repo_name, is_repo_ready()).await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();

        let backup_name = trigger_backup_on_primary(name, &s.client).await;
        let backup_id = uuid::Uuid::new_v4().to_string();
        let namespace_uid = "default";
        let manifest_key = format!(
            "e2e-finalizer/v1/tenants/{namespace_uid}/clusters/{kanidm_uid}/backups/{backup_id}/manifest.json"
        );

        let operation_doc = serde_json::json!({
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "OperationDocument",
            "operation": "upload",
            "payloadPath": format!("/data/{backup_name}"),
            "bucket": MINIO_BUCKET,
            "prefix": "e2e-finalizer",
            "endpoint": MINIO_ENDPOINT,
            "region": MINIO_REGION,
            "forcePathStyle": true,
            "caBundlePath": "/run/kaniop-ca-bundle/ca-bundle.pem",
            "backupId": backup_id,
            "namespaceUid": namespace_uid,
            "kanidmUid": kanidm_uid,
            "kanidmName": name,
            "domain": kanidm.spec.domain.clone(),
            "kanidmVersion": "e2e",
            "consistency": "kanidm-offline",
            "reason": "e2e-test",
            "resultPath": "/run/kaniop-result/result.json",
            "maxConcurrentParts": 4,
            "maxRetries": 3,
        });

        let data_mover_image = std::env::var("DATA_MOVER_IMAGE").unwrap_or_else(|_| {
            format!(
                "ghcr.io/pando85/kaniop-data-mover:{}",
                option_env!("GIT_SHA").unwrap_or("aed6d7e")
            )
        });

        let op_cm_name = format!("{name}-upload-op");
        let cm_api =
            Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(s.client.clone(), "default");
        let op_cm = k8s_openapi::api::core::v1::ConfigMap {
            metadata: kube::api::ObjectMeta {
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

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let job_api = Api::<Job>::namespaced(s.client.clone(), "default");
        let upload_job_name = format!("{name}-upload");
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
                            "image": data_mover_image,
                            "command": ["/bin/kaniop-data-mover", "upload"],
                            "env": [
                                {"name": "AWS_ACCESS_KEY_ID", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_ACCESS_KEY_ID"}}},
                                {"name": "AWS_SECRET_ACCESS_KEY", "valueFrom": {"secretKeyRef": {"name": MINIO_CREDS_SECRET, "key": "AWS_SECRET_ACCESS_KEY"}}},
                                {"name": "RUST_LOG", "value": "info"},
                                {"name": "SSL_CERT_FILE", "value": "/run/kaniop-ca-bundle/ca-bundle.pem"}
                            ],
                            "volumeMounts": [
                                {"name": "data", "mountPath": "/data"},
                                {"name": "operation", "mountPath": "/run/kaniop"},
                                {"name": "ca-bundle", "mountPath": "/run/kaniop-ca-bundle"},
                                {"name": "result", "mountPath": "/run/kaniop-result"}
                            ]
                        }],
                        "volumes": [
                            {"name": "data", "persistentVolumeClaim": {"claimName": format!("kanidm-data-{sts_name}-0")}},
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

        poll_until("upload job completes", || {
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

        let backup_cr_name = format!("kb-{}", &backup_id[..8]);
        let backup_cr = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some(backup_cr_name.clone()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupSpec {
                backup_id: backup_id.clone(),
                kanidm_ref: BackupKanidmRef {
                    name: name.to_string(),
                    uid: kanidm_uid.to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: repo_name.clone(),
                },
                manifest_key: manifest_key.clone(),
            },
            status: None,
        };
        let backup_api = Api::<KanidmBackup>::namespaced(s.client.clone(), "default");
        backup_api
            .create(&PostParams::default(), &backup_cr)
            .await
            .unwrap();

        test_wait_for(
            backup_api.clone(),
            &backup_cr_name,
            is_backup_phase(KanidmBackupPhase::Ready),
        )
        .await;

        let backup_with_finalizer = backup_api.get(&backup_cr_name).await.unwrap();
        assert!(
            backup_with_finalizer
                .metadata
                .finalizers
                .as_ref()
                .is_some_and(|f| f.iter().any(|s| s == "kanidmbackups.kaniop.rs/finalizer")),
            "backup should have finalizer attached"
        );

        backup_api
            .delete(&backup_cr_name, &Default::default())
            .await
            .ok();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let deleting_backup = backup_api.get(&backup_cr_name).await.unwrap();
        assert!(
            deleting_backup.metadata.deletion_timestamp.is_some(),
            "backup should have deletionTimestamp set"
        );
        assert!(
            deleting_backup
                .metadata
                .finalizers
                .as_ref()
                .is_some_and(|f| f.iter().any(|s| s == "kanidmbackups.kaniop.rs/finalizer")),
            "finalizer should still be present during deletion"
        );

        poll_until("backup CR fully deleted", || {
            let api = backup_api.clone();
            let name = backup_cr_name.clone();
            async move {
                if api.get(&name).await.is_err() {
                    Some(())
                } else {
                    None
                }
            }
        })
        .await;

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);
