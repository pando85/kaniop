use super::super::controller::context::Context;
use crate::kanidm::crd::{DomainAppearanceImageStatus, Kanidm, KanidmStatus};
use crate::metrics::{
    KANIDM_OP_DELETE, KANIDM_OP_IMAGE, KANIDM_OP_SET_DISPLAY_NAME, KANIDM_OUTCOME_CHANGED,
    KANIDM_OUTCOME_ERROR, KANIDM_OUTCOME_UNCHANGED, KANIDM_RESOURCE_DOMAIN,
};
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::image::{ImageOperation, publish_image_error_event, update_image_if_needed};

use std::sync::Arc;

use kanidm_client::{ClientError, KanidmClient, StatusCode};
use kanidm_proto::internal::OperationError;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams};
use tracing::debug;

async fn delete_domain_image(kanidm_client: &KanidmClient, ctx: &Context) -> Result<()> {
    let start = tokio::time::Instant::now();
    let result = kanidm_client.idm_domain_delete_image().await;
    match result {
        Ok(()) => {
            ctx.kaniop_ctx.metrics.record_kanidm_sdk_outcome(
                KANIDM_RESOURCE_DOMAIN,
                KANIDM_OP_DELETE,
                KANIDM_OUTCOME_CHANGED,
                start.elapsed(),
            );
            Ok(())
        }
        Err(ClientError::Http(
            StatusCode::NOT_FOUND,
            Some(OperationError::NoMatchingEntries),
            _,
        )) => {
            debug!("domain image already absent, skipping delete");
            ctx.kaniop_ctx.metrics.record_kanidm_sdk_outcome(
                KANIDM_RESOURCE_DOMAIN,
                KANIDM_OP_DELETE,
                KANIDM_OUTCOME_UNCHANGED,
                start.elapsed(),
            );
            Ok(())
        }
        Err(e) => {
            ctx.kaniop_ctx.metrics.record_kanidm_sdk_outcome(
                KANIDM_RESOURCE_DOMAIN,
                KANIDM_OP_DELETE,
                KANIDM_OUTCOME_ERROR,
                start.elapsed(),
            );
            Err(Error::KanidmClientError(
                "failed to delete domain image".to_string(),
                Box::new(e),
            ))
        }
    }
}

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
            &ctx,
        )
        .await?;

        reconcile_domain_image_with_spec(kanidm, kanidm_client, status, ctx.clone(), image_spec)
            .await?;
    } else {
        if status.domain_appearance_image.is_some() {
            clear_domain_appearance_image_status(&kanidm_api, &name, &namespace).await?;
        }

        debug!("removing domain image from Kanidm");
        delete_domain_image(&kanidm_client, &ctx).await?;
    }

    Ok(())
}

async fn reconcile_domain_display_name(
    kanidm: &Kanidm,
    kanidm_client: Arc<KanidmClient>,
    display_name: Option<&str>,
    ctx: &Context,
) -> Result<()> {
    if let Some(name) = display_name {
        debug!("setting domain display name to '{}'", name);
        let start = tokio::time::Instant::now();
        let result = kanidm_client.idm_domain_set_display_name(name).await;
        match result {
            Ok(()) => {
                ctx.kaniop_ctx.metrics.record_kanidm_sdk_outcome(
                    KANIDM_RESOURCE_DOMAIN,
                    KANIDM_OP_SET_DISPLAY_NAME,
                    KANIDM_OUTCOME_CHANGED,
                    start.elapsed(),
                );
            }
            Err(ClientError::Http(
                StatusCode::NOT_FOUND,
                Some(OperationError::NoMatchingEntries),
                _,
            )) => {
                debug!("domain display name target is absent, skipping update");
                ctx.kaniop_ctx.metrics.record_kanidm_sdk_outcome(
                    KANIDM_RESOURCE_DOMAIN,
                    KANIDM_OP_SET_DISPLAY_NAME,
                    KANIDM_OUTCOME_UNCHANGED,
                    start.elapsed(),
                );
            }
            Err(e) => {
                ctx.kaniop_ctx.metrics.record_kanidm_sdk_outcome(
                    KANIDM_RESOURCE_DOMAIN,
                    KANIDM_OP_SET_DISPLAY_NAME,
                    KANIDM_OUTCOME_ERROR,
                    start.elapsed(),
                );
                return Err(Error::KanidmClientError(
                    format!(
                        "failed to set domain display name for {namespace}/{name}",
                        namespace = kanidm.namespace().unwrap(),
                        name = kanidm.name_any()
                    ),
                    Box::new(e),
                ));
            }
        }
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
            debug!("deleting domain image from Kanidm");

            if status.domain_appearance_image.is_some() {
                let namespace = kanidm.namespace().unwrap();
                let name = kanidm.name_any();
                let kanidm_api =
                    Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
                clear_domain_appearance_image_status(&kanidm_api, &name, &namespace).await?;
            }

            delete_domain_image(&kanidm_client, &ctx).await?;
        }
        Some(image_spec) => {
            let url = &image_spec.url;
            let namespace = kanidm.namespace().unwrap();
            let name = kanidm.name_any();
            debug!("checking domain image from {}", url);

            let kanidm_client_clone = kanidm_client.clone();
            let namespace_for_error = namespace.clone();
            let name_for_error = name.clone();
            let metrics = ctx.kaniop_ctx.metrics.clone();

            let update_result = update_image_if_needed(
                url,
                status.domain_appearance_image.as_ref(),
                |image_value| {
                    let metrics = metrics.clone();
                    async move {
                        let start = tokio::time::Instant::now();
                        let result = kanidm_client_clone
                            .idm_domain_update_image(image_value)
                            .await;
                        match &result {
                            Ok(_) => {
                                metrics.record_kanidm_sdk_outcome(
                                    KANIDM_RESOURCE_DOMAIN,
                                    KANIDM_OP_IMAGE,
                                    KANIDM_OUTCOME_CHANGED,
                                    start.elapsed(),
                                );
                            }
                            Err(_) => {
                                metrics.record_kanidm_sdk_outcome(
                                    KANIDM_RESOURCE_DOMAIN,
                                    KANIDM_OP_IMAGE,
                                    KANIDM_OUTCOME_ERROR,
                                    start.elapsed(),
                                );
                            }
                        }
                        result.map_err(|e| {
                            Error::KanidmClientError(
                                format!(
                                    "failed to update domain image for {namespace_for_error}/{name_for_error}",
                                ),
                                Box::new(e),
                            )
                        })
                    }
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
