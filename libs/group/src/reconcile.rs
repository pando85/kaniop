use crate::crd::{
    KanidmGroup, KanidmGroupAccountPolicyAttributes, KanidmGroupPosixAttributes, KanidmGroupStatus,
};

use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::types::{compare_names, get_first_cloned, normalize_spn};
use kaniop_operator::controller::kanidm::{KanidmResource, is_resource_watched};
use kaniop_operator::controller::{
    DEFAULT_RECONCILE_INTERVAL,
    context::{Context, IdmClientContext},
};
use kaniop_operator::telemetry;

use std::sync::Arc;
use std::time::Duration;

use futures::TryFutureExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kanidm_client::KanidmClient;
use kanidm_proto::constants::{
    ATTR_ALLOW_PRIMARY_CRED_FALLBACK, ATTR_CREDENTIAL_TYPE_MINIMUM, ATTR_ENTRY_MANAGED_BY,
    ATTR_MAIL, ATTR_MEMBER,
};
use kanidm_proto::v1::Entry;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use kube::{Resource, ResourceExt};
use tracing::{Span, debug, field, info, instrument, trace, warn};

pub static GROUP_OPERATOR_NAME: &str = "kanidmgroups.kaniop.rs";
pub static GROUP_FINALIZER: &str = "kanidmgroups.kaniop.rs/finalizer";

const TYPE_EXISTS: &str = "Exists";
const TYPE_MAIL_UPDATED: &str = "MailUpdated";
const TYPE_MANAGED_UPDATED: &str = "ManagedUpdated";
const TYPE_MEMBERS_UPDATED: &str = "MembersUpdated";
const TYPE_POSIX_INITIALIZED: &str = "PosixInitialized";
const TYPE_POSIX_UPDATED: &str = "PosixUpdated";
const TYPE_ACCOUNT_POLICY_ENABLED: &str = "AccountPolicyEnabled";
const TYPE_ACCOUNT_POLICY_UPDATED: &str = "AccountPolicyUpdated";
const REASON_ATTRIBUTE_MATCH: &str = "AttributeMatch";
const REASON_ATTRIBUTE_NOT_MATCH: &str = "AttributeNotMatch";
const REASON_ATTRIBUTES_MATCH: &str = "AttributesMatch";
const REASON_ATTRIBUTES_NOT_MATCH: &str = "AttributesNotMatch";
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

pub fn watched_resource(group: &KanidmGroup, ctx: Arc<Context<KanidmGroup>>) -> bool {
    let kanidm = if let Some(k) = ctx.get_kanidm(group) {
        k
    } else {
        trace!(msg = "no kanidm found");
        return false;
    };

    is_resource_watched(group, &kanidm, &ctx.namespace_store)
}

#[instrument(skip(ctx, group))]
pub async fn reconcile_group(
    group: Arc<KanidmGroup>,
    ctx: Arc<Context<KanidmGroup>>,
) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    let kanidm_client = ctx.get_idm_client(&group).await?;

    if !watched_resource(&group, ctx.clone()) {
        debug!(msg = "resource not watched, skipping reconcile");
        ctx.recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: "ResourceNotWatched".to_string(),
                note: Some("configure `groupNamespaceSelector` on Kanidm resource to watch this namespace".to_string()),
                action: "Reconcile".to_string(),
                secondary: None,
            },
            &group.object_ref(&()),
        )
        .await
        .map_err(|e| {
            warn!(msg = "failed to publish ResourceNotWatched event", %e);
            Error::KubeError("failed to publish event".to_string(), Box::new(e))
        })?;
        return Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL));
    }

    info!(msg = "reconciling group");

    // safe unwrap: group is namespaced scoped
    let namespace = group.get_namespace();
    let status = group
        .update_status(kanidm_client.clone(), ctx.clone())
        .await
        .map_err(|e| {
            debug!(msg = "failed to reconcile status", %e);
            ctx.metrics.status_update_errors_inc();
            e
        })?;
    let groups_api: Api<KanidmGroup> = Api::namespaced(ctx.client.clone(), &namespace);
    finalizer(&groups_api, GROUP_FINALIZER, group, |event| async {
        match event {
            Finalizer::Apply(g) => g.reconcile(kanidm_client, status, ctx).await,
            Finalizer::Cleanup(g) => g.cleanup(kanidm_client, status).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError("failed on group finalizer".to_string(), Box::new(e)))
}

impl KanidmGroup {
    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: group is namespaced scoped
        self.namespace().unwrap()
    }

    #[inline]
    async fn reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmGroupStatus,
        ctx: Arc<Context<KanidmGroup>>,
    ) -> Result<Action> {
        match self.internal_reconcile(kanidm_client, status).await {
            Ok(action) => Ok(action),
            Err(e) => match e {
                Error::KanidmClientError(_, _) => {
                    ctx.recorder
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
                            warn!(msg = "failed to publish KanidmError event", %e);
                            Error::KubeError("failed to publish event".to_string(), Box::new(e))
                        })?;
                    Err(e)
                }
                _ => Err(e),
            },
        }
    }

    #[inline]
    async fn internal_reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmGroupStatus,
    ) -> Result<Action> {
        let name = &self.name_any();
        let mut require_status_update = false;
        if is_group_false(TYPE_EXISTS, status.clone()) {
            self.create(&kanidm_client, name).await?;
            require_status_update = true;
        }

        if is_group_false(TYPE_MANAGED_UPDATED, status.clone()) {
            self.update_managed_by(&kanidm_client, name).await?;
            require_status_update = true;
        }

        if is_group_false(TYPE_MAIL_UPDATED, status.clone()) {
            self.update_mail(&kanidm_client, name).await?;
            require_status_update = true;
        }

        if is_group_false(TYPE_MEMBERS_UPDATED, status.clone()) {
            self.update_members(&kanidm_client, name).await?;
            require_status_update = true;
        }

        if is_group_false(TYPE_POSIX_UPDATED, status.clone())
            || (is_group_false(TYPE_POSIX_INITIALIZED, status.clone())
                && is_group(TYPE_POSIX_UPDATED, status.clone()))
        {
            self.update_posix_attributes(&kanidm_client, name).await?;
            require_status_update = true;
        }

        // Account policy reconciliation: enable first if needed, then update settings
        if is_group_false(TYPE_ACCOUNT_POLICY_ENABLED, status.clone()) {
            self.enable_account_policy(&kanidm_client, name).await?;
            require_status_update = true;
        }

        if is_group_false(TYPE_ACCOUNT_POLICY_UPDATED, status.clone()) {
            self.update_account_policy(&kanidm_client, name).await?;
            require_status_update = true;
        }

        if require_status_update {
            trace!(msg = "status update required, requeueing in 500ms");
            Ok(Action::requeue(Duration::from_millis(500)))
        } else {
            Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
        }
    }

    async fn create(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!(msg = "create");
        kanidm_client
            .idm_group_create(name, self.spec.entry_managed_by.as_deref())
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to create {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn update_managed_by(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!(msg = format!("update {ATTR_ENTRY_MANAGED_BY} attribute"));
        let entry_managed_by =
            self.spec.entry_managed_by.as_ref().ok_or_else(|| {
                Error::MissingData("group entryManagedBy is not defined".to_string())
            })?;

        kanidm_client
            .idm_group_set_entry_managed_by(name, entry_managed_by)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to update {ATTR_ENTRY_MANAGED_BY} for {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn update_mail(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!(msg = format!("update {ATTR_MAIL} attribute"));
        let mail = self
            .spec
            .mail
            .as_ref()
            .ok_or_else(|| Error::MissingData("group mail is not defined".to_string()))?;

        if mail.is_empty() {
            kanidm_client
                .idm_group_purge_mail(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to purge {ATTR_MAIL} for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .idm_group_set_mail(name, mail)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {ATTR_MAIL} for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }
        Ok(())
    }

    async fn update_members(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!(msg = format!("update {ATTR_MEMBER} attribute"));
        let members = self
            .spec
            .members
            .as_ref()
            .ok_or_else(|| Error::MissingData("group members is not defined".to_string()))?
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<&str>>();

        kanidm_client
            .idm_group_set_members(name, &members)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to update {ATTR_MEMBER} for {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn update_posix_attributes(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
    ) -> Result<()> {
        debug!(msg = "update posix attributes");
        trace!(msg = format!("update posix attributes {:?}", self.spec.posix_attributes));
        kanidm_client
            .idm_group_unix_extend(
                name,
                self.spec
                    .posix_attributes
                    .as_ref()
                    .and_then(|posix| posix.gidnumber),
            )
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to unix extend {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn enable_account_policy(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!(msg = "enable account policy");
        kanidm_client
            .group_account_policy_enable(name)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to enable account policy for {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn update_account_policy(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!(msg = "update account policy");
        let policy =
            self.spec.account_policy.as_ref().ok_or_else(|| {
                Error::MissingData("group accountPolicy is not defined".to_string())
            })?;
        trace!(msg = format!("update account policy {:?}", policy));

        // Update or reset auth session expiry
        if let Some(expiry) = policy.auth_session_expiry {
            kanidm_client
                .group_account_policy_authsession_expiry_set(name, expiry)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set auth session expiry for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .group_account_policy_authsession_expiry_reset(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset auth session expiry for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        // Update or reset credential type minimum
        if let Some(ref cred_type) = policy.credential_type_minimum {
            kanidm_client
                .group_account_policy_credential_type_minimum_set(name, &cred_type.to_string())
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set credential type minimum for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .idm_group_purge_attr(name, ATTR_CREDENTIAL_TYPE_MINIMUM)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset credential type minimum for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        // Update or reset password minimum length
        if let Some(length) = policy.password_minimum_length {
            kanidm_client
                .group_account_policy_password_minimum_length_set(name, length)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set password minimum length for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .group_account_policy_password_minimum_length_reset(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset password minimum length for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        // Update or reset privilege expiry
        if let Some(expiry) = policy.privilege_expiry {
            kanidm_client
                .group_account_policy_privilege_expiry_set(name, expiry)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set privilege expiry for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .group_account_policy_privilege_expiry_reset(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset privilege expiry for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        // Update or reset webauthn attestation CA list
        if let Some(ref ca_list) = policy.webauthn_attestation_ca_list {
            kanidm_client
                .group_account_policy_webauthn_attestation_set(name, ca_list)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set webauthn attestation CA list for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .group_account_policy_webauthn_attestation_reset(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset webauthn attestation CA list for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        // Update or reset allow primary cred fallback
        if let Some(allow) = policy.allow_primary_cred_fallback {
            kanidm_client
                .group_account_policy_allow_primary_cred_fallback(name, allow)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set allow primary cred fallback for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .idm_group_purge_attr(name, ATTR_ALLOW_PRIMARY_CRED_FALLBACK)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset allow primary cred fallback for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        // Update or reset limit search max results
        if let Some(max_results) = policy.limit_search_max_results {
            kanidm_client
                .group_account_policy_limit_search_max_results(name, max_results)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set limit search max results for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .group_account_policy_limit_search_max_results_reset(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset limit search max results for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        // Update or reset limit search max filter test
        if let Some(max_filter_test) = policy.limit_search_max_filter_test {
            kanidm_client
                .group_account_policy_limit_search_max_filter_test(name, max_filter_test)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to set limit search max filter test for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        } else {
            kanidm_client
                .group_account_policy_limit_search_max_filter_test_reset(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to reset limit search max filter test for {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
                    )
                })?;
        }

        Ok(())
    }

    async fn cleanup(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmGroupStatus,
    ) -> Result<Action> {
        let name = &self.name_any();

        if is_group(TYPE_EXISTS, status.clone()) {
            debug!(msg = "delete");
            kanidm_client.idm_group_delete(name).await.map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to delete {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        }
        Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
    }

    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context<KanidmGroup>>,
    ) -> Result<KanidmGroupStatus> {
        // safe unwrap: person is namespaced scoped
        let namespace = self.get_namespace();
        let name = self.name_any();
        let current_group = kanidm_client
            .idm_group_get(&name)
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to get {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })
            .await?;

        let status = self.generate_status(current_group)?;
        let status_patch = Patch::Apply(KanidmGroup {
            status: Some(status.clone()),
            ..KanidmGroup::default()
        });
        debug!(msg = "updating status");
        trace!(msg = format!("status patch {:?}", status_patch));
        let patch = PatchParams::apply(GROUP_OPERATOR_NAME).force();
        let kanidm_api = Api::<KanidmGroup>::namespaced(ctx.client.clone(), &namespace);
        let _o = kanidm_api
            .patch_status(&name, &patch, &status_patch)
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to patch KanidmGroup/status {namespace}/{name}"),
                    Box::new(e),
                )
            })?;
        Ok(status)
    }

    /// Generate account policy conditions by comparing desired spec with current state.
    ///
    /// Returns conditions for:
    /// - AccountPolicyEnabled: Whether account policy is enabled on the group
    /// - AccountPolicyUpdated: Whether all policy settings match desired state (only if policy is in spec)
    fn generate_account_policy_conditions(
        &self,
        current: &KanidmGroupAccountPolicyAttributes,
        now: Timestamp,
    ) -> Vec<Condition> {
        let mut conditions = Vec::new();

        // Only generate conditions if account_policy is specified in the spec
        if let Some(ref policy) = self.spec.account_policy {
            // AccountPolicyEnabled condition - check if policy is enabled in Kanidm
            let enabled_condition = if current.enabled {
                Condition {
                    type_: TYPE_ACCOUNT_POLICY_ENABLED.to_string(),
                    status: CONDITION_TRUE.to_string(),
                    reason: "AccountPolicyEnabled".to_string(),
                    message: "Account policy is enabled on this group.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                }
            } else {
                Condition {
                    type_: TYPE_ACCOUNT_POLICY_ENABLED.to_string(),
                    status: CONDITION_FALSE.to_string(),
                    reason: "AccountPolicyNotEnabled".to_string(),
                    message: "Account policy is not enabled on this group.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                }
            };
            conditions.push(enabled_condition);

            // AccountPolicyUpdated condition - check if all settings match
            // Only check if policy is already enabled
            if current.enabled {
                let settings_match = self.account_policy_settings_match(policy, current);
                let updated_condition = if settings_match {
                    Condition {
                        type_: TYPE_ACCOUNT_POLICY_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: REASON_ATTRIBUTES_MATCH.to_string(),
                        message: "Account policy settings match desired state.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_ACCOUNT_POLICY_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                        message: "Account policy settings do not match desired state.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                };
                conditions.push(updated_condition);
            }
        }

        conditions
    }

    /// Check if account policy settings in spec match current state in Kanidm.
    /// When a setting is specified in spec, it must match the current state.
    /// When a setting is None in spec, the current state should also be None (reset behavior).
    fn account_policy_settings_match(
        &self,
        policy: &crate::crd::KanidmGroupAccountPolicy,
        current: &KanidmGroupAccountPolicyAttributes,
    ) -> bool {
        // For each setting: if spec has Some(v), current must have Some(v)
        // If spec has None, current must also be None (to ensure reset happened)
        let auth_session_match = policy.auth_session_expiry == current.auth_session_expiry;

        let cred_type_match = policy.credential_type_minimum == current.credential_type_minimum;

        let password_len_match = policy.password_minimum_length == current.password_minimum_length;

        let privilege_match = policy.privilege_expiry == current.privilege_expiry;

        let webauthn_match =
            policy.webauthn_attestation_ca_list == current.webauthn_attestation_ca_list;

        let primary_cred_match =
            policy.allow_primary_cred_fallback == current.allow_primary_cred_fallback;

        let max_results_match = policy.limit_search_max_results == current.limit_search_max_results;

        let max_filter_match =
            policy.limit_search_max_filter_test == current.limit_search_max_filter_test;

        auth_session_match
            && cred_type_match
            && password_len_match
            && privilege_match
            && webauthn_match
            && primary_cred_match
            && max_results_match
            && max_filter_match
    }

    fn generate_status(&self, group: Option<Entry>) -> Result<KanidmGroupStatus> {
        let now = Timestamp::now();
        match group {
            Some(g) => {
                let exist_condition = Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_TRUE.to_string(),
                    reason: "Exists".to_string(),
                    message: "Group exists.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                };
                let managed_by_condition = self.spec.entry_managed_by.as_ref().map(|managed_by| {
                    if Some(managed_by)
                        == get_first_cloned(&g, ATTR_ENTRY_MANAGED_BY)
                            .as_ref()
                            .map(|s| normalize_spn(s.as_str()))
                            .as_ref()
                    {
                        Condition {
                            type_: TYPE_MANAGED_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "Group exists with desired {ATTR_ENTRY_MANAGED_BY} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_MANAGED_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "Group exists with different {ATTR_ENTRY_MANAGED_BY} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

                let mail_condition = self.spec.mail.as_ref().map(|mail| {
                    // comparison is ordered because first mail is primary
                    if Some(mail) == g.attrs.get(ATTR_MAIL) {
                        Condition {
                            type_: TYPE_MAIL_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!("Group exists with desired {ATTR_MAIL} attribute."),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_MAIL_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!("Group exists with different {ATTR_MAIL} attribute."),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

                let members_condition = self.spec.members.as_ref().map(|members| {
                    if compare_names(members, g.attrs.get(ATTR_MEMBER).unwrap_or(&Vec::new())) {
                        Condition {
                            type_: TYPE_MEMBERS_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTES_MATCH.to_string(),
                            message: format!("Group exists with desired {ATTR_MEMBER} attributes."),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_MEMBERS_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                            message: format!(
                                "Group exists with different {ATTR_MEMBER} attributes."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

                // Account policy conditions
                let current_account_policy = KanidmGroupAccountPolicyAttributes::from(&g);
                let account_policy_conditions =
                    self.generate_account_policy_conditions(&current_account_policy, now);

                let current_group_posix = KanidmGroupPosixAttributes::from(g);
                let posix_initialized_condition = if current_group_posix.gidnumber.is_some() {
                    Condition {
                        type_: TYPE_POSIX_INITIALIZED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "PosixInitialized".to_string(),
                        message: "Group exists with POSIX attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_POSIX_INITIALIZED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: "PosixNotInitialized".to_string(),
                        message: "Group exists without POSIX attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                };

                let posix_updated_condition = self.spec.posix_attributes.as_ref().map(|posix| {
                    if posix == &current_group_posix {
                        Condition {
                            type_: TYPE_POSIX_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTES_MATCH.to_string(),
                            message: "Group exists with desired POSIX attributes.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_POSIX_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                            message: "Group exists with different POSIX attributes.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let conditions = vec![exist_condition, posix_initialized_condition]
                    .into_iter()
                    .chain(managed_by_condition)
                    .chain(mail_condition)
                    .chain(members_condition)
                    .chain(posix_updated_condition)
                    .chain(account_policy_conditions)
                    .collect::<Vec<_>>();
                let status = conditions
                    .iter()
                    .filter(|c| {
                        c.type_ != TYPE_POSIX_INITIALIZED && c.type_ != TYPE_ACCOUNT_POLICY_ENABLED
                    })
                    .all(|c| c.status == CONDITION_TRUE);
                Ok(KanidmGroupStatus {
                    conditions: Some(conditions),
                    ready: status,
                    gid: current_group_posix.gidnumber,
                    kanidm_ref: self.kanidm_ref(),
                })
            }
            None => {
                let conditions = vec![Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_FALSE.to_string(),
                    reason: "NotExists".to_string(),
                    message: "Group is not present.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                }];
                Ok(KanidmGroupStatus {
                    conditions: Some(conditions),
                    ready: false,
                    gid: None,
                    kanidm_ref: self.kanidm_ref(),
                })
            }
        }
    }
}

pub fn is_group(type_: &str, status: KanidmGroupStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_TRUE)
}

pub fn is_group_false(type_: &str, status: KanidmGroupStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_FALSE)
}
