use crate::admission::{AdmissionResponse, AdmissionReview};
use crate::state::WebhookState;
use crate::validator::{HasKanidmRef, check_duplicate};

use axum::extract::State;
use axum::response::Json;
use kube::{Resource, ResourceExt};
use tracing::{debug, error};

/// Generic validation handler
pub async fn validate_resource<T>(
    _state: &WebhookState,
    store: &kube::runtime::reflector::Store<T>,
    review: AdmissionReview<T>,
    resource_name: &str,
) -> Json<AdmissionReview<()>>
where
    T: Resource + ResourceExt + Clone + HasKanidmRef,
    <T as Resource>::DynamicType: Eq + std::hash::Hash + Clone,
{
    let request = match review.request.as_ref() {
        Some(req) => req,
        None => {
            error!("Missing request in admission review");
            return Json(review.response(AdmissionResponse::deny(
                "unknown".to_string(),
                "Invalid admission review: missing request",
            )));
        }
    };

    let uid = request.uid.clone();
    let operation = &request.operation;

    // Only validate CREATE operations (kanidmRef is immutable)
    if operation != "CREATE" {
        debug!(
            "Skipping validation for {} operation on {}",
            operation, resource_name
        );
        return Json(review.response(AdmissionResponse::allow(uid)));
    }

    let object = match request.object.as_ref() {
        Some(obj) => obj,
        None => {
            error!("Missing object in CREATE request");
            return Json(review.response(AdmissionResponse::deny(
                uid,
                "Invalid admission review: missing object",
            )));
        }
    };

    // Check for duplicates
    match check_duplicate(object, resource_name, store) {
        Ok(_) => {
            debug!(
                "Validation passed for {} {}/{}",
                resource_name,
                object.namespace().unwrap_or_else(|| "default".to_string()),
                object.name_any()
            );
            Json(review.response(AdmissionResponse::allow(uid)))
        }
        Err(err) => {
            debug!(
                "Validation failed for {} {}/{}: {}",
                resource_name,
                object.namespace().unwrap_or_else(|| "default".to_string()),
                object.name_any(),
                err
            );
            Json(review.response(AdmissionResponse::deny(uid, err)))
        }
    }
}

// Concrete handlers for each resource type
pub async fn validate_kanidm_group(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_group::crd::KanidmGroup>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(&state, &state.group_store, review, "KanidmGroup").await
}

pub async fn validate_kanidm_person(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_person::crd::KanidmPersonAccount>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(&state, &state.person_store, review, "KanidmPersonAccount").await
}

pub async fn validate_kanidm_oauth2(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_oauth2::crd::KanidmOAuth2Client>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(&state, &state.oauth2_store, review, "KanidmOAuth2Client").await
}

pub async fn validate_kanidm_service_account(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_service_account::crd::KanidmServiceAccount>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(
        &state,
        &state.service_account_store,
        review,
        "KanidmServiceAccount",
    )
    .await
}
