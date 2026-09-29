use crate::controller::Context;
use crate::crd::{
    CredentialBootstrapState, CredentialBootstrapStatus, CredentialState, KanidmPersonAccount,
    KanidmPersonAccountStatus, KanidmPersonAttributes,
};

use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::resources::last_transition_time;
use kaniop_operator::controller::kanidm::{KanidmResource, is_resource_watched};
use kaniop_operator::controller::{context::IdmClientContext, idm_reconcile_interval};
use kaniop_operator::crd::KanidmAccountPosixAttributes;
use kaniop_operator::metrics::{
    KANIDM_OP_CREATE, KANIDM_OP_CREDENTIAL_UPDATE_INTENT, KANIDM_OP_DELETE, KANIDM_OP_GET,
    KANIDM_OP_GET_CREDENTIAL_STATUS, KANIDM_OP_UNIX_EXTEND, KANIDM_OP_UPDATE,
    KANIDM_OUTCOME_CHANGED, KANIDM_OUTCOME_ERROR, KANIDM_OUTCOME_UNCHANGED, KANIDM_RESOURCE_PERSON,
    record_kanidm_sdk_call,
};
use kaniop_operator::telemetry;

use std::collections::BTreeMap;
use std::ops::Not;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kanidm_client::KanidmClient;
use kanidm_proto::attribute::Attribute;
use kanidm_proto::constants::{
    ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM, ATTR_ATTESTED_PASSKEYS, ATTR_PASSKEYS,
    ATTR_PRIMARY_CREDENTIAL,
};
use kanidm_proto::scim_v1::ScimEntryGetQuery;
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

        if should_create_reset_token(status.credential_state, &status.credential_bootstrap) {
            debug!(
                credential_state = ?status.credential_state,
                bootstrap_state = ?status.credential_bootstrap.state,
                action = "create_reset_token",
                "credential bootstrap triggered action"
            );
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
                &Patch::Merge(serde_json::json!({
                    "status": {
                        "credentialBootstrap": bootstrap,
                    }
                })),
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

        let status = self.generate_status(current_person, credential_state)?;
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
    ) -> Result<KanidmPersonAccountStatus> {
        let now = Timestamp::now();
        let current_conditions = self.status.as_ref().and_then(|s| s.conditions.as_ref());

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

                let credential_bootstrap = if credential_state == CredentialState::Present {
                    CredentialBootstrapStatus {
                        state: CredentialBootstrapState::Complete,
                        token_expires_at: None,
                    }
                } else {
                    self.status
                        .as_ref()
                        .map(|status| status.credential_bootstrap.clone())
                        .unwrap_or_default()
                };

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
                let conditions = vec![
                    exist_condition,
                    updated_condition,
                    posix_initialized_condition,
                    validity_condition,
                    credentials_condition,
                ]
                .into_iter()
                .chain(posix_updated_condition)
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
            },
        ));
    }
}
