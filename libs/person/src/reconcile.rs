use crate::crd::{KanidmPersonAccount, KanidmPersonAccountStatus, KanidmPersonAttributes};

use kanidm_proto::constants::{
    ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM, ATTR_DISPLAYNAME, ATTR_LEGALNAME, ATTR_MAIL,
};
use kaniop_k8s_util::types::{get_first_cloned, parse_time};

use futures::TryFutureExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kanidm_client::KanidmClient;
use kanidm_proto::v1::Entry;
use kaniop_operator::controller::Context;
use kaniop_operator::error::{Error, Result};
use kaniop_operator::telemetry;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{finalizer, Event as Finalizer};
use kube::ResourceExt;
use tracing::{debug, field, info, instrument, trace, Span};

pub static PERSON_OPERATOR_NAME: &str = "kanidmpeopleaccounts.kaniop.rs";
pub static PERSON_FINALIZER: &str = "kanidms.kaniop.rs/person";

const TYPE_EXISTS: &str = "Exists";
const TYPE_UPDATED: &str = "Updated";
const TYPE_VALIDITY: &str = "Valid";
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

#[instrument(skip(ctx, person))]
pub async fn reconcile_person_account(
    person: Arc<KanidmPersonAccount>,
    ctx: Arc<Context>,
) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    info!(msg = "reconciling person account");

    // safe unwrap: person is namespaced scoped
    let namespace = person.get_namespace();
    let kanidm_client = person.get_kanidm_client(ctx.clone()).await?;
    let status = person
        .update_status(kanidm_client.clone(), ctx.clone())
        .await?;
    let people_api: Api<KanidmPersonAccount> = Api::namespaced(ctx.client.clone(), &namespace);
    finalizer(&people_api, PERSON_FINALIZER, person, |event| async {
        match event {
            Finalizer::Apply(p) => p.reconcile(kanidm_client, status).await,
            Finalizer::Cleanup(p) => p.cleanup(kanidm_client, status).await,
        }
    })
    .await
    .map_err(|e| {
        Error::FinalizerError(
            "failed on person account finalizer".to_string(),
            Box::new(e),
        )
    })
}

impl KanidmPersonAccount {
    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: Person is namespaced scoped
        self.namespace().unwrap()
    }

    #[inline]
    async fn get_kanidm_client(&self, ctx: Arc<Context>) -> Result<Arc<KanidmClient>> {
        // safe unwrap: person is namespaced scoped
        let namespace = self.get_namespace();
        ctx.get_kanidm_client(&namespace, &self.spec.kanidm_ref.name)
            .await
    }

    async fn reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmPersonAccountStatus,
    ) -> Result<Action> {
        let name = &self.name_any();
        let namespace = self.get_namespace();

        let mut require_status_update = false;
        if !is_person_exists(status.clone()) {
            debug!(msg = "create person");
            kanidm_client
                .idm_person_account_create(name, &self.spec.person_attributes.displayname)
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
            require_status_update = true;
        }
        if !is_person_updated(status) {
            debug!(msg = "update person");
            trace!(msg = format!("update person attributes {:?}", self.spec.person_attributes));
            kanidm_client
                .idm_person_account_update(
                    name,
                    None,
                    Some(&self.spec.person_attributes.displayname),
                    self.spec.person_attributes.legalname.as_deref(),
                    self.spec.person_attributes.mail.as_deref(),
                )
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
            let mut update_entry = Entry {
                attrs: BTreeMap::new(),
            };
            if let Some(account_expire) = self.spec.person_attributes.account_expire.as_ref() {
                update_entry.attrs.insert(
                    ATTR_ACCOUNT_EXPIRE.to_string(),
                    vec![account_expire.0.to_rfc3339()],
                );
            }
            if let Some(account_valid_from) =
                self.spec.person_attributes.account_valid_from.as_ref()
            {
                update_entry.attrs.insert(
                    ATTR_ACCOUNT_VALID_FROM.to_string(),
                    vec![account_valid_from.0.to_rfc3339()],
                );
            }

            if !update_entry.attrs.is_empty() {
                let _: Entry = kanidm_client
                    .perform_patch_request(&format!("/v1/person/{}", name), update_entry)
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to update {name} from {namespace}/{kanidm}",
                                kanidm = self.spec.kanidm_ref.name
                            ),
                            Box::new(e),
                        )
                    })?;
            }
            require_status_update = true;
        }

        if require_status_update {
            trace!(msg = "status update required, requeueing in 500ms");
            Ok(Action::requeue(Duration::from_millis(500)))
        } else {
            Ok(Action::requeue(Duration::from_secs(5 * 60)))
        }
    }

    async fn cleanup(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmPersonAccountStatus,
    ) -> Result<Action> {
        let name = &self.name_any();
        let namespace = self.get_namespace();

        if is_person_exists(status.clone()) {
            debug!(msg = "delete person");
            kanidm_client
                .idm_person_account_delete(name)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to delete {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
        }
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context>,
    ) -> Result<KanidmPersonAccountStatus> {
        // safe unwrap: person is namespaced scoped
        let namespace = self.get_namespace();
        let name = self.name_any();
        let current_person = kanidm_client
            .idm_person_account_get(&name)
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
        let status = self.generate_status(current_person)?;
        let status_patch = Patch::Apply(KanidmPersonAccount {
            status: Some(status.clone()),
            ..KanidmPersonAccount::default()
        });
        debug!(msg = "updating person status");
        trace!(msg = format!("status patch {:?}", status_patch));
        let patch = PatchParams::apply(PERSON_OPERATOR_NAME).force();
        let kanidm_api = Api::<KanidmPersonAccount>::namespaced(ctx.client.clone(), &namespace);
        let _o = kanidm_api
            .patch_status(&name, &patch, &status_patch)
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to patch KanidmPersonAccount/status {namespace}/{name}"),
                    e,
                )
            })?;
        Ok(status)
    }

    fn generate_status(&self, person: Option<Entry>) -> Result<KanidmPersonAccountStatus> {
        let now = Utc::now();
        let conditions = match person {
            Some(p) => {
                let exist_condition = Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_TRUE.to_string(),
                    reason: "Exists".to_string(),
                    message: "Person exists.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                };

                let current_person_attributes: KanidmPersonAttributes = p.into();
                let updated_condition = if self.spec.person_attributes == current_person_attributes
                {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "AttributesMatch".to_string(),
                        message: "Person exists with desired attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: "AttributesNotMatch".to_string(),
                        message: "Person exists with different attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
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
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_VALIDITY.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: "Invalid".to_string(),
                            message: "Account is invalid.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                };
                vec![exist_condition, updated_condition, validity_condition]
            }
            None => vec![Condition {
                type_: TYPE_EXISTS.to_string(),
                status: CONDITION_FALSE.to_string(),
                reason: "NotExists".to_string(),
                message: "Person is not present.".to_string(),
                last_transition_time: Time(now),
                observed_generation: self.metadata.generation,
            }],
        };
        Ok(KanidmPersonAccountStatus {
            conditions: Some(conditions),
        })
    }
}

impl From<Entry> for KanidmPersonAttributes {
    fn from(entry: Entry) -> Self {
        KanidmPersonAttributes {
            displayname: get_first_cloned(&entry, ATTR_DISPLAYNAME).unwrap_or_default(),
            mail: entry.attrs.get(ATTR_MAIL).cloned(),
            legalname: get_first_cloned(&entry, ATTR_LEGALNAME),
            account_valid_from: parse_time(&entry, ATTR_ACCOUNT_VALID_FROM),
            account_expire: parse_time(&entry, ATTR_ACCOUNT_EXPIRE),
        }
    }
}

impl PartialEq for KanidmPersonAttributes {
    /// Compare attributes defined in the first object with the second object values.
    /// If the second object has more attributes defined, they will be ignored.
    fn eq(&self, other: &Self) -> bool {
        self.displayname == other.displayname
            && (self.mail.is_none() || self.mail == other.mail)
            && (self.legalname.is_none() || self.legalname == other.legalname)
            && (self.account_valid_from.is_none()
                || self.account_valid_from == other.account_valid_from)
            && (self.account_expire.is_none() || self.account_expire == other.account_expire)
    }
}

pub fn is_person_exists(status: KanidmPersonAccountStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == TYPE_EXISTS && c.status == CONDITION_TRUE)
}

pub fn is_person_updated(status: KanidmPersonAccountStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == TYPE_UPDATED && c.status == CONDITION_TRUE)
}
