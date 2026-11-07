//! OAuth2 image reconciliation must respond to remote content changes without
//! requiring a Kubernetes spec update.

use super::{setup_kanidm_connection, wait_for};
use k8s_openapi::api::core::v1::{Pod, Service};
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kube::api::{Api, AttachParams, DeleteParams, LogParams, Patch, PatchParams, PostParams};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::{sleep, timeout};

// Different sizes as well as different contents: the built-in HTTP server
// provides Last-Modified and Content-Length validators, without a public CDN.
const IMAGE_V1: &str = "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><rect width='16' height='16' fill='red'/></svg>";
const IMAGE_V2: &str = "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><circle cx='8' cy='8' r='7' fill='dodgerblue'/></svg>";

fn has_synced_image(obj: Option<&KanidmOAuth2Client>, expected_hash: &str) -> bool {
    obj.and_then(|o| o.status.as_ref()).is_some_and(|status| {
        status
            .image
            .as_ref()
            .and_then(|image| image.content_hash.as_deref())
            == Some(expected_hash)
            && status.conditions.as_ref().is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == "ImageUpdated" && c.status == "True")
            })
    })
}

async fn image_requests(pods: &Api<Pod>, name: &str) -> (usize, usize) {
    let logs = pods.logs(name, &LogParams::default()).await.unwrap();
    (
        logs.matches("\"HEAD /logo.svg ").count(),
        logs.matches("\"GET /logo.svg ").count(),
    )
}

async fn wait_for_next_head(pods: &Api<Pod>, name: &str, previous: usize) -> (usize, usize) {
    timeout(Duration::from_secs(130), async {
        loop {
            let counts = image_requests(pods, name).await;
            if counts.0 > previous {
                return counts;
            }
            sleep(Duration::from_secs(2)).await;
        }
    })
    .await
    .expect("operator did not re-check image with HEAD")
}

e2e_test!(oauth2_image_periodic_refresh_and_removal, {
    let name = "test-oauth2-image-periodic-refresh";
    let fixture = format!(
        "oauth2-image-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let s = setup_kanidm_connection("test-oauth2").await;
    let pods: Api<Pod> = Api::namespaced(s.client.clone(), "default");
    let services: Api<Service> = Api::namespaced(s.client.clone(), "default");
    let oauth2_api: Api<KanidmOAuth2Client> = Api::namespaced(s.client.clone(), "default");

    // A real in-cluster HTTP server: the operator can reach the same URL even
    // though this test process runs outside the kind cluster. Updating the file
    // through Kubernetes exec keeps the URL and the OAuth2 spec unchanged.
    let bootstrap = format!(
        "mkdir -p /www && printf '%s' \"{IMAGE_V1}\" > /www/logo.svg && \
         exec python -u -m http.server 8080 --bind 0.0.0.0 --directory /www"
    );
    let pod: Pod = serde_json::from_value(json!({
        "metadata": {"name": fixture, "labels": {"image-fixture": fixture}},
        "spec": {
            "containers": [{
                "name": "http",
                "image": "python:3.13-alpine",
                "command": ["sh", "-c", bootstrap],
                "ports": [{"containerPort": 8080}],
                "readinessProbe": {
                    "tcpSocket": {"port": 8080},
                    "periodSeconds": 1
                }
            }]
        }
    }))
    .unwrap();
    let service: Service = serde_json::from_value(json!({
        "metadata": {"name": fixture},
        "spec": {
            "selector": {"image-fixture": fixture},
            "ports": [{"port": 8080, "targetPort": 8080}]
        }
    }))
    .unwrap();

    pods.create(&PostParams::default(), &pod).await.unwrap();
    services
        .create(&PostParams::default(), &service)
        .await
        .unwrap();
    wait_for(pods.clone(), &fixture, |p: Option<&Pod>| {
        p.and_then(|p| p.status.as_ref())
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
            })
    })
    .await;

    let image_url = format!("http://{fixture}.default.svc.cluster.local:8080/logo.svg");
    let spec = json!({
        "kanidmRef": {"name": "test-oauth2"},
        "displayname": "Image revalidation test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "image": {"url": image_url}
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(spec).unwrap());
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    let original_hash = format!("{:x}", Sha256::digest(IMAGE_V1.as_bytes()));
    wait_for(
        oauth2_api.clone(),
        name,
        |o: Option<&KanidmOAuth2Client>| has_synced_image(o, &original_hash),
    )
    .await;
    let first_gets = image_requests(&pods, &fixture).await;
    assert!(first_gets.1 >= 1, "initial image should be downloaded");

    // With unchanged remote content, a subsequent HEAD must not cause a GET.
    let no_change = wait_for_next_head(&pods, &fixture, first_gets.0).await;
    assert_eq!(
        no_change.1, first_gets.1,
        "unchanged image must not be downloaded again"
    );

    // Ensure Last-Modified changes at HTTP's one-second resolution.
    sleep(Duration::from_secs(2)).await;
    let update_script = format!("printf '%s' \"{IMAGE_V2}\" > /www/logo.svg");
    let attached = pods
        .exec(
            &fixture,
            vec!["sh", "-c", update_script.as_str()],
            &AttachParams::default().container("http"),
        )
        .await
        .unwrap();
    kaniop_k8s_util::client::get_output(attached).await.unwrap();

    // The spec is deliberately left untouched. This failed before #1134.
    let updated_hash = format!("{:x}", Sha256::digest(IMAGE_V2.as_bytes()));
    wait_for(
        oauth2_api.clone(),
        name,
        |o: Option<&KanidmOAuth2Client>| has_synced_image(o, &updated_hash),
    )
    .await;
    assert!(
        s.kanidm_client
            .idm_oauth2_rs_get(name)
            .await
            .unwrap()
            .unwrap()
            .attrs
            .contains_key("image"),
        "Kanidm must have the refreshed image"
    );

    let refreshed = image_requests(&pods, &fixture).await;
    assert_eq!(
        refreshed.1,
        first_gets.1 + 1,
        "content change should cause exactly one new download"
    );
    let stable = wait_for_next_head(&pods, &fixture, refreshed.0).await;
    assert_eq!(
        stable.1, refreshed.1,
        "a later HEAD must not cause another GET"
    );

    // Removing spec.image must delete both Kanidm's image and cached status.
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"spec": {"image": null}})),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        |o: Option<&KanidmOAuth2Client>| {
            o.and_then(|o| o.status.as_ref()).is_some_and(|status| {
                o.is_some_and(|o| o.spec.image.is_none())
                    && status.image.is_none()
                    && status.conditions.as_ref().is_some_and(|conditions| {
                        conditions
                            .iter()
                            .any(|c| c.type_ == "ImageUpdated" && c.status == "True")
                    })
            })
        },
    )
    .await;
    let attrs = s
        .kanidm_client
        .idm_oauth2_rs_get(name)
        .await
        .unwrap()
        .unwrap()
        .attrs;
    assert!(
        !attrs.contains_key("image"),
        "image removal must propagate to Kanidm"
    );

    services
        .delete(&fixture, &DeleteParams::default())
        .await
        .unwrap();
    pods.delete(&fixture, &DeleteParams::default())
        .await
        .unwrap();
});
