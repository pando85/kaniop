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
use std::time::Duration;

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
use crate::crd::KanidmRadiusServer;
use crate::reconcile::apply::{
    DeleteMode, DeleteOutcome, SecretOutcome, apply_owned, delete_owned, ensure_config_secret,
    is_controlled_by,
};
use crate::reconcile::cleanup::{
    CleanupStep, CleanupView, DeploymentPhase, Finish, SkipReason, identity_cleanup, next_step,
};
use crate::reconcile::error::{ExecError, Failure, RadiusBlock};
use crate::reconcile::identity::{RADIUS_GROUP, RadiusIdentity, delete_identity, ensure_identity};
use crate::reconcile::image::resolve_radius_image;
use crate::reconcile::inputs::{RadiusPreflightError, prepare_radius_inputs};
use crate::reconcile::kanidm_token::{
    RevokeOutcome, VerifyRetry, mint_token, revoke_token, verify_token,
};
use crate::reconcile::observe::{observe, observe_drain};
use crate::reconcile::status::{
    ConditionKind, StatusReport, TokenFacts, Truth, Verdict, available_verdict, build_status,
    patch_status,
};
use crate::reconcile::token::{
    Consumer, ConsumerKind, CurrentToken, MintReason, ObservedSecret, TokenPlan, TokenPolicy,
    TokenWorld, plan_tokens,
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

/// The live objects a pass reports on and whether it changed anything.
#[derive(Debug, Default)]
struct PassState {
    deployment: Option<Deployment>,
    service: Option<Service>,
    changed: bool,
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
        changed: false,
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
            let requeue = block.retry_after().unwrap_or_else(idm_reconcile_interval);
            Ok((Action::requeue(requeue), state.changed))
        }
        // The source error wins over a status write failure.
        Err(Failure::Transient { error, .. }) => Err(error),
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
    let image = resolve_radius_image(&kanidm.spec.image, radius.spec.image.as_deref())
        .map_err(RadiusBlock::from)?;
    RadiusServiceExposure::from_spec(
        &radius.spec.service.type_,
        radius.spec.service.external_traffic_policy.clone(),
    )
    .map_err(RadiusBlock::from)?;
    let identity = RadiusIdentity::for_resource(radius).map_err(RadiusBlock::from)?;
    let namespace = radius
        .namespace()
        .ok_or(RadiusBlock::Inputs(RadiusPreflightError::MissingNamespace))?;
    let owner =
        radius
            .controller_owner_ref(&())
            .ok_or(RadiusBlock::Exec(ExecError::MissingMetadata {
                kind: "KanidmRadiusServer",
                field: "uid",
            }))?;
    report.configuration = Verdict::yes(
        "ConfigurationValid",
        "referenced Secrets, CA and TLS material are valid",
    );

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
    let services: Api<Service> = Api::namespaced(kube.clone(), &namespace);
    let desired = workload.service().map_err(RadiusBlock::from)?;
    let service_before = state
        .service
        .as_ref()
        .and_then(|s| s.metadata.resource_version.clone());
    let applied = apply_owned(&services, &desired, &owner)
        .await
        .map_err(Failure::exec(ConditionKind::Exposed))?;
    state.changed |= applied.metadata.resource_version != service_before;
    state.service = Some(applied);

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
        .and_then(|id| world.tokens.iter().find(|t| t.id == id))
        .map(|t| (t.id, t.issued_at));
    let current_id = current.map(|(id, _)| id);
    TokenFacts {
        current,
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
        consumers: drain.consumers.iter().map(consumer_label).collect(),
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
    info!("RADIUS server cleaned up");
    Ok((Action::await_change(), true))
}

fn consumer_label(consumer: &Consumer) -> String {
    let kind = match consumer.kind {
        ConsumerKind::ReplicaSet => "replicaset",
        ConsumerKind::Pod => "pod",
    };
    format!("{kind}/{}", consumer.name)
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

    use super::*;
    use crate::reconcile::kube_mock::{mock_client, to_json};
    use crate::reconcile::status::{TYPE_AVAILABLE, TYPE_CONFIGURATION_READY};

    const UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";

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
