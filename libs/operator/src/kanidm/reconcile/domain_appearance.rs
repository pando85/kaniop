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
use tracing::{debug, trace};

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

/// Remove the domain image from Kanidm if the operator set one, as recorded in the status.
///
/// Skipping the delete when no image was set avoids an admin API call on every reconciliation.
/// The status is cleared only after a successful delete, so a failed delete is retried.
async fn remove_domain_image(
    kanidm: &Kanidm,
    kanidm_client: &KanidmClient,
    status: &KanidmStatus,
    ctx: &Context,
) -> Result<()> {
    if status.domain_appearance_image.is_none() {
        trace!("no domain image set by the operator, skipping delete");
        return Ok(());
    }

    debug!("removing domain image from Kanidm");
    delete_domain_image(kanidm_client, ctx).await?;

    let namespace = kanidm.namespace().unwrap();
    let name = kanidm.name_any();
    let kanidm_api = Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
    clear_domain_appearance_image_status(&kanidm_api, &name, &namespace).await
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
        remove_domain_image(kanidm, &kanidm_client, status, &ctx).await?;
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
            remove_domain_image(kanidm, &kanidm_client, status, &ctx).await?;
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

#[cfg(test)]
mod test {
    use super::reconcile_domain_appearance;

    use crate::kanidm::crd::{DomainAppearanceImageStatus, Kanidm, KanidmStatus};
    use crate::kanidm::reconcile::test::get_test_context;
    use kaniop_k8s_util::error::Result;

    use std::sync::Arc;
    use std::time::Duration;

    use futures::FutureExt;
    use http::{Method, Response};
    use kanidm_client::{KanidmClient, KanidmClientBuilder};
    use kube::client::Body;

    /// Client for a Kanidm refusing connections, so any Kanidm API call fails.
    fn unreachable_kanidm_client() -> Arc<KanidmClient> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        Arc::new(
            KanidmClientBuilder::new()
                .no_proxy()
                .address(format!("http://{address}"))
                .build()
                .unwrap(),
        )
    }

    /// Reconcile the domain appearance of a Kanidm without image in its spec. The mocked
    /// Kubernetes API answers only the initial read of the Kanidm; returns the reconcile result
    /// and whether any further Kubernetes API request (e.g. a status patch) was made.
    async fn reconcile_without_image_spec(status: KanidmStatus) -> (Result<()>, bool) {
        let (ctx, verifier) = get_test_context();
        let mut api_server = verifier.0;
        let kanidm = Kanidm::test();
        let kanidm_response = serde_json::to_vec(&kanidm).unwrap();

        let api_server = async move {
            let (request, send) = api_server
                .next_request()
                .await
                .expect("current Kanidm is read");
            assert_eq!(request.method(), Method::GET);
            send.send_response(
                Response::builder()
                    .body(Body::from(kanidm_response))
                    .unwrap(),
            );
            api_server
        };
        let (result, mut api_server) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                reconcile_domain_appearance(&kanidm, unreachable_kanidm_client(), &status, ctx),
                api_server
            )
        })
        .await
        .expect("domain appearance reconcile does not wait on further Kubernetes API requests");
        let more_requests = api_server.next_request().now_or_never().is_some();
        (result, more_requests)
    }

    #[tokio::test]
    async fn domain_image_is_not_deleted_when_none_was_set() {
        let (result, more_requests) = reconcile_without_image_spec(KanidmStatus::default()).await;

        result.expect("no Kanidm API call is made");
        assert!(!more_requests);
    }

    #[tokio::test]
    async fn domain_image_status_is_kept_when_delete_fails() {
        let status = KanidmStatus {
            domain_appearance_image: Some(DomainAppearanceImageStatus {
                url: "https://example.com/logo.svg".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let (result, more_requests) = reconcile_without_image_spec(status).await;

        assert!(result.is_err(), "a previously set image is deleted");
        assert!(
            !more_requests,
            "image status must be kept so the delete is retried"
        );
    }
}
