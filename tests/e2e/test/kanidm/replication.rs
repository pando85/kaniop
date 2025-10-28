use super::{
    DEFAULT_REPLICA_GROUP_NAME, KANIDM_DEFAULT_SPEC_JSON, STORAGE_VOLUME_CLAIM_TEMPLATE_JSON,
    is_kanidm, is_kanidm_false, setup, wait_for, wait_for_replication_success_with_timeout,
};

use kaniop_operator::kanidm::crd::{Kanidm, KanidmReplicaGroupServices, ReplicaGroup};
use kaniop_operator::kanidm::reconcile::secret::SecretExt;
use kaniop_operator::kanidm::reconcile::statefulset::StatefulSetExt;

use std::time::Duration;

use json_patch::merge;
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::client::Client;
use serde_json::json;

#[tokio::test]
async fn kanidm_no_replica_groups() {
    let client = Client::try_default().await.unwrap();
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch = json!({
        "replicaGroups": [],
    });

    merge(&mut kanidm_spec_json, &patch);
    let kanidm = Kanidm::new(
        "test-no-replica-groups",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("spec.replicaGroups in body should have at least 1 items")
    );
}

#[tokio::test]
async fn kanidm_no_replication_with_ephemeral_storage() {
    let client = Client::try_default().await.unwrap();
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch = json!({
        "replicaGroups": [
            {"name": "default", "replicas": 2},
        ],
    });

    merge(&mut kanidm_spec_json, &patch);
    let kanidm = Kanidm::new(
        "no-replication-with-ephemeral-storage",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    dbg!(&result);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Replication not available for ephemeral storage.")
    );
}

#[tokio::test]
async fn kanidm_replica_groups_same_name() {
    let client = Client::try_default().await.unwrap();
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch_rgs = json!({
        "replicaGroups": [
            {"name": "same-name", "replicas": 1},
            {"name": "same-name", "replicas": 1},
        ],
    });
    let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

    merge(&mut kanidm_spec_json, &patch_rgs);
    merge(&mut kanidm_spec_json, &patch_storage);

    let kanidm = Kanidm::new(
        "test-replica-groups-same-name",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    dbg!(&result);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Replica group names must be unique.")
    );
}

#[tokio::test]
async fn kanidm_replica_groups_read_replica_primary() {
    let client = Client::try_default().await.unwrap();
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch_rgs = json!({
        "replicaGroups": [
            {"name": "default", "replicas": 1},
            {"name": "read-replica", "replicas": 1, "role": "read_only_replica", "primaryNode": true},
        ],
    });
    let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

    merge(&mut kanidm_spec_json, &patch_rgs);
    merge(&mut kanidm_spec_json, &patch_storage);

    let kanidm = Kanidm::new(
        "test-replica-groups-read-replica-primary",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    dbg!(&result);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains(
        "Primary node only can be true if role is 'write_replica' or 'write_replica_no_ui'."
    ));
}

#[tokio::test]
async fn kanidm_replica_groups_two_primary() {
    let client = Client::try_default().await.unwrap();
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch_rgs = json!({
        "replicaGroups": [
            {"name": "default", "replicas": 1, "primaryNode": true},
            {"name": "write-replica", "replicas": 1, "primaryNode": true},
        ],
    });
    let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

    merge(&mut kanidm_spec_json, &patch_rgs);
    merge(&mut kanidm_spec_json, &patch_storage);

    let kanidm = Kanidm::new(
        "test-replica-groups-two-primary",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    dbg!(&result);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains(
        "Only one primary node replica group or automatic refresh external node is allowed."
    ));
}

#[tokio::test]
async fn kanidm_change_kanidm_replica_groups() {
    let name = "test-change-kanidm-replica-groups";
    let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups.push(ReplicaGroup {
        name: "new".to_string(),
        replicas: 1,
        primary_node: true,
        ..ReplicaGroup::default()
    });
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let sts_names = kanidm
        .spec
        .replica_groups
        .iter()
        .map(|rg| kanidm.statefulset_name(&rg.name))
        .collect::<Vec<_>>();
    for sts_name in sts_names.clone() {
        let check_sts = s.statefulset_api.get(&sts_name).await.unwrap();

        assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 1);
        let sts_name = check_sts.name_any();
        let secret_name = kanidm.replica_secret_name(&format!("{sts_name}-0"));
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        assert_eq!(secret.data.unwrap().len(), 1);
    }
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    let pod_names = sts_names
        .iter()
        .map(|sts_name| format!("{sts_name}-0"))
        .collect::<Vec<_>>();
    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
}

#[tokio::test]
async fn kanidm_replica_groups_one_read_only() {
    let name = "test-replica-groups-one-read-only";
    let mut patch_rgs = json!({
        "replicaGroups": [
            {"name": "default", "replicas": 1, "primaryNode": true},
            {"name": "read", "replicas": 1, "role": "read_only_replica"},
        ],
    });
    let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();
    merge(&mut patch_rgs, &patch_storage);

    let s = setup(name, Some(patch_rgs)).await;
    let kanidm = s.kanidm_api.get(name).await.unwrap();
    let sts_names = kanidm
        .spec
        .replica_groups
        .iter()
        .map(|rg| kanidm.statefulset_name(&rg.name))
        .collect::<Vec<_>>();
    for sts_name in sts_names.clone() {
        let check_sts = s.statefulset_api.get(&sts_name).await.unwrap();

        assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 1);
        let sts_name = check_sts.name_any();
        let secret_name = kanidm.replica_secret_name(&format!("{sts_name}-0"));
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        assert_eq!(secret.data.unwrap().len(), 1);
    }

    // wait for restarts
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    let sts_name_read_only = sts_names.last().unwrap();
    let pod_names = vec![format!("{sts_name_read_only}-0")];
    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
}

#[tokio::test]
async fn kanidm_delete_replica_group() {
    let name = "test-delete-replica-group";
    let mut kanidm_path = json!({
        "replicaGroups": [
            {"name": "default", "replicas": 1},
            {"name": "to-delete", "replicas": 1},
        ],
    });
    let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

    merge(&mut kanidm_path, &patch_storage);
    let s = setup(name, Some(kanidm_path.clone())).await;
    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups.pop();
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let sts_name = kanidm.statefulset_name("to-delete");
    let check_sts = s.statefulset_api.get(&sts_name).await;

    assert!(check_sts.is_err());
    let pod_name = format!("{sts_name}-0");
    let secret = kanidm.replica_secret_name(&pod_name);
    let check_secret = s.secret_api.get(&secret).await;
    assert!(check_secret.is_err());
}

#[tokio::test]
async fn kanidm_replica_groups_one_primary_and_external_node_automatic_refresh() {
    let client = Client::try_default().await.unwrap();
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch_rgs = json!({
        "replicaGroups": [
            {"name": "default", "replicas": 1, "primaryNode": true}
        ],
        "externalReplicationNodes": [{
            "name": "external-node",
            "hostname": "host-0",
            "port": 8444,
            "certificate": {
                "name": "external-node-cert",
                "key": "tls.der.b64url",
            },
            "automaticRefresh": true
        }]
    });
    let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

    merge(&mut kanidm_spec_json, &patch_rgs);
    merge(&mut kanidm_spec_json, &patch_storage);

    let kanidm = Kanidm::new(
        "test-replica-groups-one-primary-one-external",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    dbg!(&result);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains(
        "Only one primary node replica group or automatic refresh external node is allowed."
    ));
}

#[tokio::test]
async fn kanidm_no_replication_with_ephemeral_storage_external_replication_node() {
    let client = Client::try_default().await.unwrap();
    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    let patch = json!({
        "externalReplicationNodes": [
            {
                "name": "external-node",
                "hostname": "host-0",
                "port": 8444,
                "certificate": {
                    "name": "external-node-cert",
                    "key": "tls.der.b64url",
                }
            }
        ],
    });

    merge(&mut kanidm_spec_json, &patch);

    let kanidm = Kanidm::new(
        "no-replication-with-ephemeral-storage-ern",
        serde_json::from_value(kanidm_spec_json).unwrap(),
    );
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

    dbg!(&result);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Replication not available for ephemeral storage.")
    );
}

#[tokio::test]
async fn kanidm_external_replication_node() {
    let kanidms_params = [
        (
            "test-external-replication-node-0",
            "test-external-replication-node-1-default-0.test-external-replication-node-1",
            "test-external-replication-node-1-default-0-cert",
            false,
        ),
        (
            "test-external-replication-node-1",
            "test-external-replication-node-0-default-0.test-external-replication-node-0",
            "test-external-replication-node-0-default-0-cert",
            true,
        ),
    ];

    let mut s = None;

    for (name, hostname, cert_name, automatic_refresh) in &kanidms_params {
        let mut kanidm_path = json!({
            "externalReplicationNodes": [
                {
                    "name": "external-node",
                    "hostname": hostname,
                    "port": 8444,
                    "certificate": {
                        "name": cert_name,
                        "key": "tls.der.b64url",
                        "optional": true,
                    },
                    "automaticRefresh": *automatic_refresh,
                }
            ],
        });

        merge(
            &mut kanidm_path,
            &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone(),
        );
        let setup_result = setup(name, Some(kanidm_path.clone())).await;
        if s.is_none() {
            s = Some(setup_result);
        }
    }

    let s = s.unwrap();

    dbg!("setup done");
    for (name, _, _, _) in &kanidms_params {
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
    }
    dbg!("kanidms available");

    for (name, _, _, _) in &kanidms_params {
        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        s.statefulset_api.restart(&sts_name).await.unwrap();
        dbg!(format!("restarting sts/{sts_name}"));
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
    }

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    let pod_names = kanidms_params
        .iter()
        .map(|(name, _, _, _)| format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}-0"))
        .collect::<Vec<_>>();
    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
}

#[tokio::test]
async fn kanidm_replication_with_services() {
    let name = "test-replication-with-services";
    let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups.push(ReplicaGroup {
        name: "new".to_string(),
        replicas: 1,
        primary_node: true,
        services: Some(KanidmReplicaGroupServices {
            ..KanidmReplicaGroupServices::default()
        }),
        ..ReplicaGroup::default()
    });
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let sts_names = kanidm
        .spec
        .replica_groups
        .iter()
        .map(|rg| kanidm.statefulset_name(&rg.name))
        .collect::<Vec<_>>();
    for sts_name in sts_names.clone() {
        let check_sts = s.statefulset_api.get(&sts_name).await.unwrap();

        assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 1);
        let sts_name = check_sts.name_any();
        let secret_name = kanidm.replica_secret_name(&format!("{sts_name}-0"));
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        assert_eq!(secret.data.unwrap().len(), 1);
    }
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    let pod_names = sts_names
        .iter()
        .map(|sts_name| format!("{sts_name}-0"))
        .collect::<Vec<_>>();
    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
}

#[tokio::test]
async fn kanidm_replication_change_services() {
    let name = "test-replication-change-services";
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

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let sts_name = kanidm.statefulset_name(&kanidm.spec.replica_groups[0].name);
    for i in 0..2 {
        let check_sts = s.statefulset_api.get(&sts_name).await.unwrap();

        assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 2);
        let sts_name = check_sts.name_any();
        let secret_name = kanidm.replica_secret_name(&format!("{sts_name}-{i}"));
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        assert_eq!(secret.data.unwrap().len(), 1);
    }

    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    let pod_names = (0..2)
        .map(|i| format!("{sts_name}-{i}"))
        .collect::<Vec<_>>();
    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups[0].services = Some(KanidmReplicaGroupServices {
        ..KanidmReplicaGroupServices::default()
    });
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    // 60s per certificate renewal
    tokio::time::sleep(Duration::from_secs(60 * 2)).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.replica_groups[0].services = None;
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    // 60s per certificate renewal
    tokio::time::sleep(Duration::from_secs(60 * 2)).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
}
