use serial_test::serial;

use super::{
    DEFAULT_REPLICA_GROUP_NAME, KANIDM_DEFAULT_SPEC_JSON, MINIO_BUCKET, MINIO_CA_CM,
    MINIO_CREDS_SECRET, MINIO_ENDPOINT, MINIO_REGION, STORAGE_VOLUME_CLAIM_TEMPLATE_JSON,
    cleanup_test_resources, create_backup_cr_and_wait, create_repository,
    has_statefulset_ready_replicas, is_kanidm, is_kanidm_false, is_statefulset_ready, minio_auth,
    minio_s3_config, setup, upload_backup_to_s3, wait_for,
    wait_for_replication_success_with_timeout,
};
use crate::test::{init_crypto_provider, poll_until};

use kaniop_backup_core::crd::{
    BackupKanidmRef, BackupRepositoryRef, EncryptionMode, KanidmBackup, KanidmBackupPhase,
    KanidmBackupRepository, KanidmBackupRepositorySpec, KanidmBackupSpec, RepositoryEncryption,
    SecretRef,
};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::{
    BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION, KanidmRestore,
    KanidmRestoreBackupRefSource, KanidmRestoreLocalSource, KanidmRestorePhase,
    KanidmRestoreSource, KanidmRestoreSpec, KanidmRestoreTargetRef, SafetyBackupConfig,
    SafetyBackupRepositoryRef,
};

use std::time::Duration;

use json_patch::merge;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::client::Client;
use kube::runtime::wait::conditions;
use serde_json::json;

const RESTORE_ANNOTATION: &str = "kanidm.kaniop.rs/restore-in-progress";

async fn cleanup_restore_test_resources(name: &str) {
    let client = Client::try_default().await.unwrap();
    let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), "default");
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let job_api = Api::<Job>::namespaced(client.clone(), "default");
    let secret_api =
        Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), "default");
    let pvc_api = Api::<k8s_openapi::api::core::v1::PersistentVolumeClaim>::namespaced(
        client.clone(),
        "default",
    );

    // Force delete restore CR and wait for it to be gone
    let restore_name = format!("{name}-restore");

    // First, try to remove the finalizer to allow deletion
    let _ = restore_api
        .patch(
            &restore_name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(json!({"metadata": {"finalizers": null}})),
        )
        .await;

    // Then delete the restore
    restore_api
        .delete(&restore_name, &Default::default())
        .await
        .ok();

    // Wait for it to be gone
    poll_until(&format!("{restore_name} deleted"), || {
        let api = restore_api.clone();
        let name = restore_name.clone();
        async move {
            if api.get(&name).await.is_err() {
                Some(())
            } else {
                None
            }
        }
    })
    .await;

    // Delete Kanidm CR and wait for it to be gone
    let _ = kanidm_api.delete(name, &Default::default()).await;
    poll_until(&format!("{name} deleted"), || {
        let api = kanidm_api.clone();
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

    // Delete jobs with the name prefix
    if let Ok(jobs) = job_api.list(&Default::default()).await {
        for job in jobs.items {
            if let Some(job_name) = &job.metadata.name {
                if job_name.starts_with(name) {
                    let _ = job_api.delete(job_name, &Default::default()).await;
                }
            }
        }
    }

    // Delete secrets with the name prefix
    if let Ok(secrets) = secret_api.list(&Default::default()).await {
        for secret in secrets.items {
            if let Some(secret_name) = &secret.metadata.name {
                if secret_name.starts_with(name) {
                    let _ = secret_api.delete(secret_name, &Default::default()).await;
                }
            }
        }
    }

    // Delete PVCs with the name prefix
    if let Ok(pvcs) = pvc_api.list(&Default::default()).await {
        for pvc in pvcs.items {
            if let Some(pvc_name) = &pvc.metadata.name {
                if pvc_name.starts_with(name) {
                    let _ = pvc_api.delete(pvc_name, &Default::default()).await;
                }
            }
        }
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

fn has_database_mutation_started() -> impl kube::runtime::wait::Condition<KanidmRestore> {
    move |obj: Option<&KanidmRestore>| {
        obj.and_then(|restore| restore.status.as_ref())
            .is_some_and(|status| status.database_mutation_started)
    }
}

fn has_restore_annotation(uid: &str) -> impl kube::runtime::wait::Condition<Kanidm> + '_ {
    move |obj: Option<&Kanidm>| {
        obj.is_some_and(|kanidm| {
            kanidm
                .annotations()
                .get(RESTORE_ANNOTATION)
                .is_some_and(|v| v == uid)
        })
    }
}

fn create_restore(
    name: &str,
    target_name: &str,
    target_uid: &str,
    file_name: &str,
    restore_image: &str,
) -> KanidmRestore {
    let mut restore = KanidmRestore::new(
        name,
        KanidmRestoreSpec {
            target_ref: KanidmRestoreTargetRef {
                name: target_name.to_string(),
                uid: target_uid.to_string(),
            },
            source: KanidmRestoreSource {
                local: Some(KanidmRestoreLocalSource {
                    file_name: file_name.to_string(),
                }),
                backup_ref: None,
            },
            restore_image: restore_image.to_string(),
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
                "local restore e2e test".to_string(),
            ),
            (
                BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
                "e2e-test-runner".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    restore
}

async fn setup_kanidm_with_backup(name: &str) -> (super::SetupKanidm, String, String) {
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
    (s, kanidm_uid, image)
}

async fn trigger_backup_on_primary(s: &super::SetupKanidm, kanidm_name: &str) -> String {
    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
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

e2e_test!(
    #[serial(restore)]
    restore_stale_uid_rejected,
    {
        let name = "test-restore-stale-uid";
        let (s, _actual_uid, image) = setup_kanidm_with_backup(name).await;

        let stale_uid = "00000000-0000-0000-0000-000000000000";
        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, stale_uid, "backup.json", &image);

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            status.message.unwrap().contains("UID mismatch"),
            "failure message should mention UID mismatch"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_wrong_image_rejected,
    {
        let name = "test-restore-wrong-image";
        let (s, kanidm_uid, _image) = setup_kanidm_with_backup(name).await;

        let wrong_image = "kanidm/server:1.0.0";
        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, "backup.json", wrong_image);

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            status.message.unwrap().contains("restoreImage"),
            "failure message should mention restoreImage validation"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_latest_image_rejected,
    {
        let name = "test-restore-latest-image";
        let (s, kanidm_uid, _image) = setup_kanidm_with_backup(name).await;

        let latest_image = "kanidm/server:latest";
        let restore_name = format!("{name}-restore");
        let restore = create_restore(
            &restore_name,
            name,
            &kanidm_uid,
            "backup.json",
            latest_image,
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
    }
);

e2e_test!(
    #[serial(restore)]
    restore_missing_backup_file_fails,
    {
        let name = "test-restore-missing-backup";
        cleanup_restore_test_resources(name).await;
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(
            &restore_name,
            name,
            &kanidm_uid,
            "nonexistent-backup.json",
            &image,
        );

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            status.message.unwrap().contains("backup file check failed"),
            "failure should mention the local backup file check failure"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_corrupt_backup_file_fails_closed,
    {
        let name = "test-restore-corrupt-backup";
        cleanup_restore_test_resources(name).await;
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let backup_path = format!("/data/{backup_name}");
        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let pvc_name = format!("kanidm-data-{sts_name}-0");
        let corrupt_job_name = format!("{name}-corrupt-backup");

        let job_api = Api::<Job>::namespaced(s.client.clone(), "default");
        let corrupt_job: Job = serde_json::from_value(json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {
                "name": corrupt_job_name,
                "namespace": "default"
            },
            "spec": {
                "backoffLimit": 1,
                "template": {
                    "spec": {
                        "restartPolicy": "Never",
                        "containers": [{
                            "name": "corrupter",
                            "image": "busybox:latest",
                            "command": ["sh", "-c", format!("printf 'GARBAGE' > {backup_path}")],
                            "volumeMounts": [{
                                "name": "data",
                                "mountPath": "/data"
                            }]
                        }],
                        "volumes": [{
                            "name": "data",
                            "persistentVolumeClaim": {"claimName": pvc_name}
                        }]
                    }
                }
            }
        }))
        .unwrap();

        job_api
            .create(&PostParams::default(), &corrupt_job)
            .await
            .expect("corrupt job create should succeed");

        poll_until("corrupt job completes", || {
            let job_api = job_api.clone();
            let job_name = corrupt_job_name.clone();
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
            .delete(&corrupt_job_name, &Default::default())
            .await
            .ok();

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            !status.database_mutation_started,
            "database_mutation_started must be false after corrupt backup source check failure"
        );
        assert!(
            status
                .message
                .as_ref()
                .is_some_and(|m| m.contains("backup file check failed")),
            "failure message should mention backup file check failure, got: {:?}",
            status.message
        );

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_path_traversal_rejected,
    {
        let name = "test-restore-path-traversal";
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let malicious_path = "../etc/passwd";
        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, malicious_path, &image);

        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            status.message.unwrap().contains("safe basename"),
            "failure should mention safe basename validation"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_concurrent_rejected,
    {
        let name = "test-restore-concurrent";
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore1_name = format!("{name}-restore-1");
        let restore1 = create_restore(&restore1_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore1)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore1_name,
            is_restore_phase(KanidmRestorePhase::Quiescing),
        )
        .await;

        let restore2_name = format!("{name}-restore-2");
        let restore2 = create_restore(&restore2_name, name, &kanidm_uid, &backup_name, &image);
        restore_api
            .create(&PostParams::default(), &restore2)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore2_name,
            is_restore_phase(KanidmRestorePhase::Failed),
        )
        .await;

        let final_restore2 = restore_api.get(&restore2_name).await.unwrap();
        let status = final_restore2.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Failed);
        assert!(
            status.message.unwrap().contains("active restore"),
            "failure should mention concurrent restore"
        );

        restore_api
            .delete(&restore1_name, &Default::default())
            .await
            .unwrap();
    }
);

e2e_test!(
    #[serial(replication)]
    restore_local_success_with_replicas,
    {
        let name = "test-restore-local-replicas";
        cleanup_restore_test_resources(name).await;
        let mut patch = json!({
            "replicaGroups": [
                {"name": "default", "replicas": 2, "primaryNode": true},
            ],
        });
        merge(&mut patch, &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone());
        let s = setup(name, Some(patch)).await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
        let pod_names = (0..2)
            .map(|i| format!("{sts_name}-{i}"))
            .collect::<Vec<_>>();
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Initialized")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
        wait_for(s.statefulset_api.clone(), &sts_name, is_statefulset_ready).await;
        wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        let restore_uid = restore_api.get(&restore_name).await.unwrap().uid().unwrap();
        wait_for(
            s.kanidm_api.clone(),
            name,
            has_restore_annotation(&restore_uid),
        )
        .await;

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Quiescing),
        )
        .await;

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::RestoringPrimary),
        )
        .await;

        wait_for(
            restore_api.clone(),
            &restore_name,
            has_database_mutation_started(),
        )
        .await;

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();
        assert_eq!(status.phase, KanidmRestorePhase::Completed);
        assert!(status.database_mutation_started);
        assert!(status.restore_job_name.is_some());
        assert!(status.verify_job_name.is_some());
        assert!(status.replicas_cleared);

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        let kanidm_after = s.kanidm_api.get(name).await.unwrap();
        assert!(
            kanidm_after.annotations().get(RESTORE_ANNOTATION).is_none(),
            "restore annotation should be cleared after completion"
        );

        wait_for(
            s.statefulset_api.clone(),
            &sts_name,
            has_statefulset_ready_replicas(2),
        )
        .await;

        let sts_after = s.statefulset_api.get(&sts_name).await.unwrap();
        assert_eq!(
            sts_after.spec.as_ref().unwrap().replicas.unwrap(),
            2,
            "StatefulSet should have 2 replicas after restore"
        );

        let pod_names_after = (0..2)
            .map(|i| format!("{sts_name}-{i}"))
            .collect::<Vec<_>>();
        wait_for_replication_success_with_timeout(&pod_api, &pod_names_after).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_delete_before_mutation_resumes_service,
    {
        let name = "test-restore-delete-before-mutation";
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Quiescing),
        )
        .await;

        let restore_before_delete = restore_api.get(&restore_name).await.unwrap();
        assert!(
            !restore_before_delete
                .status
                .as_ref()
                .is_some_and(|s| s.database_mutation_started),
            "mutation should not have started yet"
        );

        restore_api
            .delete(&restore_name, &Default::default())
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            conditions::is_deleted(&restore_before_delete.uid().unwrap()),
        )
        .await;

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

        let kanidm_after = s.kanidm_api.get(name).await.unwrap();
        assert!(
            kanidm_after.annotations().get(RESTORE_ANNOTATION).is_none(),
            "restore annotation should be cleared after deletion before mutation"
        );

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let sts = s.statefulset_api.get(&sts_name).await.unwrap();
        assert_eq!(
            sts.spec.unwrap().replicas.unwrap(),
            1,
            "replicas should be restored after deletion before mutation"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_delete_after_mutation_refused,
    {
        let name = "test-restore-delete-after-mutation";
        cleanup_restore_test_resources(name).await;
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            has_database_mutation_started(),
        )
        .await;

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::RestoringPrimary),
        )
        .await;

        let delete_result = restore_api
            .delete(&restore_name, &Default::default())
            .await
            .unwrap();

        assert!(
            delete_result.left().is_some(),
            "delete should return the object with finalizer"
        );

        let restore_after_delete = restore_api.get(&restore_name).await.unwrap();
        assert!(
            restore_after_delete.metadata.deletion_timestamp.is_some(),
            "restore should have deletion timestamp"
        );
        assert!(
            restore_after_delete
                .status
                .as_ref()
                .is_some_and(|s| s.database_mutation_started),
            "database_mutation_started should still be true"
        );

        // Restore continues making progress during deletion and eventually completes
        // and is deleted. We poll until it's gone or reaches Completed.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        loop {
            match restore_api.get(&restore_name).await {
                Ok(r) => {
                    if r.status
                        .as_ref()
                        .is_some_and(|s| s.phase == KanidmRestorePhase::Completed)
                    {
                        break;
                    }
                }
                Err(_) => break, // Resource deleted, test passes
            }
            if tokio::time::Instant::now() > deadline {
                panic!("Timeout waiting for restore to complete or be deleted");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
);

e2e_test!(
    #[serial(restore)]
    restore_status_conditions_set_correctly,
    {
        let name = "test-restore-conditions";
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();

        let ready_condition = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready condition should exist");
        assert_eq!(ready_condition.status, "True");

        let progressing_condition = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Progressing")
            .expect("Progressing condition should exist");
        assert_eq!(progressing_condition.status, "False");

        let failed_condition = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Failed")
            .expect("Failed condition should exist");
        assert_eq!(failed_condition.status, "False");
    }
);

e2e_test!(
    #[serial(restore)]
    restore_jobs_are_owned_by_restore,
    {
        let name = "test-restore-jobs-owned";
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let restore_uid = final_restore.uid().unwrap();
        let status = final_restore.status.unwrap();

        let job_api = Api::<Job>::namespaced(s.client.clone(), "default");

        let restore_job_name = status.restore_job_name.unwrap();
        let restore_job = job_api.get(&restore_job_name).await.unwrap();
        let owner_refs = restore_job.metadata.owner_references.unwrap();
        assert!(
            owner_refs.iter().any(|o| o.uid == restore_uid),
            "restore job should be owned by the restore resource"
        );

        let verify_job_name = status.verify_job_name.unwrap();
        let verify_job = job_api.get(&verify_job_name).await.unwrap();
        let owner_refs = verify_job.metadata.owner_references.unwrap();
        assert!(
            owner_refs.iter().any(|o| o.uid == restore_uid),
            "verify job should be owned by the restore resource"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_requires_pvc_storage,
    {
        let name = "test-restore-requires-pvc";
        let kanidm_name = format!("{name}-kanidm");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();

        let mut spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        merge(
            &mut spec_json,
            &json!({
                "storage": {
                    "emptyDir": {}
                },
                "replicaGroups": [
                    {"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 2, "primaryNode": true}
                ]
            }),
        );

        let kanidm = Kanidm::new(&kanidm_name, serde_json::from_value(spec_json).unwrap());
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");

        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;
        assert!(
            result.is_err(),
            "ephemeral storage with replication should be rejected"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_requires_exactly_one_primary,
    {
        let name = "test-restore-one-primary";
        let kanidm_name = format!("{name}-kanidm");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();

        let mut spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        merge(
            &mut spec_json,
            &json!({
                "replicaGroups": [
                    {"name": "primary", "replicas": 1, "primaryNode": true},
                    {"name": "secondary", "replicas": 1, "primaryNode": true}
                ],
                "backup": {
                    "schedule": "0 0 * * *"
                }
            }),
        );
        merge(&mut spec_json, &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone());

        let kanidm = Kanidm::new(&kanidm_name, serde_json::from_value(spec_json).unwrap());
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");

        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;
        assert!(
            result.is_err(),
            "two primary nodes should be rejected at admission"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_observed_generation_updated,
    {
        let name = "test-restore-observed-gen";
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        let created = restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        let expected_generation = created.metadata.generation.unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();

        assert_eq!(
            status.observed_generation,
            Some(expected_generation),
            "observed_generation should match the restore generation"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_observed_target_uid_set,
    {
        let name = "test-restore-observed-uid";
        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let restore_name = format!("{name}-restore");
        let restore = create_restore(&restore_name, name, &kanidm_uid, &backup_name, &image);
        let restore_api = Api::<KanidmRestore>::namespaced(s.client.clone(), "default");
        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let status = final_restore.status.unwrap();

        assert_eq!(
            status.observed_target_uid,
            Some(kanidm_uid),
            "observed_target_uid should match the target Kanidm UID"
        );
    }
);

e2e_test!(
    #[serial(restore)]
    restore_wrong_kek_fails_closed,
    {
        let name = "test-wrong-kek-fail";
        let repo_name = format!("{name}-repo");
        let safety_repo_name = format!("{name}-safety-repo");
        let kek_secret_name = format!("{name}-kek");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();

        let restore_api = Api::<KanidmRestore>::namespaced(client.clone(), "default");
        let _ = restore_api
            .patch(
                &format!("{name}-restore"),
                &kube::api::PatchParams::default(),
                &kube::api::Patch::Merge(json!({"metadata": {"finalizers": null}})),
            )
            .await;
        restore_api
            .delete(&format!("{name}-restore"), &Default::default())
            .await
            .ok();

        let backup_api = Api::<KanidmBackup>::namespaced(client.clone(), "default");
        if let Ok(list) = backup_api.list(&Default::default()).await {
            for backup in list.items {
                if backup.spec.kanidm_ref.name == name {
                    let _ = backup_api
                        .patch(
                            &backup.name_any(),
                            &kube::api::PatchParams::default(),
                            &kube::api::Patch::Merge(json!({"metadata": {"finalizers": null}})),
                        )
                        .await;
                    backup_api
                        .delete(&backup.name_any(), &Default::default())
                        .await
                        .ok();
                }
            }
        }

        let repo_api = Api::<KanidmBackupRepository>::namespaced(client.clone(), "default");
        repo_api.delete(&repo_name, &Default::default()).await.ok();
        repo_api
            .delete(&safety_repo_name, &Default::default())
            .await
            .ok();

        let secret_api =
            Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), "default");
        secret_api
            .delete(&kek_secret_name, &Default::default())
            .await
            .ok();

        let job_api = Api::<Job>::namespaced(client.clone(), "default");
        for suffix in ["-upload", "-source-prep", "-safety-backup"] {
            job_api
                .delete(&format!("{name}{suffix}"), &Default::default())
                .await
                .ok();
        }

        let cm_api =
            Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(client.clone(), "default");
        cm_api
            .delete(&format!("{name}-upload-op"), &Default::default())
            .await
            .ok();

        poll_until("pre-test cleanup done", || {
            let restore_api = restore_api.clone();
            let name = format!("{name}-restore");
            async move {
                if restore_api.get(&name).await.is_err() {
                    Some(())
                } else {
                    None
                }
            }
        })
        .await;

        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        let safety_repo = KanidmBackupRepository::new(
            &safety_repo_name,
            KanidmBackupRepositorySpec {
                s3: minio_s3_config("e2e-wrong-kek-safety"),
                authentication: minio_auth(MINIO_CREDS_SECRET),
                encryption: None,
                limits: None,
            },
        );
        repo_api
            .create(&PostParams::default(), &safety_repo)
            .await
            .unwrap();
        wait_for(
            repo_api.clone(),
            &safety_repo_name,
            |obj: Option<&KanidmBackupRepository>| {
                obj.and_then(|repo| repo.status.as_ref())
                    .is_some_and(|status| {
                        status.conditions.iter().any(|c| {
                            c.type_ == "Ready" && c.status == "True" && c.reason == "Accepted"
                        })
                    })
            },
        )
        .await;

        create_kek_secret_restore(&s.client, &kek_secret_name, &[0x42u8; 32]).await;

        let encrypted_repo = KanidmBackupRepository::new(
            &repo_name,
            KanidmBackupRepositorySpec {
                s3: minio_s3_config("e2e-wrong-kek"),
                authentication: minio_auth(MINIO_CREDS_SECRET),
                encryption: Some(RepositoryEncryption {
                    mode: EncryptionMode::ClientSide,
                    key_id: None,
                    key_ref: Some(SecretRef {
                        name: kek_secret_name.clone(),
                    }),
                }),
                limits: None,
            },
        );
        repo_api
            .create(&PostParams::default(), &encrypted_repo)
            .await
            .unwrap();
        wait_for(
            repo_api.clone(),
            &repo_name,
            |obj: Option<&KanidmBackupRepository>| {
                obj.and_then(|repo| repo.status.as_ref())
                    .is_some_and(|status| {
                        status.conditions.iter().any(|c| {
                            c.type_ == "Ready" && c.status == "True" && c.reason == "Accepted"
                        })
                    })
            },
        )
        .await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let mut sts = s.statefulset_api.get(&sts_name).await.unwrap();
        sts.spec.as_mut().unwrap().replicas = Some(0);
        sts.metadata.managed_fields = None;
        s.statefulset_api
            .patch(
                &sts_name,
                &kube::api::PatchParams::apply("e2e-test").force(),
                &kube::api::Patch::Apply(&sts),
            )
            .await
            .unwrap();

        poll_until("kanidm scaled to 0", || {
            let statefulset_api = s.statefulset_api.clone();
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
            "e2e-wrong-kek/v1/tenants/{namespace_uid}/clusters/{kanidm_uid}/backups/{backup_id}/manifest.json"
        );

        let domain = s.kanidm_api.get(name).await.unwrap().spec.domain.clone();

        let operation_doc = serde_json::json!({
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "OperationDocument",
            "operation": "upload",
            "payloadPath": format!("/data/{backup_name}"),
            "bucket": MINIO_BUCKET,
            "prefix": "e2e-wrong-kek",
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
            "encryptionMode": "clientSide",
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
                                {"name": "KANIOP_ENCRYPTION_KEY", "valueFrom": {"secretKeyRef": {"name": kek_secret_name, "key": "encryption-key"}}},
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
        backup_api
            .create(&PostParams::default(), &backup_cr)
            .await
            .unwrap();

        wait_for(
            backup_api.clone(),
            &backup_cr_name,
            |obj: Option<&KanidmBackup>| {
                obj.and_then(|backup| backup.status.as_ref())
                    .is_some_and(|status| status.phase == KanidmBackupPhase::Ready)
            },
        )
        .await;

        let mut wrong_kek_secret = secret_api.get(&kek_secret_name).await.unwrap();
        wrong_kek_secret.data = None;
        wrong_kek_secret.string_data = Some(std::collections::BTreeMap::from([(
            "encryption-key".to_string(),
            "99999999999999999999999999999999".to_string(),
        )]));
        wrong_kek_secret.metadata.managed_fields = None;
        secret_api
            .patch(
                &kek_secret_name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&wrong_kek_secret),
            )
            .await
            .unwrap();

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
                        name: safety_repo_name.clone(),
                    }),
                    skip: false,
                }),
            },
        );

        restore_api
            .create(&PostParams::default(), &restore)
            .await
            .unwrap();

        wait_for(
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
            "database_mutation_started must be false after KEK failure"
        );
        let message = status
            .message
            .as_ref()
            .expect("failure message should exist");
        assert!(
            message.contains("source preparation") || message.contains("decryption"),
            "failure message should mention source preparation or decryption, got: {message}"
        );

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

        cleanup_restore_test_resources(name).await;
        repo_api.delete(&repo_name, &Default::default()).await.ok();
        repo_api
            .delete(&safety_repo_name, &Default::default())
            .await
            .ok();
        secret_api
            .delete(&kek_secret_name, &Default::default())
            .await
            .ok();
    }
);

async fn create_kek_secret_restore(client: &Client, name: &str, key_value: &[u8]) {
    let secret_api =
        Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), "default");
    let secret = k8s_openapi::api::core::v1::Secret {
        metadata: kube::api::ObjectMeta {
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

e2e_test!(
    #[serial(restore)]
    restore_truncated_remote_payload_fails_before_mutation_and_resumes_service,
    {
        let name = "test-trunc-remote-payload";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();
        cleanup_test_resources(&client, name, &repo_name).await;

        let (s, kanidm_uid, image) = setup_kanidm_with_backup(name).await;

        create_repository(
            &s.client,
            &repo_name,
            "e2e-trunc-remote",
            MINIO_CREDS_SECRET,
        )
        .await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(s.client.clone(), "default");
        wait_for(repo_api.clone(), &repo_name, super::is_repo_ready()).await;

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let pvc_name = format!("kanidm-data-{sts_name}-0");
        let trunc_job_name = format!("{name}-corrupt-trunc");
        let job_api = Api::<Job>::namespaced(s.client.clone(), "default");
        let trunc_job: Job = serde_json::from_value(json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {
                "name": trunc_job_name,
                "namespace": "default"
            },
            "spec": {
                "backoffLimit": 1,
                "template": {
                    "spec": {
                        "restartPolicy": "Never",
                        "containers": [{
                            "name": "truncater",
                            "image": "busybox:latest",
                            "command": ["sh", "-c", format!("dd if=/data/{} bs=16 count=1 of=/data/{} 2>/dev/null", backup_name, backup_name)],
                            "volumeMounts": [{
                                "name": "data",
                                "mountPath": "/data"
                            }]
                        }],
                        "volumes": [{
                            "name": "data",
                            "persistentVolumeClaim": {"claimName": pvc_name}
                        }]
                    }
                }
            }
        }))
        .unwrap();

        job_api
            .create(&PostParams::default(), &trunc_job)
            .await
            .expect("trunc job create should succeed");

        poll_until("trunc job completes", || {
            let job_api = job_api.clone();
            let job_name = trunc_job_name.clone();
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
            .delete(&trunc_job_name, &Default::default())
            .await
            .ok();

        let backup_id = uuid::Uuid::new_v4().to_string();
        let domain = s.kanidm_api.get(name).await.unwrap().spec.domain.clone();

        let manifest_key = upload_backup_to_s3(
            &s.client,
            super::UploadOptions::new(
                name,
                "e2e-trunc-remote",
                &backup_name,
                &backup_id,
                &kanidm_uid,
                &domain,
            ),
        )
        .await;

        let backup_cr_name = create_backup_cr_and_wait(
            &s.client,
            &backup_id,
            name,
            &kanidm_uid,
            &repo_name,
            &manifest_key,
        )
        .await;

        let backup_api = Api::<KanidmBackup>::namespaced(s.client.clone(), "default");
        let mismatched_sha256 =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        let patch = serde_json::json!({
            "status": {
                "payloadSha256": mismatched_sha256
            }
        });
        backup_api
            .patch_status(
                &backup_cr_name,
                &PatchParams::apply("e2e-test"),
                &Patch::Merge(&patch),
            )
            .await
            .unwrap();

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

        wait_for(
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
            "database_mutation_started must be false after truncated remote payload failure"
        );

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);

e2e_test!(
    #[serial(restore)]
    restore_remote_semantic_drill_with_identity_data,
    {
        use crate::test::{create_fresh_authenticated_client, setup_kanidm_connection};
        use kaniop_group::crd::KanidmGroup;
        use kaniop_oauth2::crd::KanidmOAuth2Client;
        use kaniop_person::crd::KanidmPersonAccount;
        use kaniop_service_account::crd::KanidmServiceAccount;

        let name = "test-semantic-drill";
        let repo_name = format!("{name}-repo");

        init_crypto_provider();
        let client = Client::try_default().await.unwrap();

        let person_name = format!("{name}-alice");
        let group_name = format!("{name}-engineers");
        let oauth2_name = format!("{name}-webapp");
        let sa_name = format!("{name}-ci-bot");

        let person_api = Api::<KanidmPersonAccount>::namespaced(client.clone(), "default");
        let group_api = Api::<KanidmGroup>::namespaced(client.clone(), "default");
        let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
        let sa_api = Api::<KanidmServiceAccount>::namespaced(client.clone(), "default");

        person_api
            .delete(&person_name, &Default::default())
            .await
            .ok();
        group_api
            .delete(&group_name, &Default::default())
            .await
            .ok();
        oauth2_api
            .delete(&oauth2_name, &Default::default())
            .await
            .ok();
        sa_api.delete(&sa_name, &Default::default()).await.ok();

        cleanup_test_resources(&client, name, &repo_name).await;

        let s = setup(
            name,
            Some(json!({
                "domain": format!("{name}.localhost"),
                "ingress": {
                    "annotations": {
                        "nginx.ingress.kubernetes.io/backend-protocol": "HTTPS",
                    }
                },
                "storage": STORAGE_VOLUME_CLAIM_TEMPLATE_JSON["storage"].clone(),
                "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1, "primaryNode": true}]
            })),
        )
        .await;

        create_repository(
            &s.client,
            &repo_name,
            "e2e-semantic-drill",
            MINIO_CREDS_SECRET,
        )
        .await;
        let repo_api = Api::<KanidmBackupRepository>::namespaced(s.client.clone(), "default");
        wait_for(repo_api.clone(), &repo_name, super::is_repo_ready()).await;

        let kanidm_conn = setup_kanidm_connection(name).await;

        let person = KanidmPersonAccount::new(
            &person_name,
            serde_json::from_value(json!({
                "kanidmRef": {"name": name},
                "personAttributes": {
                    "displayname": "Alice Drill",
                    "mail": ["alice-drill@example.com"],
                },
            }))
            .unwrap(),
        );
        person_api
            .create(&PostParams::default(), &person)
            .await
            .unwrap();

        let group = KanidmGroup::new(
            &group_name,
            serde_json::from_value(json!({
                "kanidmRef": {"name": name},
                "mail": ["engineers-drill@example.com"],
            }))
            .unwrap(),
        );
        group_api
            .create(&PostParams::default(), &group)
            .await
            .unwrap();

        let oauth2 = KanidmOAuth2Client::new(
            &oauth2_name,
            serde_json::from_value(json!({
                "kanidmRef": {"name": name},
                "redirectUrl": ["https://webapp-drill.example.com/callback"],
                "displayname": "Drill WebApp",
                "origin": "https://webapp-drill.example.com",
                "public": false,
            }))
            .unwrap(),
        );
        oauth2_api
            .create(&PostParams::default(), &oauth2)
            .await
            .unwrap();

        let sa = KanidmServiceAccount::new(
            &sa_name,
            serde_json::from_value(json!({
                "kanidmRef": {"name": name},
                "serviceAccountAttributes": {
                    "displayname": "CI Drill Bot",
                    "entryManagedBy": "idm_admin",
                },
            }))
            .unwrap(),
        );
        sa_api.create(&PostParams::default(), &sa).await.unwrap();

        let kanidm_client = &kanidm_conn.kanidm_client;
        poll_until("person exists in kanidm", || {
            let pn = person_name.clone();
            async move {
                kanidm_client
                    .idm_person_account_get(&pn)
                    .await
                    .ok()
                    .flatten()
            }
        })
        .await;

        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let kanidm_uid = kanidm.uid().unwrap();
        let image = kanidm.spec.image.clone();
        let domain = kanidm.spec.domain.clone();

        let person_kanidm = kanidm_conn
            .kanidm_client
            .idm_person_account_get(&person_name)
            .await
            .unwrap()
            .expect("person should exist before backup");
        let person_displayname = person_kanidm
            .attrs
            .get("displayname")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default();
        assert_eq!(person_displayname, "Alice Drill");

        let backup_name = trigger_backup_on_primary(&s, name).await;

        let backup_id = uuid::Uuid::new_v4().to_string();

        let manifest_key = upload_backup_to_s3(
            &s.client,
            super::UploadOptions::new(
                name,
                "e2e-semantic-drill",
                &backup_name,
                &backup_id,
                &kanidm_uid,
                &domain,
            ),
        )
        .await;

        let backup_cr_name = create_backup_cr_and_wait(
            &s.client,
            &backup_id,
            name,
            &kanidm_uid,
            &repo_name,
            &manifest_key,
        )
        .await;

        person_api
            .delete(&person_name, &Default::default())
            .await
            .unwrap();
        poll_until("person deleted after backup", || {
            let person_api = person_api.clone();
            let person_name = person_name.clone();
            async move {
                if person_api.get(&person_name).await.is_err() {
                    Some(())
                } else {
                    None
                }
            }
        })
        .await;

        let mut group_after = group_api.get(&group_name).await.unwrap();
        group_after.metadata.annotations = Some(
            [("post-backup-mutation".to_string(), "true".to_string())]
                .into_iter()
                .collect(),
        );
        group_after.metadata.managed_fields = None;
        group_api
            .patch(
                &group_name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&group_after),
            )
            .await
            .unwrap();

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

        wait_for(
            restore_api.clone(),
            &restore_name,
            is_restore_phase(KanidmRestorePhase::Completed),
        )
        .await;

        let final_restore = restore_api.get(&restore_name).await.unwrap();
        let restore_status = final_restore.status.unwrap();
        assert_eq!(restore_status.phase, KanidmRestorePhase::Completed);
        assert!(restore_status.database_mutation_started);

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Initialized")).await;

        let fresh_client = create_fresh_authenticated_client(name).await;

        let restored_person = fresh_client
            .idm_person_account_get(&person_name)
            .await
            .unwrap();
        assert!(
            restored_person.is_some(),
            "person should be recovered after restore to the backup point"
        );
        let restored_displayname = restored_person
            .unwrap()
            .attrs
            .get("displayname")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            restored_displayname, "Alice Drill",
            "person displayname should match the backup point"
        );

        let restored_group = fresh_client.idm_group_get(&group_name).await.unwrap();
        assert!(restored_group.is_some(), "group should exist after restore");

        let restored_oauth2 = fresh_client.idm_oauth2_rs_get(&oauth2_name).await.unwrap();
        assert!(
            restored_oauth2.is_some(),
            "OAuth2 client should exist after restore"
        );

        let restored_sa = fresh_client
            .idm_service_account_get(&sa_name)
            .await
            .unwrap();
        assert!(
            restored_sa.is_some(),
            "service account should exist after restore"
        );

        person_api
            .delete(&person_name, &Default::default())
            .await
            .ok();
        group_api
            .delete(&group_name, &Default::default())
            .await
            .ok();
        oauth2_api
            .delete(&oauth2_name, &Default::default())
            .await
            .ok();
        sa_api.delete(&sa_name, &Default::default()).await.ok();

        cleanup_test_resources(&s.client, name, &repo_name).await;
    }
);
