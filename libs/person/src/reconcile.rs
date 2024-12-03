use crate::crd::{KanidmPersonAccount, KanidmPersonAccountStatus, KanidmPersonAttributes};

use kaniop_k8s_util::events::{Event, EventType};
use kaniop_operator::controller::{Context, ContextKanidmClient, DEFAULT_RECONCILE_INTERVAL};
use kaniop_operator::crd::KanidmPosixAttributes;
use kaniop_operator::error::{Error, Result};
use kaniop_operator::telemetry;

use std::collections::BTreeMap;
use std::ops::Not;
use std::sync::Arc;
use std::time::Duration;

use futures::TryFutureExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kanidm_client::{ClientError, KanidmClient};
use kanidm_proto::constants::{ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM};
use kanidm_proto::v1::Entry;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{finalizer, Event as Finalizer};
use kube::runtime::reflector::ObjectRef;
use kube::{Resource, ResourceExt};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use tracing::{debug, field, info, instrument, trace, warn, Span};

pub static PERSON_OPERATOR_NAME: &str = "kanidmpeopleaccounts.kaniop.rs";
pub static PERSON_FINALIZER: &str = "kanidms.kaniop.rs/person";

// TODO: check when create-reset-token is executed
const DEFAULT_RESET_TOKEN_TTL: u32 = 3600;
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

#[instrument(skip(ctx, person))]
pub async fn reconcile_person_account(
    person: Arc<KanidmPersonAccount>,
    ctx: Arc<Context<KanidmPersonAccount>>,
) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    info!(msg = "reconciling person account");

    // safe unwrap: person is namespaced scoped
    let namespace = person.get_namespace();
    let kanidm_client = ctx.get_kanidm_client(&person).await?;
    let status = person
        .update_status(kanidm_client.clone(), ctx.clone())
        .await?;
    let people_api: Api<KanidmPersonAccount> = Api::namespaced(ctx.client.clone(), &namespace);
    finalizer(
        &people_api,
        PERSON_FINALIZER,
        person.clone(),
        |event| async {
            match event {
                Finalizer::Apply(p) => p.reconcile(kanidm_client, status, ctx, &person).await,
                Finalizer::Cleanup(p) => p.cleanup(kanidm_client, status, ctx, &person).await,
            }
        },
    )
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

    async fn reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmPersonAccountStatus,
        ctx: Arc<Context<KanidmPersonAccount>>,
        person: &KanidmPersonAccount,
    ) -> Result<Action> {
        let name = &self.name_any();
        let namespace = self.get_namespace();

        let mut require_status_update = false;
        if is_person_false(TYPE_EXISTS, status.clone()) {
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
        if is_person_false(TYPE_UPDATED, status.clone()) {
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

            if update_entry.attrs.is_empty().not() {
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

        if is_person_false(TYPE_POSIX_UPDATED, status.clone())
            || (is_person_false(TYPE_POSIX_INITIALIZED, status.clone())
                && is_person(TYPE_POSIX_UPDATED, status.clone()))
        {
            debug!(msg = "update person posix attributes");
            trace!(
                msg = format!(
                    "update person posix attributes {:?}",
                    self.spec.posix_attributes
                )
            );
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
                    Error::KanidmClientError(
                        format!(
                            "failed to update {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
            require_status_update = true;
        }

        if is_person_false(TYPE_CREDENTIAL, status) {
            let create_token = match ctx
                .internal_cache
                .read()
                .await
                .get(&ObjectRef::from(person))
            {
                Some(expiry) if expiry > &OffsetDateTime::now_utc() => {
                    trace!(msg = "token not expired, skipping creation");
                    false
                }
                _ => true,
            };
            if create_token {
                debug!(msg = "create reset token");
                let cu_token = kanidm_client
                    .idm_person_account_credential_update_intent(name, Some(DEFAULT_RESET_TOKEN_TTL))
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to create a credential reset token for {name} from {namespace}/{kanidm}",
                                kanidm = self.spec.kanidm_ref.name
                            ),
                            Box::new(e),
                        )
                    })?;
                let token = cu_token.token.as_str();
                let url = if let Some(domain) = ctx.get_domain(person).await {
                    format!("https://{domain}/ui/reset?token={token}")
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
                        .expect("Failed to format date time!!!")
                );
                ctx.recorder
                    .publish(
                        Event {
                            type_: EventType::Normal,
                            reason: "TokenCreated".to_string(),
                            note: Some(msg),
                            action: "CreateUpdateCredentialsToken".into(),
                            secondary: None,
                        },
                        &person.object_ref(&()),
                    )
                    .await
                    .map_err(|e| {
                        warn!(msg = "failed to publish TokenCreated event", %e);
                        Error::KubeError("failed to publish event".to_string(), e)
                    })?;
                ctx.internal_cache
                    .write()
                    .await
                    .insert(ObjectRef::from(person), expiry_time);
            };
        };

        if require_status_update {
            trace!(msg = "status update required, requeueing in 500ms");
            Ok(Action::requeue(Duration::from_millis(500)))
        } else {
            Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
        }
    }

    async fn cleanup(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmPersonAccountStatus,
        ctx: Arc<Context<KanidmPersonAccount>>,
        person: &KanidmPersonAccount,
    ) -> Result<Action> {
        let name = &self.name_any();
        let namespace = self.get_namespace();

        if is_person(TYPE_EXISTS, status.clone()) {
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

            ctx.internal_cache
                .write()
                .await
                .remove(&ObjectRef::from(person));
        }
        Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
    }

    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context<KanidmPersonAccount>>,
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
        let credential_present = match kanidm_client
            .idm_person_account_get_credential_status(&name)
            .await
        {
            Ok(cs) => Some(cs.creds.is_empty().not()),
            Err(ClientError::EmptyResponse) => Some(false),
            Err(_) => None,
        };

        let status = self.generate_status(current_person, credential_present)?;
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

    fn generate_status(
        &self,
        person: Option<Entry>,
        credential_present: Option<bool>,
    ) -> Result<KanidmPersonAccountStatus> {
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

                let current_person_attributes = KanidmPersonAttributes::from(p.clone());
                let updated_condition = if self.spec.person_attributes == current_person_attributes
                {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: REASON_ATTRIBUTES_MATCH.to_string(),
                        message: "Person exists with desired attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                        message: "Person exists with different attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                };

                let current_person_posix = KanidmPosixAttributes::from(p);
                let posix_initialized_condition = if current_person_posix.gidnumber.is_some() {
                    Condition {
                        type_: TYPE_POSIX_INITIALIZED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "PosixInitialized".to_string(),
                        message: "Person exists with POSIX attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_POSIX_INITIALIZED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: "PosixNotInitialized".to_string(),
                        message: "Person exists without POSIX attributes.".to_string(),
                        last_transition_time: Time(now),
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
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_POSIX_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                            message: "Person exists with different POSIX attributes.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

                let credentials_condition = credential_present.map(|c| {
                    if c {
                        Condition {
                            type_: TYPE_CREDENTIAL.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: "Present".to_string(),
                            message: "Credentials are present.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_CREDENTIAL.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: "NotPresent".to_string(),
                            message: "Credentials are not present.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

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
                vec![
                    exist_condition,
                    updated_condition,
                    posix_initialized_condition,
                    validity_condition,
                ]
                .into_iter()
                .chain(credentials_condition)
                .chain(posix_updated_condition)
                .collect()
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
