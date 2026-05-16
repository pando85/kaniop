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
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct CratesVersionsResponse {
    versions: Vec<CrateVersion>,
}

#[derive(Deserialize)]
struct CrateVersion {
    num: String,
    yanked: bool,
}

fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 3 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts[2].split('-').next()?.parse().ok()?;
        Some((major, minor, patch))
    } else {
        None
    }
}

fn fetch_previous_minor_from_crates_io(current_major: u64, current_minor: u64) -> Option<String> {
    let response: CratesVersionsResponse =
        ureq::get("https://crates.io/api/v1/crates/kanidm_client/versions?per_page=100")
            .header("User-Agent", "kaniop-e2e-test")
            .call()
            .ok()?
            .body_mut()
            .read_json()
            .ok()?;

    let mut best: Option<(u64, u64, u64)> = None;
    for v in &response.versions {
        if v.yanked {
            continue;
        }
        let Some((major, minor, patch)) = parse_semver(&v.num) else {
            continue;
        };
        if major == current_major && minor == current_minor {
            continue;
        }
        let is_previous_minor = if current_minor > 0 {
            major == current_major && minor == current_minor - 1
        } else {
            major == current_major - 1
        };
        if !is_previous_minor {
            continue;
        }
        if best.is_none_or(|(b_major, b_minor, b_patch)| {
            (major, minor, patch) > (b_major, b_minor, b_patch)
        }) {
            best = Some((major, minor, patch));
        }
    }
    best.map(|(major, minor, patch)| format!("{major}.{minor}.{patch}"))
}

fn previous_minor_version() -> String {
    let current = get_dependency_version().unwrap();
    let (current_major, current_minor, _) = parse_semver(&current).unwrap();

    if current_minor > 0 {
        format!("{current_major}.{}.0", current_minor - 1)
    } else {
        fetch_previous_minor_from_crates_io(current_major, current_minor)
            .expect("Failed to determine previous minor version from crates.io")
    }
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
