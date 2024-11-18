use crate::crd::{KanidmPersonAccount, KanidmPersonAccountStatus, KanidmPersonAttributes};

use kaniop_k8s_util::types::{get_first_cloned, parse_time};

use futures::TryFutureExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kanidm_client::KanidmClient;
use kanidm_proto::v1::Entry;
use kaniop_operator::controller::Context;
use kaniop_operator::error::{Error, Result};

use std::sync::Arc;
use std::time::Duration;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{finalizer, Event as Finalizer};
use kube::ResourceExt;
use tracing::{debug, instrument, trace};

pub static PERSON_OPERATOR_NAME: &str = "kanidmpeopleaccounts.kaniop.rs";
pub static PERSON_FINALIZER: &str = "kanidms.kaniop.rs/person";

const TYPE_EXISTS: &str = "Exists";
const TYPE_UPDATED: &str = "Updated";
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

#[instrument(skip(ctx, person))]
pub async fn reconcile_person_account(
    person: Arc<KanidmPersonAccount>,
    ctx: Arc<Context>,
) -> Result<Action> {
    // TODO: ctx has to contain a cache with all the Kanidm clients, one per Kanidm.
    // Shared between all controllers.
    // safe unwrap: person is namespaced scoped
    let namespace = person.get_namespace();
    let people_api: Api<KanidmPersonAccount> = Api::namespaced(ctx.client.clone(), &namespace);
    finalizer(&people_api, PERSON_FINALIZER, person, |event| async {
        match event {
            // patch account
            Finalizer::Apply(p) => p.reconcile(ctx).await,
            // check finalizer to remove account
            Finalizer::Cleanup(p) => p.cleanup(ctx).await,
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

    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        let kanidm_client = self.get_kanidm_client(ctx.clone()).await?;
        let status = self
            .update_status(kanidm_client.clone(), ctx.clone())
            .await?;

        let name = &self.name_any();
        let namespace = self.get_namespace();

        if !is_person_exists(status.clone()) {
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
        }
        if !is_person_updated(status) {
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
        }
        // patch account
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        let kanidm_client = self.get_kanidm_client(ctx.clone()).await?;
        let name = &self.name_any();
        let namespace = self.get_namespace();
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
        let conditions = match person {
            Some(p) => {
                let exist_condition = Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_TRUE.to_string(),
                    reason: "Exists".to_string(),
                    message: "Person exists.".to_string(),
                    last_transition_time: Time(Utc::now()),
                    observed_generation: self.metadata.generation,
                };
                let updated_condition = if self.spec.person_attributes == p.into() {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "AttributesMatch".to_string(),
                        message: "Person exists with desired attributes.".to_string(),
                        last_transition_time: Time(Utc::now()),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: "AttributesNotMatch".to_string(),
                        message: "Person exists with different attributes.".to_string(),
                        last_transition_time: Time(Utc::now()),
                        observed_generation: self.metadata.generation,
                    }
                };
                vec![exist_condition, updated_condition]
            }
            None => vec![Condition {
                type_: TYPE_EXISTS.to_string(),
                status: CONDITION_TRUE.to_string(),
                reason: "NotExists".to_string(),
                message: "Person is not present.".to_string(),
                last_transition_time: Time(Utc::now()),
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
            displayname: get_first_cloned(&entry, "displayname").unwrap_or_default(),
            mail: entry.attrs.get("mail").cloned(),
            legalname: get_first_cloned(&entry, "legalname"),
            begin_from: parse_time(&entry, "begin_from"),
            expire_at: parse_time(&entry, "expire_at"),
        }
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
