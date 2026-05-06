use super::super::controller::context::Context;
use crate::kanidm::crd::{DomainAppearanceImageStatus, Kanidm, KanidmStatus};
use crate::kanidm::image::headers_changed;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::image::{download_image, fetch_headers};

use std::sync::Arc;

use kanidm_client::KanidmClient;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::events::{Event, EventType};
use kube::{Resource, ResourceExt};
use tracing::{debug, info, warn};

pub async fn reconcile_domain_appearance(
    kanidm: &Kanidm,
    kanidm_client: Arc<KanidmClient>,
    status: &KanidmStatus,
    ctx: Arc<Context>,
) -> Result<()> {
    let namespace = kanidm.namespace().unwrap();
    let name = kanidm.name_any();
    let kanidm_api = Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);

    let current_kanidm = kanidm_api.get(&name).await.map_err(|e| {
        Error::KubeError(
            format!("failed to get current Kanidm {namespace}/{name}"),
            Box::new(e),
        )
    })?;

    let domain_appearance = current_kanidm.spec.domain_appearance.as_ref();
    let image_spec = domain_appearance.and_then(|da| da.image.as_ref());

    if let Some(domain_appearance) = domain_appearance {
        reconcile_domain_display_name(
            kanidm,
            kanidm_client.clone(),
            domain_appearance.display_name.as_deref(),
        )
        .await?;

        reconcile_domain_image_with_spec(kanidm, kanidm_client, status, ctx, image_spec).await?;
    } else {
        debug!(msg = "removing domain image from Kanidm");
        kanidm_client.idm_domain_delete_image().await.map_err(|e| {
            Error::KanidmClientError(
                format!(
                    "failed to delete domain image from {namespace}/{name}",
                    namespace = kanidm.namespace().unwrap(),
                    name = kanidm.name_any()
                ),
                Box::new(e),
            )
        })?;

        if status.domain_appearance_image.is_some() {
            let status_patch = serde_json::json!({
                "status": {
                    "domainAppearanceImage": null
                }
            });
            kanidm_api
                .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status_patch))
                .await
                .map_err(|e| {
                    Error::KubeError(
                        format!(
                            "failed to clear domain appearance image status for {namespace}/{name}"
                        ),
                        Box::new(e),
                    )
                })?;
        }
    }

    Ok(())
}

async fn reconcile_domain_display_name(
    kanidm: &Kanidm,
    kanidm_client: Arc<KanidmClient>,
    display_name: Option<&str>,
) -> Result<()> {
    if let Some(name) = display_name {
        debug!(msg = format!("setting domain display name to '{}'", name));
        kanidm_client
            .idm_domain_set_display_name(name)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to set domain display name for {namespace}/{name}",
                        namespace = kanidm.namespace().unwrap(),
                        name = kanidm.name_any()
                    ),
                    Box::new(e),
                )
            })?;
    }
    Ok(())
}

async fn reconcile_domain_image_with_spec(
    kanidm: &Kanidm,
    kanidm_client: Arc<KanidmClient>,
    status: &KanidmStatus,
    ctx: Arc<Context>,
    image_spec: Option<&crate::kanidm::crd::DomainAppearanceImageSpec>,
) -> Result<()> {
    match image_spec {
        None => {
            debug!(msg = "deleting domain image from Kanidm");
            kanidm_client.idm_domain_delete_image().await.map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to delete domain image for {namespace}/{name}",
                        namespace = kanidm.namespace().unwrap(),
                        name = kanidm.name_any()
                    ),
                    Box::new(e),
                )
            })?;

            if status.domain_appearance_image.is_some() {
                let namespace = kanidm.namespace().unwrap();
                let name = kanidm.name_any();
                let kanidm_api =
                    Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
                let status_patch = serde_json::json!({
                    "status": {
                        "domainAppearanceImage": null
                    }
                });
                kanidm_api
                    .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status_patch))
                    .await
                    .map_err(|e| {
                        Error::KubeError(
                            format!(
                                "failed to clear domain appearance image status for {namespace}/{name}"
                            ),
                            Box::new(e),
                        )
                    })?;
            }
        }
        Some(image_spec) => {
            let url = &image_spec.url;
            debug!(msg = format!("checking domain image from {}", url));

            let current_headers = match fetch_headers(url).await {
                Ok(h) => h,
                Err(e) => {
                    let msg = format!(
                        "failed to fetch image headers for {namespace}/{name}: {e:?}",
                        namespace = kanidm.namespace().unwrap(),
                        name = kanidm.name_any()
                    );
                    let _ = ctx
                        .kaniop_ctx
                        .recorder
                        .publish(
                            &Event {
                                type_: EventType::Warning,
                                reason: "DomainImageFetchError".to_string(),
                                note: Some(msg.clone()),
                                action: "DomainImageUpdate".to_string(),
                                secondary: None,
                            },
                            &kanidm.object_ref(&()),
                        )
                        .await
                        .map_err(|e| {
                            warn!(msg = "failed to publish DomainImageFetchError event", %e);
                        });
                    return Err(e);
                }
            };

            let should_download = match &status.domain_appearance_image {
                None => true,
                Some(cached) => cached.url != *url || headers_changed(&current_headers, cached),
            };

            if should_download {
                info!(msg = format!("downloading domain image from {}", url));
                let downloaded = match download_image(url).await {
                    Ok(d) => d,
                    Err(e) => {
                        let msg = format!(
                            "failed to download domain image for {namespace}/{name}: {e:?}",
                            namespace = kanidm.namespace().unwrap(),
                            name = kanidm.name_any()
                        );
                        let _ = ctx
                            .kaniop_ctx
                            .recorder
                            .publish(
                                &Event {
                                    type_: EventType::Warning,
                                    reason: "DomainImageDownloadError".to_string(),
                                    note: Some(msg.clone()),
                                    action: "DomainImageUpdate".to_string(),
                                    secondary: None,
                                },
                                &kanidm.object_ref(&()),
                            )
                            .await
                            .map_err(|e| {
                                warn!(msg = "failed to publish DomainImageDownloadError event", %e);
                            });
                        return Err(e);
                    }
                };

                kanidm_client
                    .idm_domain_update_image(downloaded.image_value)
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to update domain image for {namespace}/{name}",
                                namespace = kanidm.namespace().unwrap(),
                                name = kanidm.name_any()
                            ),
                            Box::new(e),
                        )
                    })?;

                let new_image_status = DomainAppearanceImageStatus {
                    url: url.clone(),
                    etag: downloaded.headers.etag,
                    last_modified: downloaded.headers.last_modified,
                    content_length: downloaded.headers.content_length,
                    content_hash: Some(downloaded.content_hash),
                };

                let namespace = kanidm.namespace().unwrap();
                let name = kanidm.name_any();
                let kanidm_api =
                    Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
                let status_patch = serde_json::json!({
                    "status": {
                        "domainAppearanceImage": new_image_status
                    }
                });
                kanidm_api
                    .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status_patch))
                    .await
                    .map_err(|e| {
                        Error::KubeError(
                            format!("failed to patch Kanidm/status {namespace}/{name}"),
                            Box::new(e),
                        )
                    })?;
            }
        }
    };

    Ok(())
}
