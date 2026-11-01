//! World-derived reconciliation of a `KanidmRadiusServer`.
//!
//! Every pass reads Kanidm and Kubernetes and lets the pure token planner
//! decide; CR status is written as a report and never read back for a
//! decision. Order (plan, "Reconcile order"):
//!
//! 1. Restore guard, referenced Kanidm, namespace authorization (an
//!    unauthorized CR gets a `ResourceNotWatched` event and no finalizer).
//! 2. Finalizer, then read-only input validation before any mutation: an
//!    invalid input leaves the running Deployment and token untouched.
//! 3. Ensure the UID-bound Kanidm account and its membership edge.
//! 4. Observe tokens, config Secrets, the Deployment template, ReplicaSets
//!    and pods; plan.
//! 5. Mint and verify a candidate with `whoami` only if the plan says so.
//! 6. Create the immutable config Secret, apply the Deployment and Service.
//! 7. Re-observe (if anything was written), revoke and garbage-collect.
//! 8. Write status, on every path; transient errors then drive backoff.

/// Ownership-checked server-side apply, create-only Secret and delete.
pub mod apply;
/// Finalizer steps.
pub mod cleanup;
/// Validated config rendering for the upstream Kanidm RADIUS service.
pub mod config;
/// Typed errors of the executors and of a reconcile pass.
pub mod error;
/// Kanidm account naming, ownership-checked ensure and delete.
pub mod identity;
/// Image inference and the verified runtime identity of the RADIUS image.
pub mod image;
/// Fail-closed input validation and public-material digest.
pub mod inputs;
/// Kanidm token mint, verify and revoke executors.
pub mod kanidm_token;
/// One module for every label, annotation and Secret key the operator uses.
pub mod meta;
/// Builds the planner input from Kanidm and Kubernetes lists.
pub mod observe;
/// Kanidm SDK call metrics wrapper.
mod sdk;
/// Conditions and status, applied with server-side apply.
pub mod status;
/// EAP TLS certificate and private key validation.
pub mod tls;
/// Pure token planner: which token to run, revoke and garbage-collect.
pub mod token;
/// Deployment, Service and content-addressed config Secret builders.
pub mod workload;

/// Fake Kanidm HTTP server for executor tests.
#[cfg(test)]
pub(crate) mod kanidm_mock;
/// Mock Kubernetes API server for executor tests.
#[cfg(test)]
pub(crate) mod kube_mock;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Secret, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{OwnerReference, Time};
use k8s_openapi::jiff::Timestamp;
use kanidm_client::KanidmClient;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_operator::controller::context::IdmClientContext;
use kaniop_operator::controller::idm_reconcile_interval;
use kaniop_operator::controller::kanidm::{KanidmResource, is_resource_watched};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::metrics::ControllerMetrics;
use kaniop_operator::telemetry;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType};
use kube::runtime::reflector::ObjectRef;
use kube::{Client, Resource, ResourceExt};
use serde_json::json;
use time::OffsetDateTime;
use tracing::{Span, debug, field, info, instrument, warn};
use uuid::Uuid;

use crate::controller::Context;
use crate::crd::{KanidmRadiusServer, KanidmRadiusServerStatus};
use crate::reconcile::apply::{
    DeleteMode, DeleteOutcome, SecretOutcome, apply_owned, delete_owned, ensure_config_secret,
    is_controlled_by,
};
use crate::reconcile::cleanup::{
    CleanupStep, CleanupView, DRAIN_WARNING_REASON, DeploymentPhase, Finish, SkipReason,
    drain_note, identity_cleanup, next_step,
};
use crate::reconcile::error::{ExecError, Failure, RadiusBlock};
use crate::reconcile::identity::{RADIUS_GROUP, RadiusIdentity, delete_identity, ensure_identity};
use crate::reconcile::image::resolve_radius_image;
use crate::reconcile::inputs::{RadiusPreflightError, prepare_radius_inputs};
use crate::reconcile::kanidm_token::{
    RevokeOutcome, VerifyRetry, mint_token, revoke_token, verify_token,
};
use crate::reconcile::meta::COMPONENT;
use crate::reconcile::observe::{observe, observe_drain};
use crate::reconcile::status::{
    ConditionKind, StatusReport, TokenFacts, Truth, Verdict, available_verdict, block_verdict,
    build_status, kube_time, patch_status,
};
use crate::reconcile::token::{
    Consumer, CurrentToken, MintReason, ObservedSecret, TokenPlan, TokenPolicy, TokenWorld,
    expires_within_warning, plan_tokens,
};
use crate::reconcile::workload::{RadiusServiceExposure, build_config_secret, build_workload};

pub const RADIUS_FINALIZER: &str = "kanidmradiusservers.kaniop.rs/finalizer";
/// Requeue while a Kanidm restore pauses identity writes.
const RESTORE_REQUEUE: Duration = Duration::from_secs(5);
/// Requeue while a rollout, drain, revocation or garbage collection is
/// pending (pods terminating are not watched).
const CONVERGING_REQUEUE: Duration = Duration::from_secs(15);
/// Requeue while the finalizer waits for the workload to drain.
const DRAIN_REQUEUE: Duration = Duration::from_secs(5);
/// Event reason when a blocked pass leaves the running token near expiry.
const TOKEN_NEAR_EXPIRY_REASON: &str = "TokenNearExpiry";

/// The live objects a pass reports on and whether it changed anything.
#[derive(Debug, Default)]
struct PassState {
    deployment: Option<Deployment>,
    service: Option<Service>,
    changed: bool,
    /// The pass got past the Deployment apply, so the planner's mint or keep
    /// reached the pods: a later block does not stop rotation.
    rotation_applied: bool,
}

/// Kanidm URL the RADIUS runtime uses: the Kanidm origin, else its domain.
/// There is no RADIUS spec field for it (plan D5).
pub fn kanidm_uri(kanidm: &Kanidm) -> String {
    kanidm
        .spec
        .origin
        .clone()
        .unwrap_or_else(|| format!("https://{}", kanidm.spec.domain))
}

fn has_finalizer(radius: &KanidmRadiusServer) -> bool {
    radius.finalizers().iter().any(|f| f == RADIUS_FINALIZER)
}

#[instrument(skip(radius, ctx), fields(trace_id))]
pub async fn reconcile_radius(
    radius: Arc<KanidmRadiusServer>,
    ctx: Arc<Context>,
) -> Result<(Action, bool)> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx
        .kaniop_ctx
        .metrics
        .reconcile_count_and_measure(&trace_id);

    if !ctx.kaniop_ctx.kanidm_write_allowed(&radius) {
        debug!("Kanidm restore in progress, pausing RADIUS reconciliation");
        return Ok((Action::requeue(RESTORE_REQUEUE), false));
    }
    if radius.metadata.deletion_timestamp.is_some() {
        if !has_finalizer(&radius) {
            return Ok((Action::await_change(), false));
        }
        return finalize(&radius, &ctx).await;
    }

    let Some(kanidm) = ctx.kaniop_ctx.get_kanidm(&radius) else {
        let block = RadiusBlock::KanidmNotFound {
            kanidm: radius.kanidm_ref(),
        };
        return report_block(&radius, &ctx, block).await;
    };
    if !is_resource_watched(
        radius.as_ref(),
        &kanidm,
        &ctx.kaniop_ctx.namespace_store,
        &ctx.kaniop_ctx.client,
    )
    .await
    {
        publish_warning(
            &radius,
            &ctx,
            "ResourceNotWatched",
            "configure `radiusServerNamespaceSelector` on the Kanidm resource to watch this namespace",
        )
        .await?;
        let block = RadiusBlock::NamespaceNotWatched {
            kanidm: radius.kanidm_ref(),
        };
        return report_block(&radius, &ctx, block).await;
    }
    if !has_finalizer(&radius) {
        add_finalizer(&radius, ctx.kaniop_ctx.client.clone()).await?;
    }

    info!("reconciling RADIUS server");
    let mut state = initial_state(&radius, &ctx);
    let mut status_report = StatusReport::default();
    let idm_ctx = ctx.clone();
    let idm_radius = radius.clone();
    let result = run_pass(
        &radius,
        &kanidm,
        ctx.kaniop_ctx.client.clone(),
        &ctx.kaniop_ctx.metrics,
        move || async move { idm_ctx.get_idm_client(&idm_radius).await },
        &mut status_report,
        &mut state,
    )
    .await;
    report_pass(&radius, &ctx, state, status_report, result).await
}

/// Deployment and Service from the cache, for a pass that stops before it
/// reads or writes them. Objects we do not control are ignored.
fn initial_state(radius: &KanidmRadiusServer, ctx: &Context) -> PassState {
    let Some(owner) = radius.controller_owner_ref(&()) else {
        return PassState::default();
    };
    let namespace = radius.namespace().unwrap_or_default();
    let name = radius.name_any();
    let deployment = ctx
        .deployment_store
        .get(&ObjectRef::new(&name).within(&namespace))
        .filter(|d| is_controlled_by(&d.metadata, &owner))
        .map(|d| d.as_ref().clone());
    let service = ctx
        .service_store
        .get(&ObjectRef::new(&name).within(&namespace))
        .filter(|s| is_controlled_by(&s.metadata, &owner))
        .map(|s| s.as_ref().clone());
    PassState {
        deployment,
        service,
        ..Default::default()
    }
}

/// Write status for a pass blocked before it started.
async fn report_block(
    radius: &KanidmRadiusServer,
    ctx: &Context,
    block: RadiusBlock,
) -> Result<(Action, bool)> {
    let state = initial_state(radius, ctx);
    report_pass(
        radius,
        ctx,
        state,
        StatusReport::default(),
        Err(block.into()),
    )
    .await
}

/// Write the status first, then return the requeue or the transient error.
async fn report_pass(
    radius: &KanidmRadiusServer,
    ctx: &Context,
    state: PassState,
    mut status_report: StatusReport,
    result: std::result::Result<Duration, Failure>,
) -> Result<(Action, bool)> {
    if let Err(failure) = &result {
        status_report.record_failure(failure);
    }
    let status = build_status(
        radius.metadata.generation,
        radius.status.as_ref(),
        &status_report,
        state.deployment.as_ref(),
        state.service.as_ref(),
        &Time(Timestamp::now()),
    );
    let written = patch_status(ctx.kaniop_ctx.client.clone(), radius, status).await;
    if let Err(error) = &written {
        ctx.kaniop_ctx.metrics.status_update_errors_inc();
        warn!(%error, "failed to write RADIUS status");
    }
    match result {
        Ok(requeue) => {
            written?;
            Ok((Action::requeue(requeue), state.changed))
        }
        Err(Failure::Block(block)) => {
            written?;
            debug!(reason = block.reason(), %block, "RADIUS reconciliation blocked");
            // Only a block that stopped the pass before the token reached
            // the pods stops rotation; with no Deployment there are no pods.
            if state.deployment.is_some() && !state.rotation_applied {
                warn_token_near_expiry(radius, ctx, &block, &status_report).await;
            }
            let requeue = block.retry_after().unwrap_or_else(idm_reconcile_interval);
            Ok((Action::requeue(requeue), state.changed))
        }
        // The source error wins over a status write failure.
        Err(Failure::Transient { error, .. }) => Err(error),
    }
}

/// Warn when a blocked pass leaves the running token close to its expiry.
/// Best effort: a failed event does not change the pass result.
async fn warn_token_near_expiry(
    radius: &KanidmRadiusServer,
    ctx: &Context,
    block: &RadiusBlock,
    report: &StatusReport,
) {
    let policy = TokenPolicy::from_spec(Some(&radius.spec.token_rotation));
    let running = running_token_expiry(report, radius.status.as_ref(), &policy);
    let Some(note) = token_near_expiry_note(block, running, OffsetDateTime::now_utc()) else {
        return;
    };
    warn!(
        reason = block.reason(),
        "RADIUS token near expiry while blocked"
    );
    if let Err(error) = publish_warning(radius, ctx, TOKEN_NEAR_EXPIRY_REASON, &note).await {
        warn!(%error, "failed to publish RADIUS token expiry warning");
    }
}

/// One reconcile pass after authorization and finalizer. Returns the
/// requeue delay. `report` and `state` carry what the pass established even
/// when it fails, so the caller can write status first.
async fn run_pass<F, Fut>(
    radius: &KanidmRadiusServer,
    kanidm: &Kanidm,
    kube: Client,
    metrics: &ControllerMetrics,
    idm_client: F,
    report: &mut StatusReport,
    state: &mut PassState,
) -> std::result::Result<Duration, Failure>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<KanidmClient>>>,
{
    // Read-only validation of every input before any mutation.
    let inputs = prepare_radius_inputs(kube.clone(), radius, &kanidm_uri(kanidm))
        .await
        .map_err(Failure::inputs)?;
    // An unresolved image blocks only a server that is not running yet. A
    // running one keeps its current image so token rotation goes on, with
    // the block still reported on `ConfigurationReady`.
    let (image, image_block) =
        match resolve_radius_image(&kanidm.spec.image, radius.spec.image.as_deref()) {
            Ok(image) => (image, None),
            Err(error) => match state.deployment.as_ref().and_then(deployed_image) {
                Some(image) => {
                    debug!(%error, %image, "RADIUS image unresolved, keeping the running image");
                    (image, Some(RadiusBlock::from(error)))
                }
                None => return Err(RadiusBlock::from(error).into()),
            },
        };
    RadiusServiceExposure::from_spec(
        &radius.spec.service.type_,
        radius.spec.service.external_traffic_policy.clone(),
    )
    .map_err(RadiusBlock::from)?;
    let identity = RadiusIdentity::for_resource(radius).map_err(RadiusBlock::from)?;
    let namespace = radius
        .namespace()
        .ok_or(RadiusBlock::Inputs(RadiusPreflightError::MissingNamespace))?;
    let owner = radius.controller_owner_ref(&()).ok_or(RadiusBlock::Exec {
        error: ExecError::MissingMetadata {
            kind: "KanidmRadiusServer",
            field: "uid",
        },
        stage: ConditionKind::ConfigurationReady,
    })?;
    report.configuration = match &image_block {
        Some(block) => block_verdict(block),
        None => Verdict::yes(
            "ConfigurationValid",
            "referenced Secrets, CA and TLS material are valid",
        ),
    };

    // Identity: UID-bound account, additive membership, never adopted.
    let idm = idm_client()
        .await
        .map_err(Failure::transient(ConditionKind::IdentityReady))?;
    let outcome = ensure_identity(&idm, metrics, &identity)
        .await
        .map_err(Failure::exec(ConditionKind::IdentityReady))?;
    state.changed |= outcome.account_created || outcome.membership_added;
    report.identity = Verdict::yes(
        "AccountReady",
        format!(
            "Kanidm service account {} is a member of {RADIUS_GROUP}",
            identity.account_name
        ),
    );

    // Observe and plan.
    let policy = TokenPolicy::from_spec(Some(&radius.spec.token_rotation));
    let observation = observe(&idm, metrics, kube.clone(), radius, &identity)
        .await
        .map_err(Failure::exec(ConditionKind::Rotating))?;
    state.deployment = observation.deployment.clone();
    let plan = plan_tokens(&observation.world, &policy, OffsetDateTime::now_utc());
    debug!(?plan, "token plan");

    let account = identity.account_name.as_str();
    let (token_id, bearer, rotation_request, minted) = match plan.current {
        CurrentToken::Keep(id) => {
            let bearer = observation
                .template_bearer
                .clone()
                .ok_or(RadiusBlock::BearerUnreadable)?;
            // Carry the recorded request forward: a config-only change must
            // not turn a past rotation request into a new one.
            let request = template_secret(&observation.world)
                .and_then(|secret| secret.rotation_request.clone());
            (id, bearer, request, None)
        }
        CurrentToken::Mint(reason) => {
            let expiry = policy.new_token_expiry(OffsetDateTime::now_utc());
            let candidate = mint_token(&idm, metrics, account, expiry)
                .await
                .map_err(Failure::exec(ConditionKind::Rotating))?;
            state.changed = true;
            info!(token_id = %candidate.token_id, reason = reason.reason(), "minted RADIUS token");
            // Promotion gate: a candidate Kanidm does not accept never reaches
            // a Secret or a pod; it is revoked as an orphan after the grace.
            verify_token(
                &idm,
                metrics,
                account,
                &candidate.bearer,
                VerifyRetry::default(),
            )
            .await
            .map_err(Failure::exec(ConditionKind::Rotating))?;
            let request = observation
                .world
                .rotate_request
                .clone()
                .filter(|value| !value.is_empty());
            (candidate.token_id, candidate.bearer, request, Some(reason))
        }
        CurrentToken::Blocked(_) => {
            // Nothing was written since the observation, so its plan may
            // revoke and collect; that is what clears the block.
            report.tokens = Some(token_facts(&observation.world, &plan));
            collect(
                &idm, metrics, kube, &namespace, &owner, account, &plan, state,
            )
            .await?;
            return Err(RadiusBlock::TooManyTokens.into());
        }
    };

    // Immutable, content-addressed config Secret.
    let config = inputs
        .config_with_token(bearer.expose().to_string())
        .map_err(|error| RadiusBlock::Inputs(error.into()))?;
    let secret = build_config_secret(
        radius,
        &token_id.to_string(),
        rotation_request.as_deref(),
        &config,
    )
    .map_err(RadiusBlock::from)?;
    let secret_name = secret.name_any();
    let secrets: Api<Secret> = Api::namespaced(kube.clone(), &namespace);
    let secret_created = ensure_config_secret(&secrets, &secret, &owner)
        .await
        .map_err(Failure::exec(ConditionKind::ConfigurationReady))?
        == SecretOutcome::Created;
    if secret_created {
        info!(secret = %secret_name, "created RADIUS config Secret");
        state.changed = true;
    }

    // Deployment and Service.
    let workload = build_workload(
        radius,
        image,
        &secret_name,
        &token_id.to_string(),
        inputs.inputs_digest(),
    )
    .map_err(RadiusBlock::from)?;
    let deployments: Api<Deployment> = Api::namespaced(kube.clone(), &namespace);
    let desired = workload.deployment().map_err(RadiusBlock::from)?;
    let before = observation
        .deployment
        .as_ref()
        .and_then(|d| d.metadata.resource_version.clone());
    let applied = apply_owned(&deployments, &desired, &owner)
        .await
        .map_err(Failure::exec(ConditionKind::Available))?;
    let template_changed = applied.metadata.resource_version != before;
    state.changed |= template_changed;
    state.deployment = Some(applied);
    state.rotation_applied = true;
    // The Service feeds neither the planner nor the pods' token: a failure
    // to apply it is recorded now and returned only after revoke and garbage
    // collection, so a persistent one (a foreign Service) cannot hold retired
    // tokens forever.
    let services: Api<Service> = Api::namespaced(kube.clone(), &namespace);
    let service_before = state
        .service
        .as_ref()
        .and_then(|s| s.metadata.resource_version.clone());
    let exposed = match workload.service() {
        Ok(desired) => apply_owned(&services, &desired, &owner)
            .await
            .map_err(Failure::exec(ConditionKind::Exposed)),
        Err(error) => Err(RadiusBlock::from(error).into()),
    };
    let exposed = match exposed {
        Ok(applied) => {
            state.changed |= applied.metadata.resource_version != service_before;
            state.service = Some(applied);
            Ok(())
        }
        Err(failure) => {
            report.record_failure(&failure);
            Err(failure)
        }
    };

    // Revoke and collect only from an observation made after the writes. If
    // nothing that feeds the planner was written, the first one still holds.
    let wrote = minted.is_some() || secret_created || template_changed;
    let (world, plan) = if wrote {
        let fresh = observe(&idm, metrics, kube.clone(), radius, &identity)
            .await
            .map_err(Failure::exec(ConditionKind::Rotating))?;
        let plan = plan_tokens(&fresh.world, &policy, OffsetDateTime::now_utc());
        (fresh.world, plan)
    } else {
        (observation.world, plan)
    };
    report.tokens = Some(token_facts(&world, &plan));
    report.rotating = rotating_verdict(&world, &plan, minted);
    collect(
        &idm, metrics, kube, &namespace, &owner, account, &plan, state,
    )
    .await?;
    exposed?;

    let rollout_done = available_verdict(state.deployment.as_ref()).truth == Truth::True;
    let converging = !plan.retiring.is_empty()
        || !plan.unresolved_consumers.is_empty()
        || world.secrets.len() > plan.gc_secrets.len() + 1
        || !rollout_done;
    Ok(if converging {
        CONVERGING_REQUEUE
    } else {
        idm_reconcile_interval()
    })
}

/// The token the pods run and its expiry, for a blocked pass: as observed in
/// this pass, else estimated from the last reported issue time and the
/// current rotation policy.
///
/// Status records no expiry, so the estimate misjudges a token minted under
/// another rotation policy; it is used for a warning only, never a decision.
fn running_token_expiry(
    report: &StatusReport,
    previous: Option<&KanidmRadiusServerStatus>,
    policy: &TokenPolicy,
) -> Option<(String, OffsetDateTime)> {
    if let Some(facts) = &report.tokens {
        let (id, _) = facts.current?;
        return Some((id.to_string(), facts.current_expiry?));
    }
    let previous = previous?;
    let issued = previous.token_issued_at.as_ref()?;
    let issued = OffsetDateTime::from_unix_timestamp_nanos(issued.0.as_nanosecond()).ok()?;
    Some((previous.token_id.clone()?, policy.new_token_expiry(issued)?))
}

/// Warning note when the running token expires within twice the safety
/// margin while `block` stops rotation. Stable for an unchanged block and
/// token, so the event recorder folds repeats into one event series.
fn token_near_expiry_note(
    block: &RadiusBlock,
    running: Option<(String, OffsetDateTime)>,
    now: OffsetDateTime,
) -> Option<String> {
    let (id, expiry) = running?;
    if !expires_within_warning(expiry, now) {
        return None;
    }
    let at = kube_time(expiry).map_or_else(|| expiry.to_string(), |t| t.0.to_string());
    Some(format!(
        "token {id} used by the RADIUS pods expires at {at} and cannot be rotated while \
         reconciliation is blocked ({}); clear the block before the token lapses",
        block.reason()
    ))
}

/// The image of the RADIUS container in a Deployment's pod template.
fn deployed_image(deployment: &Deployment) -> Option<String> {
    deployment
        .spec
        .as_ref()?
        .template
        .spec
        .as_ref()?
        .containers
        .iter()
        .find(|c| c.name == COMPONENT)?
        .image
        .clone()
        .filter(|image| !image.trim().is_empty())
}

/// The config Secret the template mounts, as observed.
fn template_secret(world: &TokenWorld) -> Option<&ObservedSecret> {
    let name = world.template.as_ref()?.config_secret.as_deref()?;
    world.secrets.iter().find(|s| s.name == name)
}

/// Token facts for status: the token the template's config Secret was
/// rendered with (what pods run), and the other tokens still held.
fn token_facts(world: &TokenWorld, plan: &TokenPlan) -> TokenFacts {
    let secret = template_secret(world);
    let current = secret
        .and_then(|s| s.token_id.as_deref())
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .and_then(|id| world.tokens.iter().find(|t| t.id == id));
    let current_id = current.map(|t| t.id);
    TokenFacts {
        current: current.map(|t| (t.id, t.issued_at)),
        current_expiry: current.and_then(|t| t.expiry),
        config_secret_name: secret.map(|s| s.name.clone()),
        retiring: plan
            .retiring
            .iter()
            .copied()
            .filter(|id| Some(*id) != current_id)
            .collect(),
    }
}

fn rotating_verdict(world: &TokenWorld, plan: &TokenPlan, minted: Option<MintReason>) -> Verdict {
    let facts = token_facts(world, plan);
    if !plan.unresolved_consumers.is_empty() {
        return Verdict::yes(
            "ConsumerUnresolved",
            format!(
                "revocation is held: the token used by {} cannot be resolved",
                plan.unresolved_consumers.join(", ")
            ),
        );
    }
    if !facts.retiring.is_empty() {
        let ids: Vec<String> = facts.retiring.iter().map(Uuid::to_string).collect();
        let reason = minted.map_or("RetiringTokens", MintReason::reason);
        return Verdict::yes(
            reason,
            format!(
                "tokens kept until no pod, ReplicaSet or template uses them: {}",
                ids.join(", ")
            ),
        );
    }
    Verdict::no(
        "TokenCurrent",
        facts.current.map_or_else(
            || "no other token is held".to_string(),
            |(id, _)| format!("token {id} is current; no other token is held"),
        ),
    )
}

/// Revoke and garbage-collect per `plan`. Revocation goes first: a Secret
/// whose token is still on the account is collected on a later pass, while a
/// token whose Secret is gone is an orphan revoked after the grace period.
#[allow(clippy::too_many_arguments)]
async fn collect(
    idm: &KanidmClient,
    metrics: &ControllerMetrics,
    kube: Client,
    namespace: &str,
    owner: &OwnerReference,
    account: &str,
    plan: &TokenPlan,
    state: &mut PassState,
) -> std::result::Result<(), Failure> {
    for token_id in &plan.revoke {
        let outcome = revoke_token(idm, metrics, account, *token_id)
            .await
            .map_err(Failure::exec(ConditionKind::Rotating))?;
        if outcome == RevokeOutcome::Revoked {
            info!(%token_id, "revoked unused RADIUS token");
            state.changed = true;
        }
    }
    let secrets: Api<Secret> = Api::namespaced(kube, namespace);
    for name in &plan.gc_secrets {
        let outcome = delete_owned(&secrets, namespace, name, owner, DeleteMode::Default)
            .await
            .map_err(Failure::exec(ConditionKind::Rotating))?;
        if outcome == DeleteOutcome::Deleted {
            info!(secret = %name, "deleted unused RADIUS config Secret");
            state.changed = true;
        }
    }
    Ok(())
}

/// One finalizer step per pass; see [`cleanup`].
/// Whether the referenced Kanidm CR exists. The cache answers yes; a no is
/// confirmed live, because skipping the identity cleanup on a cache that has
/// not synced yet (operator start) would leak the account.
async fn kanidm_exists(radius: &KanidmRadiusServer, ctx: &Context) -> Result<bool> {
    if ctx.kaniop_ctx.get_kanidm(radius).is_some() {
        return Ok(true);
    }
    let namespace = radius.kanidm_namespace();
    let name = radius.kanidm_name();
    Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace)
        .get_opt(&name)
        .await
        .map(|kanidm| kanidm.is_some())
        .map_err(|e| Error::kube_error("get", "Kanidm", namespace, name, e))
}

async fn finalize(radius: &KanidmRadiusServer, ctx: &Context) -> Result<(Action, bool)> {
    let client = ctx.kaniop_ctx.client.clone();
    let drain = observe_drain(client.clone(), radius)
        .await
        .map_err(finalizer_error)?;
    let view = CleanupView {
        deployment: drain.deployment.as_ref().map(|d| {
            if d.metadata.deletion_timestamp.is_some() {
                DeploymentPhase::Deleting
            } else {
                DeploymentPhase::Live
            }
        }),
        consumers: drain.consumers.iter().map(Consumer::label).collect(),
        kanidm_present: kanidm_exists(radius, ctx).await?,
    };
    let finish = match next_step(&view) {
        CleanupStep::DeleteDeployment => {
            let namespace = radius.namespace().unwrap_or_default();
            let owner = radius
                .controller_owner_ref(&())
                .ok_or_else(|| Error::MissingData("RADIUS server has no UID".into()))?;
            let deployments: Api<Deployment> = Api::namespaced(client, &namespace);
            delete_owned(
                &deployments,
                &namespace,
                &radius.name_any(),
                &owner,
                DeleteMode::Foreground,
            )
            .await
            .map_err(finalizer_error)?;
            info!("deleting RADIUS Deployment before identity cleanup");
            return Ok((Action::requeue(DRAIN_REQUEUE), true));
        }
        CleanupStep::AwaitDrain { consumers } => {
            debug!(?consumers, "waiting for RADIUS pods to drain");
            let uid = radius.uid().unwrap_or_default();
            if ctx
                .drain_warnings
                .should_warn(&uid, &consumers, Instant::now())
            {
                // Best effort: a failed event must not turn the wait into an
                // error with backoff.
                let note = drain_note(&consumers);
                if let Err(error) = publish_warning(radius, ctx, DRAIN_WARNING_REASON, &note).await
                {
                    warn!(%error, "failed to publish RADIUS drain warning");
                }
            }
            return Ok((Action::requeue(DRAIN_REQUEUE), false));
        }
        CleanupStep::Skip(reason) => Finish::SkipAndRemove(reason),
        CleanupStep::DeleteIdentity => {
            let identity = RadiusIdentity::for_resource(radius)
                .map_err(|e| Error::MissingData(format!("RADIUS identity: {e}")))?;
            let idm = ctx.get_idm_client(radius).await?;
            let result = delete_identity(&idm, &ctx.kaniop_ctx.metrics, &identity).await;
            match identity_cleanup(result) {
                Ok(finish) => finish,
                Err(Failure::Transient { error, .. }) => return Err(error),
                Err(Failure::Block(block)) => {
                    warn!(reason = block.reason(), %block, "RADIUS identity cleanup blocked");
                    let requeue = block.retry_after().unwrap_or(DRAIN_REQUEUE);
                    return Ok((Action::requeue(requeue), false));
                }
            }
        }
    };
    if let Finish::SkipAndRemove(reason) = finish {
        warn_skip(radius, ctx, reason).await?;
    }
    remove_finalizer(radius, ctx.kaniop_ctx.client.clone()).await?;
    ctx.drain_warnings.forget(&radius.uid().unwrap_or_default());
    info!("RADIUS server cleaned up");
    Ok((Action::await_change(), true))
}

async fn warn_skip(radius: &KanidmRadiusServer, ctx: &Context, reason: SkipReason) -> Result<()> {
    warn!(reason = reason.reason(), "skipping RADIUS Kanidm cleanup");
    publish_warning(radius, ctx, reason.reason(), reason.note()).await
}

/// Executor errors during deletion: transient ones keep their source; a
/// changed object retries on the next pass through the backoff.
fn finalizer_error(error: ExecError) -> Error {
    match error {
        ExecError::Api(error) => error,
        other => Error::MissingData(format!("RADIUS finalizer: {other}")),
    }
}

async fn publish_warning(
    radius: &KanidmRadiusServer,
    ctx: &Context,
    reason: &str,
    note: &str,
) -> Result<()> {
    ctx.kaniop_ctx
        .recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: reason.to_string(),
                note: Some(note.to_string()),
                action: "Reconcile".to_string(),
                secondary: None,
            },
            &radius.object_ref(&()),
        )
        .await
        .map_err(|e| {
            Error::kube_error(
                "publish",
                "event",
                radius.namespace().unwrap_or_default(),
                radius.name_any(),
                e,
            )
        })
}

/// JSON patch that adds the finalizer only if the list is unchanged since
/// it was read (same approach as kube-runtime's finalizer helper).
fn add_finalizer_patch(radius: &KanidmRadiusServer) -> serde_json::Value {
    match radius.metadata.finalizers.as_deref() {
        None | Some([]) => json!([
            {"op": "test", "path": "/metadata/finalizers", "value": radius.metadata.finalizers},
            {"op": "add", "path": "/metadata/finalizers", "value": [RADIUS_FINALIZER]},
        ]),
        Some(current) => json!([
            {"op": "test", "path": "/metadata/finalizers", "value": current},
            {"op": "add", "path": "/metadata/finalizers/-", "value": RADIUS_FINALIZER},
        ]),
    }
}

/// JSON patch that removes our finalizer at its observed index, guarded by
/// a test so a concurrent change of the list fails instead of removing the
/// wrong entry.
fn remove_finalizer_patch(radius: &KanidmRadiusServer) -> Option<serde_json::Value> {
    let index = radius
        .finalizers()
        .iter()
        .position(|f| f == RADIUS_FINALIZER)?;
    let path = format!("/metadata/finalizers/{index}");
    Some(json!([
        {"op": "test", "path": path, "value": RADIUS_FINALIZER},
        {"op": "remove", "path": path},
    ]))
}

async fn patch_finalizers(
    radius: &KanidmRadiusServer,
    client: Client,
    patch: serde_json::Value,
) -> Result<()> {
    let namespace = radius.namespace().unwrap_or_default();
    let name = radius.name_any();
    let patch = serde_json::from_value(patch)
        .map_err(|e| Error::SerializationError("RADIUS finalizer patch".into(), e))?;
    match Api::<KanidmRadiusServer>::namespaced(client, &namespace)
        .patch(&name, &PatchParams::default(), &Patch::<()>::Json(patch))
        .await
    {
        Ok(_) => Ok(()),
        // Already gone: nothing left to finalize.
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(()),
        Err(e) => Err(Error::kube_error(
            "patch finalizers of",
            "KanidmRadiusServer",
            namespace,
            name,
            e,
        )),
    }
}

async fn add_finalizer(radius: &KanidmRadiusServer, client: Client) -> Result<()> {
    patch_finalizers(radius, client, add_finalizer_patch(radius)).await
}

async fn remove_finalizer(radius: &KanidmRadiusServer, client: Client) -> Result<()> {
    match remove_finalizer_patch(radius) {
        Some(patch) => patch_finalizers(radius, client, patch).await,
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::ByteString;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use serde_json::json;

    use kaniop_operator::kanidm::crd::KanidmSpec;

    use openssl::asn1::Asn1Time;
    use openssl::hash::MessageDigest;
    use openssl::pkey::PKey;
    use openssl::rsa::Rsa;
    use openssl::x509::{X509, X509Name};
    use serde_json::Value;

    use super::*;
    use crate::reconcile::kanidm_mock::{self, MockKanidm, Request};
    use crate::reconcile::kube_mock::{Recorded, api_status, mock_client, to_json};
    use crate::reconcile::status::{TYPE_AVAILABLE, TYPE_CONFIGURATION_READY};
    use crate::reconcile::token::SAFETY_MARGIN;

    const UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";
    const ACCOUNT: &str = "kaniop_radius_d10066c345994487972d26bb42587a32";
    /// Current token: mounted by the Deployment template through `SECRET_A`.
    const TOKEN_A: &str = "9ad74e68-147f-49e8-96db-dc21c920ff3b";
    /// Retired token: nothing uses it, its Secret is `SECRET_B`.
    const TOKEN_B: &str = "31b30b65-037a-418f-b7ca-0c376da2db28";
    const SECRET_A: &str = "campus-0a1b2c3d4e";
    const SECRET_B: &str = "campus-f0e1d2c3b4";
    const RUNNING_IMAGE: &str = "registry.example.com/kanidm/radius:1.11.2";

    fn radius() -> KanidmRadiusServer {
        serde_json::from_value(json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmRadiusServer",
            "metadata": {"name": "campus", "namespace": "network", "uid": UID, "generation": 2},
            "spec": {
                "kanidmRef": {"name": "idm"},
                "authorization": {"requiredGroups": ["allowed@idm.example.com"]},
                "clients": [{"name": "nas", "ipaddr": "192.0.2.1",
                             "sharedSecretRef": {"name": "nas-secret", "key": "nas"}}],
                "radiusTls": {"secretRef": {"name": "eap-tls"}}
            }
        }))
        .unwrap()
    }

    fn kanidm() -> Kanidm {
        Kanidm {
            metadata: kube::api::ObjectMeta {
                name: Some("idm".into()),
                namespace: Some("network".into()),
                ..Default::default()
            },
            spec: KanidmSpec {
                domain: "idm.example.com".into(),
                image: "kanidm/server:1.11.2".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn metrics() -> (ControllerMetrics, SdkMeterProvider) {
        let provider = SdkMeterProvider::builder().build();
        (
            ControllerMetrics::new("radius-test", &provider.meter("test")),
            provider,
        )
    }

    #[tokio::test]
    async fn invalid_input_touches_nothing_and_keeps_available() {
        // The NAS Secret exists but its key was deleted.
        let nas_secret = Secret {
            metadata: kube::api::ObjectMeta {
                name: Some("nas-secret".into()),
                namespace: Some("network".into()),
                ..Default::default()
            },
            data: Some([("other".to_string(), ByteString(b"x".to_vec()))].into()),
            ..Default::default()
        };
        let (client, recorder) = mock_client(vec![(200, to_json(&nas_secret))]);
        let (metrics, _provider) = metrics();
        let running = Deployment {
            metadata: kube::api::ObjectMeta {
                name: Some("campus".into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::apps::v1::DeploymentSpec {
                replicas: Some(2),
                ..Default::default()
            }),
            status: Some(k8s_openapi::api::apps::v1::DeploymentStatus {
                observed_generation: Some(1),
                replicas: Some(2),
                updated_replicas: Some(2),
                available_replicas: Some(2),
                ready_replicas: Some(2),
                ..Default::default()
            }),
        };
        let mut state = PassState {
            deployment: Some(running),
            ..Default::default()
        };
        let mut report = StatusReport::default();
        let result = run_pass(
            &radius(),
            &kanidm(),
            client,
            &metrics,
            || async { panic!("Kanidm must not be contacted for an invalid input") },
            &mut report,
            &mut state,
        )
        .await;
        let failure = result.unwrap_err();
        assert!(
            matches!(
                failure,
                Failure::Block(RadiusBlock::Inputs(
                    RadiusPreflightError::NasSecretKeyUnavailable
                ))
            ),
            "{failure:?}"
        );
        assert!(!state.changed);
        let requests = recorder_requests(recorder).await;
        assert_eq!(
            requests,
            ["GET /api/v1/namespaces/network/secrets/nas-secret"],
            "only the input read may happen"
        );

        report.record_failure(&failure);
        let status = build_status(
            Some(2),
            None,
            &report,
            state.deployment.as_ref(),
            None,
            &Time(Timestamp::now()),
        );
        let conditions = status.conditions.unwrap();
        let get = |t: &str| conditions.iter().find(|c| c.type_ == t).unwrap().clone();
        assert_eq!(get(TYPE_CONFIGURATION_READY).status, "False");
        assert_eq!(get(TYPE_AVAILABLE).status, "True");
        assert!(!status.ready);
    }

    fn owner_refs() -> Value {
        json!([{"apiVersion": "kaniop.rs/v1beta1", "kind": "KanidmRadiusServer",
                "name": "campus", "uid": UID, "controller": true}])
    }

    fn input_secrets() -> [(u16, Value); 2] {
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut subject = X509Name::builder().unwrap();
        subject
            .append_entry_by_text("CN", "radius.example.com")
            .unwrap();
        let subject = subject.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_subject_name(&subject).unwrap();
        cert.set_issuer_name(&subject).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(30).unwrap())
            .unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let secret = |name: &str, data: Vec<(&str, Vec<u8>)>| Secret {
            metadata: kube::api::ObjectMeta {
                name: Some(name.into()),
                namespace: Some("network".into()),
                ..Default::default()
            },
            data: Some(
                data.into_iter()
                    .map(|(k, v)| (k.to_string(), ByteString(v)))
                    .collect(),
            ),
            ..Default::default()
        };
        [
            (
                200,
                to_json(&secret(
                    "nas-secret",
                    vec![("nas", b"a-long-enough-nas-shared-secret".to_vec())],
                )),
            ),
            (
                200,
                to_json(&secret(
                    "eap-tls",
                    vec![
                        ("tls.crt", cert.build().to_pem().unwrap()),
                        ("tls.key", key.private_key_to_pem_pkcs8().unwrap()),
                    ],
                )),
            ),
        ]
    }

    /// Our Deployment, running `RUNNING_IMAGE` with token A from `SECRET_A`.
    fn running_deployment(resource_version: &str) -> Deployment {
        serde_json::from_value(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "campus", "namespace": "network", "uid": "dep-uid",
                         "resourceVersion": resource_version, "generation": 1,
                         "ownerReferences": owner_refs()},
            "spec": {
                "replicas": 1,
                "selector": {"matchLabels": {"kaniop.rs/radius-uid": UID}},
                "template": {
                    "metadata": {"annotations": {"kaniop.rs/radius-token-id": TOKEN_A}},
                    "spec": {
                        "containers": [{"name": COMPONENT, "image": RUNNING_IMAGE}],
                        "volumes": [{"name": "radius-data", "projected": {"sources": [
                            {"secret": {"name": SECRET_A,
                                        "items": [{"key": "radius.toml", "path": "radius.toml"}]}}
                        ]}}]
                    }
                }
            }
        }))
        .unwrap()
    }

    fn config_secret(name: &str, token: &str) -> Value {
        json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": name, "namespace": "network", "uid": format!("uid-{name}"),
                         "resourceVersion": "1",
                         "labels": {"kaniop.rs/radius-uid": UID},
                         "annotations": {"kaniop.rs/radius-token-id": token},
                         "ownerReferences": owner_refs()},
            "data": {"auth_token": "YmVhcmVy"}
        })
    }

    fn list(kind: &str, items: Vec<Value>) -> (u16, Value) {
        (
            200,
            json!({"apiVersion": "v1", "kind": kind, "metadata": {}, "items": items}),
        )
    }

    /// Replies to one `observe`: the Deployment, then the namespace lists.
    fn observation_replies() -> Vec<(u16, Value)> {
        vec![
            (200, to_json(&running_deployment("1"))),
            list(
                "SecretList",
                vec![
                    config_secret(SECRET_A, TOKEN_A),
                    config_secret(SECRET_B, TOKEN_B),
                ],
            ),
            list("ReplicaSetList", vec![]),
            list("PodList", vec![]),
        ]
    }

    /// Kanidm with our account, its membership and tokens A and B, both
    /// days old; token deletes succeed.
    async fn kanidm_with_two_tokens() -> MockKanidm {
        let identity = RadiusIdentity::for_resource(&radius()).unwrap();
        let entry = json!({"attrs": {"displayname": [identity.display_name]}}).to_string();
        let issued = OffsetDateTime::now_utc().unix_timestamp() - 3 * 86_400;
        let token = |id: &str| {
            json!({"account_id": "00000000-0000-0000-0000-000000000001", "token_id": id,
                   "label": "kaniop-radius-x", "expiry": null, "issued_at": issued,
                   "purpose": "readonly"})
        };
        let tokens = json!([token(TOKEN_A), token(TOKEN_B)]).to_string();
        let account_path = format!("/v1/service_account/{ACCOUNT}");
        let tokens_path = format!("{account_path}/_api_token");
        kanidm_mock::start(
            move |req: &Request| match (req.method.as_str(), req.path.as_str()) {
                ("GET", path) if path == account_path => (200, entry.clone()),
                ("GET", "/v1/group/idm_radius_servers/_attr/member") => {
                    (200, json!([ACCOUNT]).to_string())
                }
                ("GET", path) if path == tokens_path => (200, tokens.clone()),
                ("DELETE", path) if path.starts_with(&tokens_path) => (200, "null".into()),
                _ => (500, "{}".into()),
            },
        )
        .await
    }

    fn uris(requests: &[Recorded]) -> Vec<String> {
        requests
            .iter()
            .map(|r| format!("{} {}", r.method, r.uri.split('?').next().unwrap()))
            .collect()
    }

    /// The Deployment is applied, then the Service apply fails. Revoking the
    /// unused token and collecting its Secret do not depend on the Service,
    /// so they still run; the Service failure is reported afterwards.
    #[tokio::test]
    async fn service_apply_failure_still_revokes_and_collects() {
        let idm = kanidm_with_two_tokens().await;
        let mut replies = input_secrets().to_vec();
        replies.extend(observation_replies());
        replies.extend([
            // New config Secret (the rendered content differs from SECRET_A).
            api_status(404, "NotFound"),
            (201, config_secret("campus-new", TOKEN_A)),
            // Deployment apply.
            (200, to_json(&running_deployment("1"))),
            (200, to_json(&running_deployment("2"))),
            // Service apply fails.
            api_status(404, "NotFound"),
            api_status(500, "InternalError"),
        ]);
        replies.extend(observation_replies());
        replies.extend([
            (200, config_secret(SECRET_B, TOKEN_B)),
            (200, config_secret(SECRET_B, TOKEN_B)),
        ]);
        let (client, recorder) = mock_client(replies);
        let idm_client = idm.client.clone();
        let mut state = PassState::default();
        let mut report = StatusReport::default();
        let result = run_pass(
            &radius(),
            &kanidm(),
            client,
            &idm.metrics,
            move || async move { Ok(idm_client) },
            &mut report,
            &mut state,
        )
        .await;

        match result {
            Err(Failure::Transient { condition, .. }) => {
                assert_eq!(condition, ConditionKind::Exposed)
            }
            other => panic!("expected the Service failure, got {other:?}"),
        }
        let revoked: Vec<String> = idm
            .writes()
            .into_iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        assert_eq!(
            revoked,
            [format!(
                "DELETE /v1/service_account/{ACCOUNT}/_api_token/{TOKEN_B}"
            )],
            "only the unused token is revoked"
        );
        let requests = uris(&recorder.await.unwrap());
        assert_eq!(
            requests.last().map(String::as_str),
            Some(format!("DELETE /api/v1/namespaces/network/secrets/{SECRET_B}").as_str()),
            "{requests:#?}"
        );
        assert!(state.changed);
        assert!(
            state.rotation_applied,
            "a block after the apply must not raise TokenNearExpiry"
        );
        assert_eq!(
            state
                .deployment
                .and_then(|d| d.metadata.resource_version)
                .as_deref(),
            Some("2")
        );
        assert_eq!(
            report.exposed.map(|v| v.reason).as_deref(),
            Some("TransientError")
        );
        assert_eq!(report.tokens.map(|t| t.retiring), Some(vec![]));
    }

    /// A Kanidm image the RADIUS image cannot be derived from.
    fn kanidm_unresolvable() -> Kanidm {
        let mut kanidm = kanidm();
        kanidm.spec.image = "kanidm/server:latest".into();
        kanidm
    }

    /// Image unresolved but a Deployment runs: the pass keeps that image and
    /// goes on (observe, plan, apply, revoke), reporting the block on
    /// `ConfigurationReady` so it stays visible.
    #[tokio::test]
    async fn unresolved_image_reuses_the_running_image_and_keeps_rotating() {
        let idm = kanidm_with_two_tokens().await;
        let mut replies = input_secrets().to_vec();
        replies.extend(observation_replies());
        let service: Service = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Service",
            "metadata": {"name": "campus", "namespace": "network", "resourceVersion": "1",
                         "ownerReferences": owner_refs()},
            "spec": {"type": "ClusterIP", "clusterIP": "10.0.0.10"}
        }))
        .unwrap();
        replies.extend([
            api_status(404, "NotFound"),
            (201, config_secret("campus-new", TOKEN_A)),
            (200, to_json(&running_deployment("1"))),
            (200, to_json(&running_deployment("2"))),
            api_status(404, "NotFound"),
            (200, to_json(&service)),
        ]);
        replies.extend(observation_replies());
        replies.extend([
            (200, config_secret(SECRET_B, TOKEN_B)),
            (200, config_secret(SECRET_B, TOKEN_B)),
        ]);
        let (client, recorder) = mock_client(replies);
        let idm_client = idm.client.clone();
        let mut state = PassState {
            deployment: Some(running_deployment("1")),
            ..Default::default()
        };
        let mut report = StatusReport::default();
        let result = run_pass(
            &radius(),
            &kanidm_unresolvable(),
            client,
            &idm.metrics,
            move || async move { Ok(idm_client) },
            &mut report,
            &mut state,
        )
        .await;

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(report.configuration.truth, Truth::False);
        assert_eq!(report.configuration.reason, "ImageUnresolved");
        assert_eq!(report.identity.truth, Truth::True);
        let requests = recorder.await.unwrap();
        let apply = requests
            .iter()
            .find(|r| r.method == "PATCH" && r.uri.contains("/deployments/campus"))
            .expect("Deployment applied");
        assert_eq!(
            apply.json()["spec"]["template"]["spec"]["containers"][0]["image"],
            RUNNING_IMAGE
        );
        assert_eq!(
            idm.writes().len(),
            1,
            "the unused token is still revoked: {:?}",
            idm.writes()
        );
    }

    /// Image unresolved and nothing runs yet: blocked before any mutation,
    /// as before.
    #[tokio::test]
    async fn unresolved_image_without_a_deployment_blocks_before_kanidm() {
        let (client, recorder) = mock_client(input_secrets().to_vec());
        let (metrics, _provider) = metrics();
        let mut state = PassState::default();
        let mut report = StatusReport::default();
        let result = run_pass(
            &radius(),
            &kanidm_unresolvable(),
            client,
            &metrics,
            || async { panic!("Kanidm must not be contacted without an image") },
            &mut report,
            &mut state,
        )
        .await;
        assert!(
            matches!(result, Err(Failure::Block(RadiusBlock::Image(_)))),
            "{result:?}"
        );
        assert!(!state.changed);
        assert!(!state.rotation_applied);
        assert_eq!(recorder_requests(recorder).await.len(), 2);
    }

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(seconds).unwrap()
    }

    fn observed_facts(expiry: Option<OffsetDateTime>) -> StatusReport {
        StatusReport {
            tokens: Some(TokenFacts {
                current: Some((Uuid::parse_str(TOKEN_A).unwrap(), at(0))),
                current_expiry: expiry,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn running_token_expiry_prefers_the_observed_token() {
        let rotation = TokenPolicy::from_spec(Some(&radius().spec.token_rotation));
        let observed = observed_facts(Some(at(1_000_000)));
        assert_eq!(
            running_token_expiry(&observed, None, &rotation),
            Some((TOKEN_A.to_string(), at(1_000_000)))
        );
        // Observed without expiry: it never lapses.
        assert_eq!(
            running_token_expiry(&observed_facts(None), None, &rotation),
            None
        );
    }

    #[test]
    fn running_token_expiry_estimates_from_status_when_not_observed() {
        let previous = KanidmRadiusServerStatus {
            token_id: Some(TOKEN_A.into()),
            token_issued_at: kube_time(at(1_000)),
            ..Default::default()
        };
        let enabled = TokenPolicy::from_spec(Some(&kaniop_operator::crd::SecretRotation {
            enabled: true,
            period_days: 30,
        }));
        assert_eq!(
            running_token_expiry(&StatusReport::default(), Some(&previous), &enabled),
            Some((TOKEN_A.to_string(), at(1_000) + time::Duration::days(60)))
        );
        // Rotation disabled: minted without expiry.
        let disabled = TokenPolicy::from_spec(None);
        assert_eq!(
            running_token_expiry(&StatusReport::default(), Some(&previous), &disabled),
            None
        );
        assert_eq!(
            running_token_expiry(&StatusReport::default(), None, &enabled),
            None
        );
    }

    #[test]
    fn near_expiry_note_within_twice_the_safety_margin() {
        let block = RadiusBlock::NamespaceNotWatched {
            kanidm: "identity/idm".into(),
        };
        let expiry = at(10_000_000);
        let window = SAFETY_MARGIN * 2;
        let running = || Some((TOKEN_A.to_string(), expiry));
        assert_eq!(
            token_near_expiry_note(
                &block,
                running(),
                expiry - window - time::Duration::seconds(1)
            ),
            None
        );
        let note = token_near_expiry_note(&block, running(), expiry - window).unwrap();
        assert!(note.contains(TOKEN_A), "{note}");
        assert!(note.contains("NamespaceNotWatched"), "{note}");
        assert!(
            note.contains(&kube_time(expiry).unwrap().0.to_string()),
            "{note}"
        );
        // Already expired still warns; nothing running never does.
        assert!(token_near_expiry_note(&block, running(), expiry + window).is_some());
        assert_eq!(token_near_expiry_note(&block, None, expiry), None);
    }

    async fn recorder_requests(
        recorder: tokio::task::JoinHandle<Vec<crate::reconcile::kube_mock::Recorded>>,
    ) -> Vec<String> {
        // The mock ends when the client (moved into run_pass) is dropped.
        recorder
            .await
            .unwrap()
            .into_iter()
            .map(|r| format!("{} {}", r.method, r.uri.split('?').next().unwrap()))
            .collect()
    }

    #[test]
    fn kanidm_uri_prefers_origin_over_domain() {
        let mut kanidm = kanidm();
        assert_eq!(kanidm_uri(&kanidm), "https://idm.example.com");
        kanidm.spec.origin = Some("https://idm.example.com:8443".into());
        assert_eq!(kanidm_uri(&kanidm), "https://idm.example.com:8443");
    }

    #[test]
    fn finalizer_patches_are_guarded() {
        let mut radius = radius();
        assert_eq!(
            add_finalizer_patch(&radius),
            json!([
                {"op": "test", "path": "/metadata/finalizers", "value": null},
                {"op": "add", "path": "/metadata/finalizers", "value": [RADIUS_FINALIZER]},
            ])
        );
        assert_eq!(remove_finalizer_patch(&radius), None);
        radius.metadata.finalizers = Some(vec!["other/finalizer".into(), RADIUS_FINALIZER.into()]);
        assert_eq!(
            remove_finalizer_patch(&radius),
            Some(json!([
                {"op": "test", "path": "/metadata/finalizers/1", "value": RADIUS_FINALIZER},
                {"op": "remove", "path": "/metadata/finalizers/1"},
            ]))
        );
        radius.metadata.finalizers = Some(vec!["other/finalizer".into()]);
        assert_eq!(
            add_finalizer_patch(&radius)[1],
            json!({"op": "add", "path": "/metadata/finalizers/-", "value": RADIUS_FINALIZER})
        );
    }
}
