use crate::crd::{KanidmGroup, KanidmGroupPosixAttributes, KanidmGroupStatus};

use kaniop_k8s_util::events::{Event, EventType};
use kaniop_k8s_util::types::{compare_names, get_first_cloned};
use kaniop_operator::controller::{
    context::{Context, ContextKanidmClient},
    DEFAULT_RECONCILE_INTERVAL,
};
use kaniop_operator::error::{Error, Result};
use kaniop_operator::telemetry;

use std::sync::Arc;
use std::time::Duration;

use futures::TryFutureExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kanidm_client::KanidmClient;
use kanidm_proto::constants::{ATTR_ENTRY_MANAGED_BY, ATTR_MAIL, ATTR_MEMBER};
use kanidm_proto::v1::Entry;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{finalizer, Event as Finalizer};
use kube::runtime::reflector::ObjectRef;
use kube::{Resource, ResourceExt};
use tracing::{debug, field, info, instrument, trace, warn, Span};

pub static GROUP_OPERATOR_NAME: &str = "kanidmgroups.kaniop.rs";
pub static GROUP_FINALIZER: &str = "kanidms.kaniop.rs/group";

const TYPE_EXISTS: &str = "Exists";
const TYPE_MAIL_UPDATED: &str = "MailUpdated";
const TYPE_MANAGED_UPDATED: &str = "ManagedUpdated";
const TYPE_MEMBERS_UPDATED: &str = "MembersUpdated";
const TYPE_POSIX_INITIALIZED: &str = "PosixInitialized";
const TYPE_POSIX_UPDATED: &str = "PosixUpdated";
const REASON_ATTRIBUTE_MATCH: &str = "AttributeMatch";
const REASON_ATTRIBUTE_NOT_MATCH: &str = "AttributeNotMatch";
const REASON_ATTRIBUTES_MATCH: &str = "AttributesMatch";
const REASON_ATTRIBUTES_NOT_MATCH: &str = "AttributesNotMatch";
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

#[instrument(skip(ctx, group))]
pub async fn reconcile_group(
    group: Arc<KanidmGroup>,
    ctx: Arc<Context<KanidmGroup>>,
) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    info!(msg = "reconciling group");

    // safe unwrap: group is namespaced scoped
    let namespace = group.get_namespace();
    let kanidm_client = ctx.get_idm_client(&group).await?;
    let status = group
        .update_status(kanidm_client.clone(), ctx.clone())
        .await
        .map_err(|e| {
            debug!(msg = "failed to reconcile status", %e);
            ctx.metrics.status_update_errors_inc();
            e
        })?;
    let persons_api: Api<KanidmGroup> = Api::namespaced(ctx.client.clone(), &namespace);
    finalizer(&persons_api, GROUP_FINALIZER, group, |event| async {
        match event {
            Finalizer::Apply(p) => p.reconcile(kanidm_client, status, ctx).await,
            Finalizer::Cleanup(p) => p.cleanup(kanidm_client, status, ctx).await,
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
                            Event {
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
                            Error::KubeError("failed to publish event".to_string(), e)
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
        let namespace = self.get_namespace();

        let mut require_status_update = false;
        if is_group_false(TYPE_EXISTS, status.clone()) {
            self.create(&kanidm_client, name, &namespace).await?;
            require_status_update = true;
        }

        // TODO: resolve issue in update_managed_by function
        // if is_group_false(TYPE_MANAGED_UPDATED, status.clone()) {
        //     self.update_managed_by(&kanidm_client, name, &namespace)
        //         .await?;
        //     require_status_update = true;
        // }

        if is_group_false(TYPE_MAIL_UPDATED, status.clone()) {
            self.update_mail(&kanidm_client, name, &namespace).await?;
            require_status_update = true;
        }

        if is_group_false(TYPE_MEMBERS_UPDATED, status.clone()) {
            self.update_members(&kanidm_client, name, &namespace)
                .await?;
            require_status_update = true;
        }

        if is_group_false(TYPE_POSIX_UPDATED, status.clone())
            || (is_group_false(TYPE_POSIX_INITIALIZED, status.clone())
                && is_group(TYPE_POSIX_UPDATED, status.clone()))
        {
            self.update_posix_attributes(&kanidm_client, name, &namespace)
                .await?;
            require_status_update = true;
        }

        if require_status_update {
            trace!(msg = "status update required, requeueing in 500ms");
            Ok(Action::requeue(Duration::from_millis(500)))
        } else {
            Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
        }
    }

    async fn create(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = "create");
        kanidm_client
            .idm_group_create(name, self.spec.entry_managed_by.as_deref())
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to create {name} from {namespace}/{kanidm}",
                        kanidm = self.spec.kanidm_ref.name
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    #[allow(dead_code)]
    async fn update_managed_by(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_ENTRY_MANAGED_BY} attribute"));
        let entry_managed_by =
            self.spec.entry_managed_by.as_ref().ok_or_else(|| {
                Error::MissingData("group entryManagedBy is not defined".to_string())
            })?;

        // TODO: admin client is needed for this operation
        // https://github.com/kanidm/kanidm/issues/3270
        kanidm_client
            .idm_group_set_entry_managed_by(name, entry_managed_by)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to update {ATTR_ENTRY_MANAGED_BY} for {name} from {namespace}/{kanidm}",
                        kanidm = self.spec.kanidm_ref.name
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn update_mail(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
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
                            kanidm = self.spec.kanidm_ref.name
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
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
        }
        Ok(())
    }

    async fn update_members(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
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
                        kanidm = self.spec.kanidm_ref.name
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
        namespace: &str,
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
                        kanidm = self.spec.kanidm_ref.name
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn cleanup(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmGroupStatus,
        ctx: Arc<Context<KanidmGroup>>,
    ) -> Result<Action> {
        let name = &self.name_any();
        let namespace = self.get_namespace();

        if is_group(TYPE_EXISTS, status.clone()) {
            debug!(msg = "delete");
            kanidm_client.idm_group_delete(name).await.map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to delete {name} from {namespace}/{kanidm}",
                        kanidm = self.spec.kanidm_ref.name
                    ),
                    Box::new(e),
                )
            })?;

            ctx.internal_cache
                .write()
                .await
                .remove(&ObjectRef::from(self));
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
                        kanidm = self.spec.kanidm_ref.name
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
                    e,
                )
            })?;
        Ok(status)
    }

    fn generate_status(&self, group: Option<Entry>) -> Result<KanidmGroupStatus> {
        let now = Utc::now();
        let conditions = match group {
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
                    if Some(managed_by) == get_first_cloned(&g, ATTR_ENTRY_MANAGED_BY).as_ref() {
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
                vec![exist_condition, posix_initialized_condition]
                    .into_iter()
                    .chain(managed_by_condition)
                    .chain(mail_condition)
                    .chain(members_condition)
                    .chain(posix_updated_condition)
                    .collect()
            }
            None => vec![Condition {
                type_: TYPE_EXISTS.to_string(),
                status: CONDITION_FALSE.to_string(),
                reason: "NotExists".to_string(),
                message: "Group is not present.".to_string(),
                last_transition_time: Time(now),
                observed_generation: self.metadata.generation,
            }],
        };
        Ok(KanidmGroupStatus {
            conditions: Some(conditions),
        })
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
