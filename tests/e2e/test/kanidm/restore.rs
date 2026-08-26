use serial_test::serial;

use super::{
    DEFAULT_REPLICA_GROUP_NAME, KANIDM_DEFAULT_SPEC_JSON, STORAGE_VOLUME_CLAIM_TEMPLATE_JSON,
    is_kanidm, is_kanidm_false, is_statefulset_ready, setup, wait_for,
    wait_for_replication_success_with_timeout,
};
use crate::test::{init_crypto_provider, poll_until};

use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::{
    BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION, KanidmRestore,
    KanidmRestoreLocalSource, KanidmRestorePhase, KanidmRestoreSource, KanidmRestoreSpec,
    KanidmRestoreTargetRef, SafetyBackupConfig,
};

use std::time::Duration;

use json_patch::merge;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;
use kube::api::{Api, PostParams};
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

        let sts_after = s.statefulset_api.get(&sts_name).await.unwrap();
        assert_eq!(
            sts_after.spec.as_ref().unwrap().replicas.unwrap(),
            2,
            "StatefulSet should have 2 replicas after restore"
        );

        wait_for(s.statefulset_api.clone(), &sts_name, is_statefulset_ready).await;

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
                "backup": {
                    "schedule": "0 0 * * *"
                }
            }),
        );

        let kanidm = Kanidm::new(&kanidm_name, serde_json::from_value(spec_json).unwrap());
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");

        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;
        assert!(
            result.is_err(),
            "ephemeral storage with primary_node should be rejected"
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
