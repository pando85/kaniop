use serial_test::serial;

use super::{
    DEFAULT_REPLICA_GROUP_NAME, STORAGE_VOLUME_CLAIM_TEMPLATE_JSON, is_kanidm, is_kanidm_false,
    is_statefulset_ready, setup, wait_for, wait_for_replication_success_with_timeout,
};
use crate::kanidm::get_dependency_version;
use crate::test::poll_until;

use json_patch::merge;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, Patch, PatchParams};
use serde_json::json;

fn previous_minor_version() -> String {
    let current = get_dependency_version().unwrap();
    let parts: Vec<&str> = current.split('.').collect();
    let major: u64 = parts[0].parse().unwrap();
    let minor: u64 = parts[1].parse().unwrap();
    format!("{major}.{}.0", minor - 1)
}

fn get_statefulset_image(sts: &StatefulSet) -> String {
    sts.spec
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
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[serial(replication)]
async fn kanidm_upgrade_ha_cluster() {
    let name = "test-upgrade-ha-cluster";
    let prev_version = previous_minor_version();
    let current_version = get_dependency_version().unwrap();
    let prev_image = format!("kanidm/server:{prev_version}");
    let current_image = format!("kanidm/server:{current_version}");

    let mut spec_patch = json!({
        "image": prev_image,
        "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 2, "primaryNode": true}],
    });
    merge(&mut spec_patch, &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone());

    let s = setup(name, Some(spec_patch)).await;

    let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
    let sts = s.statefulset_api.get(&sts_name).await.unwrap();
    assert_eq!(sts.spec.as_ref().unwrap().replicas.unwrap(), 2);
    assert_eq!(get_statefulset_image(&sts), prev_image);

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm("Initialized")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
    let pod_names = (0..2)
        .map(|i| format!("{sts_name}-{i}"))
        .collect::<Vec<_>>();
    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;

    let mut kanidm = s.kanidm_api.get(name).await.unwrap();
    kanidm.spec.image = current_image.clone();
    kanidm.metadata.managed_fields = None;
    s.kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    let statefulset_api = s.statefulset_api.clone();
    let expected_image = current_image.clone();
    poll_until("StatefulSet image updated", || async {
        let sts = statefulset_api.get(&sts_name).await.ok()?;
        let image = get_statefulset_image(&sts);
        if image == expected_image {
            Some(())
        } else {
            None
        }
    })
    .await;

    wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
    wait_for(s.statefulset_api.clone(), &sts_name, is_statefulset_ready).await;

    let upgraded_sts = s.statefulset_api.get(&sts_name).await.unwrap();
    assert_eq!(get_statefulset_image(&upgraded_sts), current_image);
    assert_eq!(upgraded_sts.spec.as_ref().unwrap().replicas.unwrap(), 2);

    wait_for_replication_success_with_timeout(&pod_api, &pod_names).await;
}
