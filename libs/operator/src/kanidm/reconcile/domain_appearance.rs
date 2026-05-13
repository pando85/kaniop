use super::super::controller::context::Context;
use crate::kanidm::crd::{DomainAppearanceImageStatus, Kanidm, KanidmStatus};
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::image::{ImageOperation, publish_image_error_event, update_image_if_needed};

use std::sync::Arc;

use kanidm_client::KanidmClient;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams};
use tracing::debug;

async fn clear_domain_appearance_image_status(
    kanidm_api: &Api<Kanidm>,
    name: &str,
    namespace: &str,
) -> Result<()> {
    let status_patch = serde_json::json!({
        "status": {
            "domainAppearanceImage": null
        }
    });
    kanidm_api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(&status_patch))
        .await
        .map_err(|e| {
            Error::KubeError(
                format!("failed to clear domain appearance image status for {namespace}/{name}"),
                Box::new(e),
            )
        })?;
    Ok(())
}

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
        if status.domain_appearance_image.is_some() {
            clear_domain_appearance_image_status(&kanidm_api, &name, &namespace).await?;
        }

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

            if status.domain_appearance_image.is_some() {
                let namespace = kanidm.namespace().unwrap();
                let name = kanidm.name_any();
                let kanidm_api =
                    Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
                clear_domain_appearance_image_status(&kanidm_api, &name, &namespace).await?;
            }

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
        }
        Some(image_spec) => {
            let url = &image_spec.url;
            let namespace = kanidm.namespace().unwrap();
            let name = kanidm.name_any();
            debug!(msg = format!("checking domain image from {}", url));

            let kanidm_client_clone = kanidm_client.clone();
            let namespace_for_error = namespace.clone();
            let name_for_error = name.clone();

            let update_result = update_image_if_needed(
                url,
                status.domain_appearance_image.as_ref(),
                |image_value| async {
                    kanidm_client_clone
                        .idm_domain_update_image(image_value)
                        .await
                        .map_err(|e| {
                            Error::KanidmClientError(
                                format!(
                                    "failed to update domain image for {namespace_for_error}/{name_for_error}",
                                ),
                                Box::new(e),
                            )
                        })
                },
            )
            .await;

            match update_result {
                Ok(Some(updated)) => {
                    let new_image_status = DomainAppearanceImageStatus {
                        url: updated.image_status.url,
                        etag: updated.image_status.etag,
                        last_modified: updated.image_status.last_modified,
                        content_length: updated.image_status.content_length,
                        content_hash: Some(updated.image_status.content_hash),
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
                Ok(None) => {}
                Err(e) => {
                    let operation = if matches!(e, Error::HttpError(_, _)) {
                        ImageOperation::Fetch
                    } else {
                        ImageOperation::Download
                    };
                    return Err(publish_image_error_event(
                        e,
                        operation,
                        &name,
                        &namespace,
                        &name,
                        &ctx.kaniop_ctx.recorder,
                        kanidm,
                    )
                    .await);
                }
            }
        }
    };

    Ok(())
}
