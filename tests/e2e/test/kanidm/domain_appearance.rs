use super::{is_kanidm, is_kanidm_false, setup, wait_for};
use crate::kanidm::get_dependency_version;
use crate::test::init_crypto_provider;

use std::sync::LazyLock;

use backon::{ExponentialBuilder, Retryable};
use kube::api::{Api, Patch, PatchParams};
use serde_json::json;

const DEFAULT_REPLICA_GROUP_NAME: &str = "default";

static KANIDM_DEFAULT_SPEC_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "domain": "idm.example.com",
        "image": format!("kanidm/server:{}", get_dependency_version().unwrap()),
        "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1}],
    })
});

static DOMAIN_SVG_URL: &str =
    "https://cdn.jsdelivr.net/gh/homarr-labs/dashboard-icons/svg/argo-cd.svg";

fn is_domain_appearance_image_updated(obj: Option<&kaniop_operator::kanidm::crd::Kanidm>) -> bool {
    obj.and_then(|kanidm| kanidm.status.as_ref())
        .and_then(|status| status.domain_appearance_image.as_ref())
        .is_some_and(|image| image.url == DOMAIN_SVG_URL)
}

#[tokio::test]
async fn kanidm_domain_display_name() {
    let name = "test-domain-display-name";
    init_crypto_provider();

    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    kanidm_spec_json["domain"] = format!("{name}.localhost").into();
    kanidm_spec_json["domainAppearance"] = json!({
        "displayName": "My Custom Identity Portal"
    });

    let s = setup(name, Some(kanidm_spec_json)).await;

    let kanidm_api =
        Api::<kaniop_operator::kanidm::crd::Kanidm>::namespaced(s.client.clone(), "default");
    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    let kanidm_status = kanidm_api.get(name).await.unwrap().status.unwrap();

    assert!(
        kanidm_status.available_replicas > 0,
        "Kanidm should have available replicas"
    );
}

#[tokio::test]
async fn kanidm_domain_image_fetch_https() {
    let name = "test-domain-image-fetch";
    init_crypto_provider();

    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    kanidm_spec_json["domain"] = format!("{name}.localhost").into();
    kanidm_spec_json["domainAppearance"] = json!({
        "displayName": "Test Identity Portal",
        "image": {
            "url": DOMAIN_SVG_URL
        }
    });

    let s = setup(name, Some(kanidm_spec_json)).await;

    let kanidm_api =
        Api::<kaniop_operator::kanidm::crd::Kanidm>::namespaced(s.client.clone(), "default");
    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

    wait_for(kanidm_api.clone(), name, is_domain_appearance_image_updated).await;

    let kanidm = kanidm_api.get(name).await.unwrap();
    let kanidm_status = kanidm.status.unwrap();

    assert!(
        kanidm_status.domain_appearance_image.is_some(),
        "Kanidm status should have domain appearance image after fetch"
    );

    let image_status = kanidm_status.domain_appearance_image.unwrap();
    assert_eq!(
        image_status.url, DOMAIN_SVG_URL,
        "Image status should show the fetched URL"
    );
    assert!(
        image_status.content_hash.is_some(),
        "Image status should have content hash"
    );
    assert!(
        image_status.content_length.is_some(),
        "Image status should have content length"
    );
}

#[tokio::test]
async fn kanidm_domain_appearance_remove_image() {
    let name = "test-domain-image-remove";
    init_crypto_provider();

    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    kanidm_spec_json["domain"] = format!("{name}.localhost").into();
    kanidm_spec_json["domainAppearance"] = json!({
        "displayName": "Test Identity Portal",
        "image": {
            "url": DOMAIN_SVG_URL
        }
    });

    let s = setup(name, Some(kanidm_spec_json)).await;

    let kanidm_api =
        Api::<kaniop_operator::kanidm::crd::Kanidm>::namespaced(s.client.clone(), "default");
    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
    wait_for(kanidm_api.clone(), name, is_domain_appearance_image_updated).await;

    let kanidm_with_image = kanidm_api.get(name).await.unwrap();
    let status_with_image = kanidm_with_image.status.clone().unwrap();
    assert!(status_with_image.domain_appearance_image.is_some());

    let retryable_patch = || async {
        let kanidm = kanidm_api.get(name).await?;
        let mut patch_kanidm = kanidm.clone();
        patch_kanidm.spec.domain_appearance.as_mut().unwrap().image = None;
        patch_kanidm.metadata.managed_fields = None;
        kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&patch_kanidm),
            )
            .await
    };
    retryable_patch
        .retry(ExponentialBuilder::default().with_max_times(5))
        .await
        .unwrap();

    wait_for(
        kanidm_api.clone(),
        name,
        |obj: Option<&kaniop_operator::kanidm::crd::Kanidm>| {
            obj.and_then(|kanidm| kanidm.spec.domain_appearance.as_ref())
                .and_then(|da| da.image.as_ref())
                .is_none()
        },
    )
    .await;

    wait_for(
        kanidm_api.clone(),
        name,
        |obj: Option<&kaniop_operator::kanidm::crd::Kanidm>| {
            obj.and_then(|kanidm| kanidm.status.as_ref())
                .and_then(|status| status.domain_appearance_image.as_ref())
                .is_none()
        },
    )
    .await;

    let kanidm_after = kanidm_api.get(name).await.unwrap();
    let status_after = kanidm_after.status.unwrap();
    assert!(
        status_after.domain_appearance_image.is_none(),
        "Domain appearance image should be removed from status after spec change"
    );
}

#[tokio::test]
async fn kanidm_domain_appearance_preserves_status_on_other_updates() {
    let name = "test-domain-preserve-status";
    init_crypto_provider();

    let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
    kanidm_spec_json["domain"] = format!("{name}.localhost").into();
    kanidm_spec_json["domainAppearance"] = json!({
        "displayName": "Test Identity Portal",
        "image": {
            "url": DOMAIN_SVG_URL
        }
    });

    let s = setup(name, Some(kanidm_spec_json)).await;

    let kanidm_api =
        Api::<kaniop_operator::kanidm::crd::Kanidm>::namespaced(s.client.clone(), "default");
    wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
    wait_for(kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
    wait_for(kanidm_api.clone(), name, is_domain_appearance_image_updated).await;

    let kanidm_with_image = kanidm_api.get(name).await.unwrap();
    let status_with_image = kanidm_with_image.status.clone().unwrap();
    let original_image_status = status_with_image.domain_appearance_image.clone();
    assert!(original_image_status.is_some());

    let mut kanidm = kanidm_with_image;
    kanidm.spec.log_level = kaniop_operator::kanidm::crd::KanidmLogLevel::Debug;
    kanidm.metadata.managed_fields = None;
    kanidm_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    wait_for(
        kanidm_api.clone(),
        name,
        |obj: Option<&kaniop_operator::kanidm::crd::Kanidm>| {
            obj.and_then(|kanidm| kanidm.status.as_ref())
                .and_then(|status| status.version.as_ref())
                .is_some()
        },
    )
    .await;

    let kanidm_after = kanidm_api.get(name).await.unwrap();
    let status_after = kanidm_after.status.unwrap();

    assert!(
        status_after.domain_appearance_image.is_some(),
        "Domain appearance image should be preserved after other spec updates"
    );

    let preserved_image_status = status_after.domain_appearance_image.unwrap();
    let original = original_image_status.as_ref().unwrap();
    assert_eq!(
        preserved_image_status.url, original.url,
        "Image URL should be preserved"
    );
    assert_eq!(
        preserved_image_status.content_hash, original.content_hash,
        "Image content hash should be preserved"
    );
}
