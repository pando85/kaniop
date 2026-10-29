use crate::controller::Context;
use crate::crd::{
    CredentialBootstrapDelivery, CredentialBootstrapState, CredentialBootstrapStatus,
    CredentialState, KanidmPersonAccount, KanidmPersonAccountStatus, KanidmPersonAttributes,
};

use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::resources::last_transition_time;
use kaniop_operator::controller::kanidm::{KanidmResource, is_resource_watched};
use kaniop_operator::controller::{context::IdmClientContext, idm_reconcile_interval};
use kaniop_operator::crd::KanidmAccountPosixAttributes;
use kaniop_operator::metrics::{
    KANIDM_OP_CREATE, KANIDM_OP_CREDENTIAL_UPDATE_INTENT, KANIDM_OP_CREDENTIAL_UPDATE_SEND_INTENT,
    KANIDM_OP_DELETE, KANIDM_OP_GET, KANIDM_OP_GET_CREDENTIAL_STATUS, KANIDM_OP_UNIX_EXTEND,
    KANIDM_OP_UPDATE, KANIDM_OUTCOME_CHANGED, KANIDM_OUTCOME_ERROR, KANIDM_OUTCOME_UNCHANGED,
    KANIDM_RESOURCE_PERSON, record_kanidm_sdk_call,
};
use kaniop_operator::telemetry;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Not;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kanidm_client::{ClientError, KanidmClient};
use kanidm_proto::attribute::Attribute;
use kanidm_proto::constants::{
    ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM, ATTR_ATTESTED_PASSKEYS, ATTR_PASSKEYS,
    ATTR_PRIMARY_CREDENTIAL,
};
use kanidm_proto::internal::OperationError;
use kanidm_proto::scim_v1::{ScimEntryGetQuery, client::ScimSshPublicKeys};
use kanidm_proto::v1::Entry;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType};
use kube::runtime::finalizer::{Error as FinalizerError, Event as Finalizer, finalizer};
use kube::{Resource, ResourceExt};
use time::UtcOffset;
use time::format_description::well_known::Rfc3339;
use tracing::{Span, debug, field, info, instrument, trace, warn};

pub static PERSON_OPERATOR_NAME: &str = "kanidmpersonsaccounts.kaniop.rs";
pub static PERSON_FINALIZER: &str = "kanidmpersonsaccounts.kaniop.rs/finalizer";

const TYPE_CREDENTIAL: &str = "Credential";
const TYPE_CREDENTIAL_BOOTSTRAP_DELIVERY: &str = "CredentialBootstrapDelivery";
const ANNOTATION_CREDENTIAL_BOOTSTRAP_RESEND_REVISION: &str =
    "kaniop.rs/credential-bootstrap-resend-revision";
const TYPE_SSH_PUBLIC_KEYS: &str = "SshPublicKeysUpdated";
const TYPE_EXISTS: &str = "Exists";
const TYPE_UPDATED: &str = "Updated";
const TYPE_POSIX_INITIALIZED: &str = "PosixInitialized";
const TYPE_POSIX_UPDATED: &str = "PosixUpdated";
const TYPE_VALIDITY: &str = "Valid";
const REASON_ATTRIBUTES_MATCH: &str = "AttributesMatch";
const REASON_ATTRIBUTES_NOT_MATCH: &str = "AttributesNotMatch";
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";
const CONDITION_UNKNOWN: &str = "Unknown";

fn credential_state_from_attributes(
    primary: &Option<Vec<String>>,
    passkeys: &Option<Vec<String>>,
    attested_passkeys: &Option<Vec<String>>,
) -> CredentialState {
    if [primary, passkeys, attested_passkeys]
        .into_iter()
        .any(|values| {
            values
                .as_ref()
                .is_some_and(|values| values.is_empty().not())
        })
    {
        CredentialState::Present
    } else {
        CredentialState::Absent
    }
}

fn scim_search_allows_all_credential_attributes(search: &serde_json::Value) -> bool {
    match search {
        serde_json::Value::String(access) => access == "Grant",
        serde_json::Value::Object(access) => access
            .get("Allow")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|allowed| {
                [
                    ATTR_PRIMARY_CREDENTIAL,
                    ATTR_PASSKEYS,
                    ATTR_ATTESTED_PASSKEYS,
                ]
                .into_iter()
                .all(|required| {
                    allowed
                        .iter()
                        .any(|attribute| attribute.as_str() == Some(required))
                })
            }),
        _ => false,
    }
}

async fn credential_attributes_searchable(
    kanidm_client: &KanidmClient,
    name: &str,
) -> std::result::Result<bool, kanidm_client::ClientError> {
    let query = ScimEntryGetQuery {
        attributes: Some(vec![
            Attribute::PrimaryCredential,
            Attribute::PassKeys,
            Attribute::AttestedPasskeys,
        ]),
        ext_access_check: true,
        ..Default::default()
    };
    let path = format!("/scim/v1/Person/{name}");
    let entry: serde_json::Value = kanidm_client
        .perform_get_request_query(&path, Some(query))
        .await?;

    Ok(entry
        .get("extAccessCheck")
        .and_then(|access| access.get("search"))
        .is_some_and(scim_search_allows_all_credential_attributes))
}

fn ssh_public_keys_match(
    desired: &BTreeMap<String, String>,
    observed: &BTreeMap<String, String>,
    managed_tags: &[String],
) -> bool {
    let desired_tags: BTreeSet<_> = desired.keys().collect();
    let managed_tags: BTreeSet<_> = managed_tags.iter().collect();
    desired_tags == managed_tags
        && desired
            .iter()
            .all(|(tag, key)| observed.get(tag) == Some(key))
}

/// Read structured (tag, key) pairs, not authorized_keys output (which has no tags).
/// Kanidm is authoritative for SSH key syntax and supported algorithms.
async fn read_ssh_public_keys(
    kanidm_client: &KanidmClient,
    name: &str,
) -> Result<BTreeMap<String, String>> {
    let entry = kanidm_client
        .scim_v1_person_get(
            name,
            Some(ScimEntryGetQuery {
                attributes: Some(vec![Attribute::SshPublicKey]),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| {
            Error::KanidmClientError(
                format!("failed to read SSH public keys for person {name}"),
                Box::new(e),
            )
        })?;
    let Some(raw_keys) = entry.attrs.get(&Attribute::SshPublicKey) else {
        return Ok(BTreeMap::new());
    };
    let keys: ScimSshPublicKeys = serde_json::from_value(raw_keys.clone()).map_err(|e| {
        Error::SerializationError(
            format!("invalid SSH public key response from Kanidm for {name}"),
            e,
        )
    })?;
    Ok(keys
        .into_iter()
        .map(|k| (k.label, k.value.to_string()))
        .collect())
}

fn bootstrap_token_expired(status: &CredentialBootstrapStatus) -> bool {
    status
        .token_expires_at
        .as_ref()
        .is_some_and(|expiry| expiry.0 <= Timestamp::now())
}

fn should_create_reset_token(
    credential_state: CredentialState,
    bootstrap: &CredentialBootstrapStatus,
) -> bool {
    if credential_state != CredentialState::Absent {
        return false;
    }

    match bootstrap.state {
        CredentialBootstrapState::Pending => true,
        CredentialBootstrapState::TokenIssued => bootstrap_token_expired(bootstrap),
        CredentialBootstrapState::Complete => false,
    }
}

/// A missing annotation is revision zero. Invalid values never trigger an email;
/// the delivery condition explains the invalid request.
fn requested_bootstrap_resend_revision(person: &KanidmPersonAccount) -> Option<u32> {
    person
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(ANNOTATION_CREDENTIAL_BOOTSTRAP_RESEND_REVISION))
        .map_or(Some(0), |revision| revision.parse::<u32>().ok())
}

fn should_send_bootstrap_email(
    credential_state: CredentialState,
    bootstrap: &CredentialBootstrapStatus,
    requested_revision: Option<u32>,
) -> bool {
    if credential_state != CredentialState::Absent
        || bootstrap.state == CredentialBootstrapState::Complete
    {
        return false;
    }

    // A previously queued email stays queued even if delivery temporarily switches
    // to Events. Only an explicit, strictly higher resend revision queues another.
    requested_revision.is_some_and(|revision| {
        bootstrap
            .last_queued_resend_revision
            .is_none_or(|last_queued| revision > last_queued)
    })
}

/// Report whether the requested email has already been accepted, independently
/// from the last transport used or an error from a *newer* resend request.
fn bootstrap_email_condition(
    credential_state: CredentialState,
    bootstrap: &CredentialBootstrapStatus,
    requested_revision: Option<u32>,
) -> (&'static str, &str, &'static str) {
    if bootstrap.state == CredentialBootstrapState::Complete {
        return (
            CONDITION_TRUE,
            "Complete",
            "Credential bootstrap completed.",
        );
    }
    if credential_state == CredentialState::Unknown {
        return (
            CONDITION_UNKNOWN,
            "CredentialStateUnknown",
            "Credential status is unknown; no email will be queued.",
        );
    }
    let Some(requested_revision) = requested_revision else {
        return (
            CONDITION_FALSE,
            "InvalidResendRevision",
            "The credential-bootstrap-resend-revision annotation must be a non-negative integer within the u32 range.",
        );
    };
    if credential_state == CredentialState::Absent {
        if let Some(queued_revision) = bootstrap.last_queued_resend_revision {
            if requested_revision < queued_revision {
                return (
                    CONDITION_TRUE,
                    "StaleResendIgnored",
                    "The requested resend revision is older than the last queued revision; no new email was queued.",
                );
            }
            if requested_revision == queued_revision {
                return (
                    CONDITION_TRUE,
                    "EmailQueued",
                    "Kanidm accepted the email request; SMTP delivery is not confirmed.",
                );
            }
        }
    }
    if let Some(error) = bootstrap.delivery_error.as_deref() {
        return (
            CONDITION_FALSE,
            error,
            "Credential bootstrap email is blocked; inspect mail sender and account email.",
        );
    }
    (
        CONDITION_FALSE,
        "Pending",
        "Credential bootstrap email has not been queued yet.",
    )
}

fn should_create_event_token(
    credential_state: CredentialState,
    bootstrap: &CredentialBootstrapStatus,
) -> bool {
    if credential_state != CredentialState::Absent
        || bootstrap.state == CredentialBootstrapState::Complete
    {
        return false;
    }
    bootstrap.delivery_method == Some(CredentialBootstrapDelivery::Email)
        || should_create_reset_token(credential_state, bootstrap)
}

// JSON Merge Patch requires an explicit null to remove a previously stored field.
// Serde omits None fields from CredentialBootstrapStatus, so serializing the struct
// directly would leave stale delivery errors and delivery-mode metadata in status.
fn credential_bootstrap_merge_patch(bootstrap: &CredentialBootstrapStatus) -> serde_json::Value {
    serde_json::json!({
        "status": {
            "credentialBootstrap": {
                "state": bootstrap.state,
                "tokenExpiresAt": bootstrap.token_expires_at.as_ref(),
                "deliveryMethod": bootstrap.delivery_method,
                "lastQueuedResendRevision": bootstrap.last_queued_resend_revision,
                "emailQueuedAt": bootstrap.email_queued_at.as_ref(),
                "deliveryError": bootstrap.delivery_error.as_deref(),
            }
        }
    })
}

pub async fn watched_resource(person: &KanidmPersonAccount, ctx: Arc<Context>) -> bool {
    let kanidm = if let Some(k) = ctx.kaniop_ctx.get_kanidm(person) {
        k
    } else {
        trace!("no kanidm found");
        return false;
    };

    is_resource_watched(
        person,
        &kanidm,
        &ctx.kaniop_ctx.namespace_store,
        &ctx.kaniop_ctx.client,
    )
    .await
}

#[instrument(skip(ctx, person))]
pub async fn reconcile_person_account(
    person: Arc<KanidmPersonAccount>,
    ctx: Arc<Context>,
) -> Result<(Action, bool)> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx
        .kaniop_ctx
        .metrics
        .reconcile_count_and_measure(&trace_id);
    if !ctx.kaniop_ctx.kanidm_write_allowed(&person) {
        debug!("Kanidm restore in progress, pausing identity writes");
        return Ok((Action::requeue(Duration::from_secs(5)), false));
    }
    let kanidm_client = ctx.get_idm_client(&person).await?;

    if !watched_resource(&person, ctx.clone()).await {
        debug!("resource not watched, skipping reconcile");
        ctx.kaniop_ctx
            .recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "ResourceNotWatched".to_string(),
                    note: Some("configure `personNamespaceSelector` on Kanidm resource to watch this namespace".to_string()),
                    action: "Reconcile".to_string(),
                    secondary: None,
                },
                &person.object_ref(&()),
            )
            .await
            .map_err(|e| {
                warn!(error = %e, "failed to publish ResourceNotWatched event");
                Error::kube_error("publish", "event", person.get_namespace(), person.name_any(), e)
            })?;
        return Ok((Action::requeue(idm_reconcile_interval()), false));
    }
    info!("reconciling person account");

    let namespace = person.get_namespace();
    let status = person
        .update_status(kanidm_client.clone(), ctx.clone())
        .await
        .map_err(|e| {
            debug!(error = %e, "failed to reconcile status");
            ctx.kaniop_ctx.metrics.status_update_errors_inc();
            e
        })?;
    let persons_api: Api<KanidmPersonAccount> =
        Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
    let outcome = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let outcome_clone = outcome.clone();
    let action = finalizer(&persons_api, PERSON_FINALIZER, person, move |event| {
        let outcome = outcome_clone.clone();
        let ctx = ctx.clone();
        let status = status.clone();
        let kanidm_client = kanidm_client.clone();
        async move {
            match event {
                Finalizer::Apply(p) => {
                    let (action, changed) = p.reconcile(kanidm_client, status, ctx).await?;
                    outcome.store(changed, std::sync::atomic::Ordering::Relaxed);
                    Ok(action)
                }
                Finalizer::Cleanup(p) => {
                    let (action, changed) = p.cleanup(kanidm_client, status, ctx).await?;
                    outcome.store(changed, std::sync::atomic::Ordering::Relaxed);
                    Ok(action)
                }
            }
        }
    })
    .await
    .or_else(|e| match e {
        FinalizerError::RemoveFinalizer(kube::Error::Api(ae)) if ae.code == 404 => {
            debug!("resource already removed during finalizer cleanup");
            Ok(Action::requeue(idm_reconcile_interval()))
        }
        _ => Err(Error::FinalizerError(
            "failed on person account finalizer".to_string(),
            Box::new(e),
        )),
    })?;
    let changed = outcome.load(std::sync::atomic::Ordering::Relaxed);
    Ok((action, changed))
}

impl KanidmPersonAccount {
    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: person is namespaced scoped
        self.namespace().unwrap()
    }

    #[inline]
    async fn reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmPersonAccountStatus,
        ctx: Arc<Context>,
    ) -> Result<(Action, bool)> {
        match self
            .internal_reconcile(kanidm_client, status, ctx.clone())
            .await
        {
            Ok(result) => Ok(result),
            Err(e) => match e {
                Error::KanidmClientError(_, _) => {
                    ctx.kaniop_ctx
                        .recorder
                        .publish(
                            &Event {
                                type_: EventType::Warning,
                                reason: "KanidmError".to_string(),
                                note: Some(format!("{e:?}")),
                                action: "KanidmRequest".to_string(),
                                secondary: None,
                            },
                            &self.object_ref(&()),
                        )
                        .await
                        .map_err(|e| {
                            warn!(error = %e, "failed to publish KanidmError event");
                            Error::kube_error(
                                "publish",
                                "event",
                                self.get_namespace(),
                                self.name_any(),
                                e,
                            )
                        })?;
                    Err(e)
                }
                _ => Err(e),
            },
        }
    }

    async fn internal_reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmPersonAccountStatus,
        ctx: Arc<Context>,
    ) -> Result<(Action, bool)> {
        let name = &self.kanidm_entity_name();
        let metrics = &ctx.kaniop_ctx.metrics;

        let mut require_status_update = false;
        let mut changed = false;
        let mut actions = Vec::new();
        if is_person_false(TYPE_EXISTS, status.clone()) {
            debug!(
                condition = TYPE_EXISTS,
                status = CONDITION_FALSE,
                action = "create",
                "condition triggered action"
            );
            record_kanidm_sdk_call(
                metrics,
                KANIDM_RESOURCE_PERSON,
                KANIDM_OP_CREATE,
                KANIDM_OUTCOME_CHANGED,
                self.create(&kanidm_client, name),
            )
            .await?;
            require_status_update = true;
            changed = true;
            actions.push("create");
        }
        if is_person_false(TYPE_UPDATED, status.clone()) {
            debug!(
                condition = TYPE_UPDATED,
                status = CONDITION_FALSE,
                action = "update",
                "condition triggered action"
            );
            self.update(&kanidm_client, name, metrics).await?;
            require_status_update = true;
            changed = true;
            actions.push("update");
        }

        if is_person_false(TYPE_POSIX_UPDATED, status.clone())
            || (is_person_false(TYPE_POSIX_INITIALIZED, status.clone())
                && is_person(TYPE_POSIX_UPDATED, status.clone()))
        {
            debug!(
                condition = TYPE_POSIX_UPDATED,
                status = CONDITION_FALSE,
                action = "update_posix_attributes",
                "condition triggered action"
            );
            record_kanidm_sdk_call(
                metrics,
                KANIDM_RESOURCE_PERSON,
                KANIDM_OP_UNIX_EXTEND,
                KANIDM_OUTCOME_CHANGED,
                self.update_posix_attributes(&kanidm_client, name),
            )
            .await?;
            require_status_update = true;
            changed = true;
            actions.push("update_posix");
        }

        if is_person_false(TYPE_SSH_PUBLIC_KEYS, status.clone()) {
            self.sync_ssh_public_keys(&kanidm_client, name).await?;
            require_status_update = true;
            changed = true;
            actions.push("update_ssh_public_keys");
        }

        if self.spec.credential_bootstrap_delivery == CredentialBootstrapDelivery::Email {
            let requested_revision = requested_bootstrap_resend_revision(self);
            if should_send_bootstrap_email(
                status.credential_state,
                &status.credential_bootstrap,
                requested_revision,
            ) {
                // Fail closed: an opt-in email never falls back to a bearer URL in Events.
                let sender_configured = ctx
                    .kaniop_ctx
                    .get_kanidm(self)
                    .is_some_and(|kanidm| kanidm.spec.mail_sender.is_some());
                if !sender_configured {
                    self.record_bootstrap_delivery_error(
                        &status.credential_bootstrap,
                        "MailSenderNotConfigured",
                        ctx.clone(),
                    )
                    .await?;
                    changed = true;
                } else {
                    let send_result = record_kanidm_sdk_call(
                        metrics,
                        KANIDM_RESOURCE_PERSON,
                        KANIDM_OP_CREDENTIAL_UPDATE_SEND_INTENT,
                        KANIDM_OUTCOME_CHANGED,
                        kanidm_client.idm_person_account_credential_update_send_intent(
                            name,
                            Some(u64::from(self.spec.credentials_token_ttl)),
                            None,
                        ),
                    )
                    .await;

                    match send_result {
                        Ok(()) => {
                            let mut bootstrap = status.credential_bootstrap.clone();
                            bootstrap.state = CredentialBootstrapState::TokenIssued;
                            bootstrap.token_expires_at = None;
                            bootstrap.delivery_method = Some(CredentialBootstrapDelivery::Email);
                            // Preserve the highest queued revision across GitOps rollbacks
                            // and delivery-mode switches.
                            bootstrap.last_queued_resend_revision = Some(
                                bootstrap
                                    .last_queued_resend_revision
                                    .unwrap_or_default()
                                    .max(requested_revision.expect("validated resend revision")),
                            );
                            bootstrap.email_queued_at = Some(Time(Timestamp::now()));
                            bootstrap.delivery_error = None;
                            let queued_revision =
                                bootstrap.last_queued_resend_revision.unwrap_or_default();
                            self.update_credential_bootstrap_status(ctx.clone(), bootstrap)
                                .await?;
                            ctx.kaniop_ctx.recorder.publish(
                                &Event {
                                    type_: EventType::Normal,
                                    reason: "CredentialEmailQueued".to_string(),
                                    note: Some(format!(
                                        "Kanidm accepted the credential reset email (resend revision {queued_revision}) into its outgoing queue; SMTP delivery is not confirmed."
                                    )),
                                    action: format!("QueueCredentialBootstrapEmail revision {queued_revision}"),
                                    secondary: None,
                                },
                                &self.object_ref(&()),
                            ).await.map_err(|e| Error::kube_error(
                                "publish", "event", self.get_namespace(), self.name_any(), e
                            ))?;
                            changed = true;
                        }
                        Err(ClientError::Http(
                            _,
                            Some(OperationError::CU0008AccountMissingEmail),
                            _,
                        )) => {
                            self.record_bootstrap_delivery_error(
                                &status.credential_bootstrap,
                                "MissingEmail",
                                ctx.clone(),
                            )
                            .await?;
                            changed = true;
                        }
                        Err(e) => {
                            return Err(Error::kanidm_client_error(
                                "queue a credential reset email for",
                                name,
                                self.kanidm_namespace(),
                                self.kanidm_name(),
                                e,
                            ));
                        }
                    }
                }
            }
        } else if should_create_event_token(status.credential_state, &status.credential_bootstrap) {
            let token_expires_at = record_kanidm_sdk_call(
                metrics,
                KANIDM_RESOURCE_PERSON,
                KANIDM_OP_CREDENTIAL_UPDATE_INTENT,
                KANIDM_OUTCOME_CHANGED,
                self.create_reset_token(&kanidm_client, name, ctx.clone()),
            )
            .await?;
            self.update_credential_bootstrap_status(
                ctx.clone(),
                CredentialBootstrapStatus {
                    state: CredentialBootstrapState::TokenIssued,
                    token_expires_at: Some(token_expires_at),
                    delivery_method: Some(CredentialBootstrapDelivery::Event),
                    last_queued_resend_revision: status
                        .credential_bootstrap
                        .last_queued_resend_revision,
                    email_queued_at: status.credential_bootstrap.email_queued_at.clone(),
                    ..Default::default()
                },
            )
            .await?;
            changed = true;
        }

        if require_status_update {
            debug!(actions = ?actions, "requeueing in 500ms after actions");
            Ok((Action::requeue(Duration::from_millis(500)), changed))
        } else {
            debug!("reconciliation complete, requeueing for next interval");
            Ok((Action::requeue(idm_reconcile_interval()), changed))
        }
    }

    async fn create(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!("create");
        kanidm_client
            .idm_person_account_create(name, &self.spec.person_attributes.displayname)
            .await
            .map_err(|e| {
                Error::kanidm_client_error(
                    "create",
                    name,
                    self.kanidm_namespace(),
                    self.kanidm_name(),
                    e,
                )
            })?;
        Ok(())
    }

    async fn update(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        metrics: &kaniop_operator::metrics::ControllerMetrics,
    ) -> Result<()> {
        debug!("update");
        trace!(person_attributes = ?self.spec.person_attributes, "updating person attributes");
        record_kanidm_sdk_call(
            metrics,
            KANIDM_RESOURCE_PERSON,
            KANIDM_OP_UPDATE,
            KANIDM_OUTCOME_CHANGED,
            kanidm_client.idm_person_account_update(
                name,
                None,
                Some(&self.spec.person_attributes.displayname),
                self.spec.person_attributes.legalname.as_deref(),
                self.spec.person_attributes.mail.as_deref(),
            ),
        )
        .await
        .map_err(|e| {
            Error::kanidm_client_error(
                "update",
                name,
                self.kanidm_namespace(),
                self.kanidm_name(),
                e,
            )
        })?;
        let mut update_entry = Entry {
            attrs: BTreeMap::new(),
        };
        if let Some(account_expire) = self.spec.person_attributes.account_expire.as_ref() {
            update_entry.attrs.insert(
                ATTR_ACCOUNT_EXPIRE.to_string(),
                vec![account_expire.0.to_string()],
            );
        }
        if let Some(account_valid_from) = self.spec.person_attributes.account_valid_from.as_ref() {
            update_entry.attrs.insert(
                ATTR_ACCOUNT_VALID_FROM.to_string(),
                vec![account_valid_from.0.to_string()],
            );
        }

        if update_entry.attrs.is_empty().not() {
            let _: Entry = record_kanidm_sdk_call(
                metrics,
                KANIDM_RESOURCE_PERSON,
                KANIDM_OP_UPDATE,
                KANIDM_OUTCOME_CHANGED,
                kanidm_client.perform_patch_request(&format!("/v1/person/{name}"), update_entry),
            )
            .await
            .map_err(|e| {
                Error::kanidm_client_error(
                    "update",
                    name,
                    self.kanidm_namespace(),
                    self.kanidm_name(),
                    e,
                )
            })?;
        }
        Ok(())
    }

    async fn sync_ssh_public_keys(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        let Some(desired) = &self.spec.ssh_public_keys else {
            return Ok(());
        };
        debug!("syncing SSH public keys");
        let observed = read_ssh_public_keys(kanidm_client, name).await?;
        // Use the original cached status (self.status), not the computed status from update_status.
        // This ensures we have the tags from before update_status patched them.
        let managed: BTreeSet<String> = self
            .status
            .as_ref()
            .map(|s| s.managed_ssh_public_key_tags.iter().cloned().collect())
            .unwrap_or_default();

        trace!(
            desired_tags = ?desired.keys().collect::<Vec<_>>(),
            observed_tags = ?observed.keys().collect::<Vec<_>>(),
            managed_tags = ?managed,
            "SSH key state"
        );

        // Revoke keys that are in observed but not in desired.
        // We revoke any key with a tag that was previously managed (in status) or is currently
        // in observed but not in desired. This handles both the case where the operator
        // previously managed the key and the case where the key exists in Kanidm but the
        // spec no longer includes it.
        for tag in observed
            .keys()
            .filter(|tag| !desired.contains_key(*tag))
            .cloned()
            .collect::<Vec<_>>()
        {
            // Only revoke if the tag was previously managed or is in the current managed set.
            // This prevents revoking keys that were added manually and never managed by the operator.
            if managed.contains(&tag) {
                debug!(tag = %tag, "revoking managed SSH public key");
                kanidm_client
                    .idm_person_account_delete_ssh_pubkey(name, &tag)
                    .await
                    .map_err(|e| {
                        Error::kanidm_client_error_attr(
                            "delete",
                            "SSH public key",
                            name,
                            self.kanidm_namespace(),
                            self.kanidm_name(),
                            e,
                        )
                    })?;
            }
        }

        for (tag, key) in desired {
            if observed.get(tag) != Some(key) {
                // Kanidm does not overwrite an existing tag on insertion.
                // Rotation therefore requires remove + add (not atomic).
                if observed.contains_key(tag) {
                    debug!(tag = %tag, "rotating SSH public key (delete existing)");
                    kanidm_client
                        .idm_person_account_delete_ssh_pubkey(name, tag)
                        .await
                        .map_err(|e| {
                            Error::kanidm_client_error_attr(
                                "delete",
                                "SSH public key",
                                name,
                                self.kanidm_namespace(),
                                self.kanidm_name(),
                                e,
                            )
                        })?;
                }
                // Let Kanidm validate the entire SSH key; do not parse it here.
                debug!(tag = %tag, "adding SSH public key");
                kanidm_client
                    .idm_person_account_post_ssh_pubkey(name, tag, key)
                    .await
                    .map_err(|e| {
                        Error::kanidm_client_error_attr(
                            "add",
                            "SSH public key",
                            name,
                            self.kanidm_namespace(),
                            self.kanidm_name(),
                            e,
                        )
                    })?;
            }
        }
        Ok(())
    }

    async fn update_posix_attributes(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
    ) -> Result<()> {
        debug!("updating posix attributes");
        trace!(posix_attributes = ?self.spec.posix_attributes, "updating posix attributes");
        kanidm_client
            .idm_person_account_unix_extend(
                name,
                self.spec
                    .posix_attributes
                    .as_ref()
                    .and_then(|posix| posix.gidnumber),
                self.spec
                    .posix_attributes
                    .as_ref()
                    .and_then(|posix| posix.loginshell.as_deref()),
            )
            .await
            .map_err(|e| {
                Error::kanidm_client_error(
                    "update",
                    name,
                    self.kanidm_namespace(),
                    self.kanidm_name(),
                    e,
                )
            })?;
        Ok(())
    }

    async fn create_reset_token(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        ctx: Arc<Context>,
    ) -> Result<Time> {
        debug!("create reset token");
        let cu_token = kanidm_client
            .idm_person_account_credential_update_intent(
                name,
                Some(self.spec.credentials_token_ttl),
            )
            .await
            .map_err(|e| {
                Error::kanidm_client_error(
                    "create a credential reset token for",
                    name,
                    self.kanidm_namespace(),
                    self.kanidm_name(),
                    e,
                )
            })?;
        let token = cu_token.token.as_str();
        let url = if let Some(base_url) = ctx.kaniop_ctx.get_kanidm(self).map(|k| {
            k.spec
                .origin
                .clone()
                .unwrap_or_else(|| format!("https://{}", k.spec.domain))
        }) {
            format!("{base_url}/ui/reset?token={token}")
        } else {
            let mut url = kanidm_client.make_url("/ui/reset");
            url.query_pairs_mut().append_pair("token", token);
            url.to_string()
        };
        let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        let expiry_time = cu_token.expiry_time.to_offset(local_offset);

        let msg = format!(
            "Update these user credentials with this link: {url}. This token will expire at: {}",
            expiry_time
                .format(&Rfc3339)
                .unwrap_or_else(|_| expiry_time.to_string())
        );
        ctx.kaniop_ctx
            .recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "TokenCreated".to_string(),
                    note: Some(msg),
                    action: "CreateUpdateCredentialsToken".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(|e| {
                warn!(error = %e, "failed to publish TokenCreated event");
                Error::kube_error("publish", "event", self.get_namespace(), self.name_any(), e)
            })?;

        let expiry_timestamp = Timestamp::from_second(cu_token.expiry_time.unix_timestamp())
            .map_err(|e| Error::ParseError(format!("failed to convert token expiry: {e}")))?;
        Ok(Time(expiry_timestamp))
    }

    async fn record_bootstrap_delivery_error(
        &self,
        previous: &CredentialBootstrapStatus,
        reason: &str,
        ctx: Arc<Context>,
    ) -> Result<()> {
        if previous.delivery_error.as_deref() == Some(reason) {
            return Ok(());
        }
        let mut bootstrap = previous.clone();
        bootstrap.delivery_error = Some(reason.to_string());
        self.update_credential_bootstrap_status(ctx.clone(), bootstrap)
            .await?;
        ctx.kaniop_ctx
            .recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: reason.to_string(),
                    note: Some(format!("Credential email bootstrap blocked: {reason}")),
                    action: "QueueCredentialBootstrapEmail".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(|e| {
                Error::kube_error("publish", "event", self.get_namespace(), self.name_any(), e)
            })?;
        Ok(())
    }

    async fn update_credential_bootstrap_status(
        &self,
        ctx: Arc<Context>,
        bootstrap: CredentialBootstrapStatus,
    ) -> Result<()> {
        let namespace = self.get_namespace();
        let person_api =
            Api::<KanidmPersonAccount>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
        person_api
            .patch_status(
                &self.name_any(),
                &PatchParams::default(),
                &Patch::Merge(credential_bootstrap_merge_patch(&bootstrap)),
            )
            .await
            .map_err(|e| {
                Error::kube_status_error("KanidmPersonAccount", namespace, self.name_any(), e)
            })?;
        Ok(())
    }

    async fn cleanup(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmPersonAccountStatus,
        ctx: Arc<Context>,
    ) -> Result<(Action, bool)> {
        let name = &self.kanidm_entity_name();
        let mut changed = false;

        if is_person(TYPE_EXISTS, status.clone()) {
            debug!("delete");
            record_kanidm_sdk_call(
                &ctx.kaniop_ctx.metrics,
                KANIDM_RESOURCE_PERSON,
                KANIDM_OP_DELETE,
                KANIDM_OUTCOME_CHANGED,
                kanidm_client.idm_person_account_delete(name),
            )
            .await
            .map_err(|e| {
                Error::kanidm_client_error(
                    "delete",
                    name,
                    self.kanidm_namespace(),
                    self.kanidm_name(),
                    e,
                )
            })?;
            changed = true;
        }
        Ok((Action::requeue(idm_reconcile_interval()), changed))
    }

    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context>,
    ) -> Result<KanidmPersonAccountStatus> {
        // safe unwrap: person is namespaced scoped
        let namespace = self.get_namespace();
        let name = self.kanidm_entity_name();
        let metrics = &ctx.kaniop_ctx.metrics;
        let start = tokio::time::Instant::now();
        let current_person = kanidm_client
            .idm_person_account_get(&name)
            .await
            .map_err(|e| {
                let elapsed = start.elapsed();
                metrics.record_kanidm_sdk_outcome(
                    KANIDM_RESOURCE_PERSON,
                    KANIDM_OP_GET,
                    KANIDM_OUTCOME_ERROR,
                    elapsed,
                );
                Error::kanidm_client_error(
                    "get",
                    name.as_str(),
                    self.kanidm_namespace(),
                    self.kanidm_name(),
                    e,
                )
            })?;
        metrics.record_kanidm_sdk_outcome(
            KANIDM_RESOURCE_PERSON,
            KANIDM_OP_GET,
            KANIDM_OUTCOME_UNCHANGED,
            start.elapsed(),
        );

        let credential_state = if current_person.is_some() {
            let cred_start = tokio::time::Instant::now();
            let probe = tokio::try_join!(
                kanidm_client.idm_person_account_get_attr(&name, ATTR_PRIMARY_CREDENTIAL),
                kanidm_client.idm_person_account_get_attr(&name, ATTR_PASSKEYS),
                kanidm_client.idm_person_account_get_attr(&name, ATTR_ATTESTED_PASSKEYS),
            );
            match probe {
                Ok((primary, passkeys, attested_passkeys)) => {
                    let observed_state =
                        credential_state_from_attributes(&primary, &passkeys, &attested_passkeys);
                    let state = if observed_state == CredentialState::Present {
                        CredentialState::Present
                    } else {
                        match credential_attributes_searchable(&kanidm_client, &name).await {
                            Ok(true) => CredentialState::Absent,
                            Ok(false) => {
                                warn!(
                                    "credential attributes are not searchable; leaving state unknown"
                                );
                                CredentialState::Unknown
                            }
                            Err(e) => {
                                warn!(
                                    error = ?e,
                                    "credential effective-access probe failed; leaving state unknown"
                                );
                                CredentialState::Unknown
                            }
                        }
                    };
                    trace!(
                        credential_state = ?state,
                        primary_present = primary.as_ref().is_some_and(|v| v.is_empty().not()),
                        passkeys_count = passkeys.as_ref().map_or(0, Vec::len),
                        attested_passkeys_count = attested_passkeys.as_ref().map_or(0, Vec::len),
                        "read-only credential status"
                    );
                    metrics.record_kanidm_sdk_outcome(
                        KANIDM_RESOURCE_PERSON,
                        KANIDM_OP_GET_CREDENTIAL_STATUS,
                        if state == CredentialState::Unknown {
                            KANIDM_OUTCOME_ERROR
                        } else {
                            KANIDM_OUTCOME_UNCHANGED
                        },
                        cred_start.elapsed(),
                    );
                    state
                }
                Err(e) => {
                    warn!(
                        error = ?e,
                        "read-only credential status probe failed; leaving state unknown"
                    );
                    metrics.record_kanidm_sdk_outcome(
                        KANIDM_RESOURCE_PERSON,
                        KANIDM_OP_GET_CREDENTIAL_STATUS,
                        KANIDM_OUTCOME_ERROR,
                        cred_start.elapsed(),
                    );
                    CredentialState::Unknown
                }
            }
        } else {
            CredentialState::Unknown
        };

        let ssh_public_keys = if current_person.is_some() && self.spec.ssh_public_keys.is_some() {
            Some(read_ssh_public_keys(&kanidm_client, &name).await?)
        } else {
            None
        };

        let status = self.generate_status(current_person, credential_state, ssh_public_keys)?;
        if self.status.as_ref() == Some(&status) {
            trace!("status unchanged, skipping patch");
            return Ok(status);
        }
        if let Some(old) = self.status.as_ref() {
            let old_conds: Vec<_> = old
                .conditions
                .iter()
                .flatten()
                .map(|c| format!("{}={}", c.type_, c.status))
                .collect();
            let new_conds: Vec<_> = status
                .conditions
                .iter()
                .flatten()
                .map(|c| format!("{}={}", c.type_, c.status))
                .collect();
            debug!(
                old = ?old_conds,
                new = ?new_conds,
                old_ready = old.ready,
                new_ready = status.ready,
                old_credential_state = ?old.credential_state,
                new_credential_state = ?status.credential_state,
                "status changed"
            );
        } else {
            debug!(conditions = ?status.conditions, "initial status patch");
        }
        let status_patch = Patch::Apply(KanidmPersonAccount {
            status: Some(status.clone()),
            ..KanidmPersonAccount::default()
        });
        debug!("updating status");
        trace!(status_patch = ?status_patch, "status patch");
        let patch = PatchParams::apply(PERSON_OPERATOR_NAME).force();
        let kanidm_api =
            Api::<KanidmPersonAccount>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
        let _o = kanidm_api
            .patch_status(&self.name_any(), &patch, &status_patch)
            .await
            .map_err(|e| {
                Error::kube_status_error("KanidmPersonAccount", namespace, self.name_any(), e)
            })?;
        Ok(status)
    }

    fn generate_status(
        &self,
        person: Option<Entry>,
        credential_state: CredentialState,
        ssh_public_keys: Option<BTreeMap<String, String>>,
    ) -> Result<KanidmPersonAccountStatus> {
        let now = Timestamp::now();
        let current_conditions = self.status.as_ref().and_then(|s| s.conditions.as_ref());
        let managed_ssh_public_key_tags = if let Some(desired) = &self.spec.ssh_public_keys {
            // Tags this operator currently owns: union of:
            // 1. Tags that were previously managed and are still in observed (need revocation if not in desired)
            // 2. Tags that are in desired and match observed (new or existing managed tags)
            // This ensures managed_tags includes tags pending revocation, preventing the SSH condition
            // from becoming True prematurely and blocking sync_ssh_public_keys.
            if let Some(observed) = ssh_public_keys.as_ref() {
                let old_managed: BTreeSet<String> = self
                    .status
                    .as_ref()
                    .map(|s| s.managed_ssh_public_key_tags.iter().cloned().collect())
                    .unwrap_or_default();

                // Tags that were previously managed and are still in observed
                // (these need to be revoked if not in desired)
                let still_in_observed: BTreeSet<String> = old_managed
                    .iter()
                    .filter(|tag| observed.contains_key(*tag))
                    .cloned()
                    .collect();

                // Tags that are in desired and match observed (new or existing managed tags)
                let new_managed: BTreeSet<String> = desired
                    .iter()
                    .filter(|(tag, key)| observed.get(*tag).map(String::as_str) == Some(key))
                    .map(|(tag, _)| tag.clone())
                    .collect();

                still_in_observed.union(&new_managed).cloned().collect()
            } else {
                Vec::new()
            }
        } else {
            // Removing the field opts out of management without revoking keys.
            Vec::new()
        };

        match person {
            Some(p) => {
                let exist_condition = Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_TRUE.to_string(),
                    reason: "Exists".to_string(),
                    message: "Person exists.".to_string(),
                    last_transition_time: last_transition_time(
                        current_conditions,
                        TYPE_EXISTS,
                        CONDITION_TRUE,
                        "Exists",
                    ),
                    observed_generation: self.metadata.generation,
                };

                let current_person_attributes = KanidmPersonAttributes::from(p.clone());
                let updated_condition = if self.spec.person_attributes == current_person_attributes
                {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: REASON_ATTRIBUTES_MATCH.to_string(),
                        message: "Person exists with desired attributes.".to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_UPDATED,
                            CONDITION_TRUE,
                            REASON_ATTRIBUTES_MATCH,
                        ),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    let spec = &self.spec.person_attributes;
                    debug!(
                        displayname_spec = %spec.displayname,
                        displayname_actual = %current_person_attributes.displayname,
                        mail_spec = ?spec.mail,
                        mail_actual = ?current_person_attributes.mail,
                        legalname_spec = ?spec.legalname,
                        legalname_actual = ?current_person_attributes.legalname,
                        account_valid_from_spec = ?spec.account_valid_from,
                        account_valid_from_actual = ?current_person_attributes.account_valid_from,
                        account_expire_spec = ?spec.account_expire,
                        account_expire_actual = ?current_person_attributes.account_expire,
                        "person attributes mismatch"
                    );
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                        message: "Person exists with different attributes.".to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_UPDATED,
                            CONDITION_FALSE,
                            REASON_ATTRIBUTES_NOT_MATCH,
                        ),
                        observed_generation: self.metadata.generation,
                    }
                };

                let current_person_posix = KanidmAccountPosixAttributes::from(p);
                let posix_initialized_condition = if current_person_posix.gidnumber.is_some() {
                    Condition {
                        type_: TYPE_POSIX_INITIALIZED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "PosixInitialized".to_string(),
                        message: "Person exists with POSIX attributes.".to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_POSIX_INITIALIZED,
                            CONDITION_TRUE,
                            "PosixInitialized",
                        ),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_POSIX_INITIALIZED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: "PosixNotInitialized".to_string(),
                        message: "Person exists without POSIX attributes.".to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_POSIX_INITIALIZED,
                            CONDITION_FALSE,
                            "PosixNotInitialized",
                        ),
                        observed_generation: self.metadata.generation,
                    }
                };

                let posix_updated_condition = self.spec.posix_attributes.as_ref().map(|posix| {
                    if posix == &current_person_posix {
                        Condition {
                            type_: TYPE_POSIX_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTES_MATCH.to_string(),
                            message: "Person exists with desired POSIX attributes.".to_string(),
                            last_transition_time: last_transition_time(
                                current_conditions,
                                TYPE_POSIX_UPDATED,
                                CONDITION_TRUE,
                                REASON_ATTRIBUTES_MATCH,
                            ),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        debug!(
                            gidnumber_spec = ?posix.gidnumber,
                            gidnumber_actual = ?current_person_posix.gidnumber,
                            loginshell_spec = ?posix.loginshell,
                            loginshell_actual = ?current_person_posix.loginshell,
                            "posix attributes mismatch"
                        );
                        Condition {
                            type_: TYPE_POSIX_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                            message: "Person exists with different POSIX attributes.".to_string(),
                            last_transition_time: last_transition_time(
                                current_conditions,
                                TYPE_POSIX_UPDATED,
                                CONDITION_FALSE,
                                REASON_ATTRIBUTES_NOT_MATCH,
                            ),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

                let (credential_condition_status, credential_reason, credential_message) =
                    match credential_state {
                        CredentialState::Present => {
                            (CONDITION_TRUE, "Present", "Credentials are present.")
                        }
                        CredentialState::Absent => (
                            CONDITION_FALSE,
                            "NotPresent",
                            "Credentials are not present.",
                        ),
                        CredentialState::Unknown => (
                            CONDITION_UNKNOWN,
                            "DetectionFailed",
                            "Credential presence could not be determined.",
                        ),
                    };
                let credentials_condition = Condition {
                    type_: TYPE_CREDENTIAL.to_string(),
                    status: credential_condition_status.to_string(),
                    reason: credential_reason.to_string(),
                    message: credential_message.to_string(),
                    last_transition_time: last_transition_time(
                        current_conditions,
                        TYPE_CREDENTIAL,
                        credential_condition_status,
                        credential_reason,
                    ),
                    observed_generation: self.metadata.generation,
                };

                let mut credential_bootstrap = self
                    .status
                    .as_ref()
                    .map(|status| status.credential_bootstrap.clone())
                    .unwrap_or_default();
                if credential_state == CredentialState::Present {
                    credential_bootstrap.state = CredentialBootstrapState::Complete;
                    credential_bootstrap.token_expires_at = None;
                    credential_bootstrap.delivery_error = None;
                }

                let validity_condition = {
                    let valid = if let Some(valid_from) =
                        current_person_attributes.account_valid_from.as_ref()
                    {
                        now > valid_from.0
                    } else {
                        true
                    } && if let Some(expire) =
                        current_person_attributes.account_expire.as_ref()
                    {
                        now < expire.0
                    } else {
                        true
                    };

                    if valid {
                        Condition {
                            type_: TYPE_VALIDITY.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: "Valid".to_string(),
                            message: "Account is valid.".to_string(),
                            last_transition_time: last_transition_time(
                                current_conditions,
                                TYPE_VALIDITY,
                                CONDITION_TRUE,
                                "Valid",
                            ),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_VALIDITY.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: "Invalid".to_string(),
                            message: "Account is invalid.".to_string(),
                            last_transition_time: last_transition_time(
                                current_conditions,
                                TYPE_VALIDITY,
                                CONDITION_FALSE,
                                "Invalid",
                            ),
                            observed_generation: self.metadata.generation,
                        }
                    }
                };

                let old_managed_ssh_public_key_tags: Vec<String> = self
                    .status
                    .as_ref()
                    .map(|s| s.managed_ssh_public_key_tags.clone())
                    .unwrap_or_default();

                let ssh_public_keys_condition = if let Some(desired) = &self.spec.ssh_public_keys {
                    let observed = ssh_public_keys.as_ref().ok_or_else(|| {
                        Error::MissingData("SSH key state was not fetched from Kanidm".to_string())
                    })?;
                    let matches =
                        ssh_public_keys_match(desired, observed, &old_managed_ssh_public_key_tags);
                    let (condition_status, reason, message) = if matches {
                        (
                            CONDITION_TRUE,
                            REASON_ATTRIBUTES_MATCH,
                            "Person has the desired managed SSH public keys.",
                        )
                    } else {
                        (
                            CONDITION_FALSE,
                            REASON_ATTRIBUTES_NOT_MATCH,
                            "Person SSH public keys differ from the managed keys.",
                        )
                    };
                    Some(Condition {
                        type_: TYPE_SSH_PUBLIC_KEYS.to_string(),
                        status: condition_status.to_string(),
                        reason: reason.to_string(),
                        message: message.to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_SSH_PUBLIC_KEYS,
                            condition_status,
                            reason,
                        ),
                        observed_generation: self.metadata.generation,
                    })
                } else {
                    None
                };

                let delivery_condition = (self.spec.credential_bootstrap_delivery
                    == CredentialBootstrapDelivery::Email)
                    .then(|| {
                        let (condition_status, reason, message) = bootstrap_email_condition(
                            credential_state,
                            &credential_bootstrap,
                            requested_bootstrap_resend_revision(self),
                        );
                        Condition {
                            type_: TYPE_CREDENTIAL_BOOTSTRAP_DELIVERY.to_string(),
                            status: condition_status.to_string(),
                            reason: reason.to_string(),
                            message: message.to_string(),
                            last_transition_time: last_transition_time(
                                current_conditions,
                                TYPE_CREDENTIAL_BOOTSTRAP_DELIVERY,
                                condition_status,
                                reason,
                            ),
                            observed_generation: self.metadata.generation,
                        }
                    });

                let conditions = vec![
                    exist_condition,
                    updated_condition,
                    posix_initialized_condition,
                    validity_condition,
                    credentials_condition,
                ]
                .into_iter()
                .chain(posix_updated_condition)
                .chain(ssh_public_keys_condition)
                .chain(delivery_condition)
                .collect::<Vec<_>>();
                let status = conditions
                    .iter()
                    .filter(|c| {
                        c.type_ != TYPE_POSIX_INITIALIZED
                            && c.type_ != TYPE_CREDENTIAL
                            && c.type_ != TYPE_VALIDITY
                    })
                    .all(|c| c.status == CONDITION_TRUE);
                Ok(KanidmPersonAccountStatus {
                    conditions: Some(conditions),
                    ready: status,
                    gid: current_person_posix.gidnumber,
                    kanidm_ref: self.kanidm_ref(),
                    credential_state,
                    credential_bootstrap,
                    managed_ssh_public_key_tags,
                })
            }
            None => {
                let conditions = vec![Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_FALSE.to_string(),
                    reason: "NotExists".to_string(),
                    message: "Person is not present.".to_string(),
                    last_transition_time: last_transition_time(
                        current_conditions,
                        TYPE_EXISTS,
                        CONDITION_FALSE,
                        "NotExists",
                    ),
                    observed_generation: self.metadata.generation,
                }];
                Ok(KanidmPersonAccountStatus {
                    conditions: Some(conditions),
                    ready: false,
                    gid: None,
                    kanidm_ref: self.kanidm_ref(),
                    credential_state: CredentialState::Unknown,
                    credential_bootstrap: CredentialBootstrapStatus::default(),
                    managed_ssh_public_key_tags,
                })
            }
        }
    }
}

pub fn is_person(type_: &str, status: KanidmPersonAccountStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_TRUE)
}

pub fn is_person_false(type_: &str, status: KanidmPersonAccountStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_FALSE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_merge_patch_explicitly_clears_optional_fields() {
        let patch = credential_bootstrap_merge_patch(&CredentialBootstrapStatus::default());
        let status = &patch["status"]["credentialBootstrap"];
        assert_eq!(status["state"], "pending");
        for field in [
            "tokenExpiresAt",
            "deliveryMethod",
            "lastQueuedResendRevision",
            "emailQueuedAt",
            "deliveryError",
        ] {
            assert_eq!(
                status.get(field),
                Some(&serde_json::Value::Null),
                "missing explicit null for {field}"
            );
        }

        let patch = credential_bootstrap_merge_patch(&CredentialBootstrapStatus {
            state: CredentialBootstrapState::TokenIssued,
            delivery_method: Some(CredentialBootstrapDelivery::Email),
            last_queued_resend_revision: Some(1),
            delivery_error: Some("MissingEmail".into()),
            ..Default::default()
        });
        let status = &patch["status"]["credentialBootstrap"];
        assert_eq!(status["state"], "tokenIssued");
        assert_eq!(status["deliveryMethod"], "email");
        assert_eq!(status["lastQueuedResendRevision"], 1);
        assert_eq!(status["deliveryError"], "MissingEmail");
    }

    #[test]
    fn email_bootstrap_only_queues_initial_email_and_higher_annotated_revisions() {
        let mut person = KanidmPersonAccount::default();
        assert_eq!(requested_bootstrap_resend_revision(&person), Some(0));
        person.metadata.annotations = Some(std::collections::BTreeMap::from([(
            ANNOTATION_CREDENTIAL_BOOTSTRAP_RESEND_REVISION.to_string(),
            "2".to_string(),
        )]));
        assert_eq!(requested_bootstrap_resend_revision(&person), Some(2));
        person.metadata.annotations.as_mut().unwrap().insert(
            ANNOTATION_CREDENTIAL_BOOTSTRAP_RESEND_REVISION.to_string(),
            "invalid".to_string(),
        );
        assert_eq!(requested_bootstrap_resend_revision(&person), None);

        let mut status = CredentialBootstrapStatus::default();
        assert!(should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(0)
        ));
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            None
        ));
        assert!(!should_send_bootstrap_email(
            CredentialState::Unknown,
            &status,
            Some(0)
        ));
        assert!(!should_send_bootstrap_email(
            CredentialState::Present,
            &status,
            Some(0)
        ));

        // The first switch from Events to Email still queues the initial invite.
        status.state = CredentialBootstrapState::TokenIssued;
        status.delivery_method = Some(CredentialBootstrapDelivery::Event);
        assert!(should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(0)
        ));
        status.delivery_method = Some(CredentialBootstrapDelivery::Email);
        status.last_queued_resend_revision = Some(0);
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(0)
        ));
        assert!(should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(1)
        ));

        status.last_queued_resend_revision = Some(2);
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(2)
        ));
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(1)
        ));
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(0)
        ));
        assert!(should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(3)
        ));

        // Delivery-mode switches are not resend requests.
        status.delivery_method = Some(CredentialBootstrapDelivery::Event);
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(0)
        ));
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(2)
        ));
        assert!(should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(3)
        ));
        status.state = CredentialBootstrapState::Complete;
        assert!(!should_send_bootstrap_email(
            CredentialState::Absent,
            &status,
            Some(3)
        ));
    }

    #[test]
    fn stale_resend_does_not_make_account_unready_after_failed_newer_request() {
        let status = CredentialBootstrapStatus {
            state: CredentialBootstrapState::TokenIssued,
            delivery_method: Some(CredentialBootstrapDelivery::Email),
            last_queued_resend_revision: Some(2),
            delivery_error: Some("MailSenderNotConfigured".to_string()),
            ..Default::default()
        };
        let stale = bootstrap_email_condition(CredentialState::Absent, &status, Some(1));
        assert_eq!((stale.0, stale.1), (CONDITION_TRUE, "StaleResendIgnored"));

        let already_sent = bootstrap_email_condition(CredentialState::Absent, &status, Some(2));
        assert_eq!(
            (already_sent.0, already_sent.1),
            (CONDITION_TRUE, "EmailQueued")
        );

        let pending_failure = bootstrap_email_condition(CredentialState::Absent, &status, Some(3));
        assert_eq!(
            (pending_failure.0, pending_failure.1),
            (CONDITION_FALSE, "MailSenderNotConfigured")
        );
        let invalid = bootstrap_email_condition(CredentialState::Absent, &status, None);
        assert_eq!(
            (invalid.0, invalid.1),
            (CONDITION_FALSE, "InvalidResendRevision")
        );
    }

    #[test]
    fn event_mode_renews_after_expiry_and_switches_back_from_email() {
        let mut status = CredentialBootstrapStatus::default();
        assert!(should_create_event_token(CredentialState::Absent, &status));
        status.state = CredentialBootstrapState::TokenIssued;
        status.delivery_method = Some(CredentialBootstrapDelivery::Email);
        assert!(should_create_event_token(CredentialState::Absent, &status));
        status.delivery_method = Some(CredentialBootstrapDelivery::Event);
        assert!(!should_create_event_token(CredentialState::Absent, &status));
        status.token_expires_at = Some(Time(Timestamp::from_second(1_600_000_000).unwrap()));
        assert!(should_create_event_token(CredentialState::Absent, &status));
        status.state = CredentialBootstrapState::Complete;
        assert!(!should_create_event_token(CredentialState::Absent, &status));
    }

    #[test]
    fn no_credential_attributes_is_absent() {
        assert_eq!(
            credential_state_from_attributes(&None, &None, &None),
            CredentialState::Absent
        );
    }

    #[test]
    fn primary_credential_is_present() {
        assert_eq!(
            credential_state_from_attributes(&Some(vec!["primary".to_string()]), &None, &None,),
            CredentialState::Present
        );
    }

    #[test]
    fn passkey_only_is_present() {
        assert_eq!(
            credential_state_from_attributes(&None, &Some(vec!["passkey".to_string()]), &None,),
            CredentialState::Present
        );
    }

    #[test]
    fn attested_passkey_only_is_present() {
        assert_eq!(
            credential_state_from_attributes(
                &None,
                &None,
                &Some(vec!["attested-passkey".to_string()]),
            ),
            CredentialState::Present
        );
    }

    #[test]
    fn scim_grant_allows_all_credential_attributes() {
        assert!(scim_search_allows_all_credential_attributes(
            &serde_json::json!("Grant")
        ));
    }

    #[test]
    fn scim_allow_requires_all_credential_attributes() {
        assert!(scim_search_allows_all_credential_attributes(
            &serde_json::json!({
                "Allow": [
                    ATTR_PRIMARY_CREDENTIAL,
                    ATTR_PASSKEYS,
                    ATTR_ATTESTED_PASSKEYS
                ]
            })
        ));
        assert!(!scim_search_allows_all_credential_attributes(
            &serde_json::json!({
                "Allow": [ATTR_PRIMARY_CREDENTIAL, ATTR_PASSKEYS]
            })
        ));
    }

    #[test]
    fn scim_deny_or_malformed_access_is_not_searchable() {
        assert!(!scim_search_allows_all_credential_attributes(
            &serde_json::json!("Deny")
        ));
        assert!(!scim_search_allows_all_credential_attributes(
            &serde_json::json!({})
        ));
    }

    #[test]
    fn unknown_credential_state_never_creates_reset_token() {
        assert!(!should_create_reset_token(
            CredentialState::Unknown,
            &CredentialBootstrapStatus::default(),
        ));
    }

    #[test]
    fn present_credential_state_never_creates_reset_token() {
        assert!(!should_create_reset_token(
            CredentialState::Present,
            &CredentialBootstrapStatus::default(),
        ));
    }

    #[test]
    fn absent_pending_bootstrap_creates_reset_token() {
        assert!(should_create_reset_token(
            CredentialState::Absent,
            &CredentialBootstrapStatus::default(),
        ));
    }

    #[test]
    fn complete_bootstrap_never_creates_reset_token() {
        assert!(!should_create_reset_token(
            CredentialState::Absent,
            &CredentialBootstrapStatus {
                state: CredentialBootstrapState::Complete,
                token_expires_at: None,
                ..Default::default()
            },
        ));
    }

    #[test]
    fn ssh_key_matching_tracks_ownership_without_touching_unmanaged_tags() {
        let desired = BTreeMap::from([("laptop".to_string(), "key-1".to_string())]);
        let observed = BTreeMap::from([
            ("laptop".to_string(), "key-1".to_string()),
            ("personal".to_string(), "key-2".to_string()),
        ]);

        // Same public key does not imply ownership before it is adopted.
        assert!(!ssh_public_keys_match(&desired, &observed, &[]));
        assert!(ssh_public_keys_match(
            &desired,
            &observed,
            &["laptop".to_string()]
        ));

        // A previously managed but no longer desired tag requires a cleanup.
        assert!(!ssh_public_keys_match(
            &BTreeMap::new(),
            &observed,
            &["laptop".to_string()],
        ));
        assert!(ssh_public_keys_match(&BTreeMap::new(), &observed, &[]));

        let mut changed = observed;
        changed.insert("laptop".to_string(), "key-rotated".to_string());
        assert!(!ssh_public_keys_match(
            &desired,
            &changed,
            &["laptop".to_string()],
        ));
    }
}
