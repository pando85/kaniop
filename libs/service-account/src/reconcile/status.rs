use super::SERVICE_ACCOUNT_OPERATOR_NAME;
use super::secret::SecretExt;

use crate::controller::Context;
use crate::crd::{
    KanidmAPIToken, KanidmAPITokenStatus, KanidmServiceAccount, KanidmServiceAccountAttributes,
    KanidmServiceAccountStatus,
};
use crate::reconcile::secret::TOKEN_LABEL;

use kaniop_k8s_util::error::{Error, Result};
use kaniop_operator::controller::INSTANCE_LABEL;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::KanidmAccountPosixAttributes;

use std::collections::BTreeSet;
use std::sync::Arc;

use futures::TryFutureExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kanidm_client::KanidmClient;
use kanidm_proto::v1::Entry;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams};
use tracing::{debug, trace};

pub const TYPE_API_TOKENS: &str = "ApiTokensUpdated";
pub const TYPE_EXISTS: &str = "Exists";
pub const TYPE_UPDATED: &str = "Updated";
pub const TYPE_POSIX_INITIALIZED: &str = "PosixInitialized";
pub const TYPE_POSIX_UPDATED: &str = "PosixUpdated";
pub const TYPE_VALIDITY: &str = "Valid";
pub const REASON_ATTRIBUTES_MATCH: &str = "AttributesMatch";
pub const REASON_ATTRIBUTES_NOT_MATCH: &str = "AttributesNotMatch";
pub const CONDITION_TRUE: &str = "True";
pub const CONDITION_FALSE: &str = "False";

#[allow(async_fn_in_trait)]
pub trait StatusExt {
    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context>,
    ) -> Result<KanidmServiceAccountStatus>;
}

impl StatusExt for KanidmServiceAccount {
    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context>,
    ) -> Result<KanidmServiceAccountStatus> {
        // safe unwrap: service account is namespaced scoped
        let namespace = self.get_namespace();
        let name = self.name_any();
        let current_service_account = kanidm_client
            .idm_service_account_get(&name)
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
        let api_tokens: Vec<KanidmAPITokenStatus> = kanidm_client
            .idm_service_account_list_api_token(&name)
            .await
            .or_else(|e| match e {
                // if service account does not exist, return empty list of tokens
                kanidm_client::ClientError::Http(status, _, _) if status == 404 => Ok(vec![]),
                _ => Err(Error::KanidmClientError(
                    format!(
                        "failed to list API tokens for {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )),
            })?
            .into_iter()
            .map(|t| {
                let secret_name = ctx
                    .secret_store
                    .state()
                    .into_iter()
                    .find(|secret| {
                        secret.metadata.labels.iter().any(|l| {
                            l.get(INSTANCE_LABEL) == Some(&self.name_any())
                                && l.get(TOKEN_LABEL) == Some(&t.label)
                        })
                    })
                    .and_then(|s| {
                        let name = s.name_any();
                        // TODO: it can be defined as that name and then it will marked as not ready
                        match name == self.generate_token_secret_name(&t.label) {
                            true => None,
                            false => Some(name),
                        }
                    });
                KanidmAPITokenStatus::new(t, secret_name)
            })
            .collect();
        let status = self.generate_status(current_service_account, api_tokens)?;
        let status_patch = Patch::Apply(KanidmServiceAccount {
            status: Some(status.clone()),
            ..KanidmServiceAccount::default()
        });
        debug!(msg = "updating status");
        trace!(msg = format!("status patch {:?}", status_patch));
        let patch = PatchParams::apply(SERVICE_ACCOUNT_OPERATOR_NAME).force();
        let kanidm_api =
            Api::<KanidmServiceAccount>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
        let _o = kanidm_api
            .patch_status(&name, &patch, &status_patch)
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to patch KanidmServiceAccount/status {namespace}/{name}"),
                    Box::new(e),
                )
            })?;
        Ok(status)
    }
}

impl KanidmServiceAccount {
    fn generate_status(
        &self,
        service_account: Option<Entry>,
        api_tokens: Vec<KanidmAPITokenStatus>,
        //token_secrets:
    ) -> Result<KanidmServiceAccountStatus> {
        let now = Utc::now();
        match service_account {
            Some(sa) => {
                let exist_condition = Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_TRUE.to_string(),
                    reason: "Exists".to_string(),
                    message: "Service account exists.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                };

                let current_service_account_attributes =
                    KanidmServiceAccountAttributes::from(sa.clone());

                let updated_condition = if self.spec.service_account_attributes
                    == current_service_account_attributes
                {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: REASON_ATTRIBUTES_MATCH.to_string(),
                        message: "Service account exists with desired attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                        message: "Service account exists with different attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                };

                let current_service_account_posix = KanidmAccountPosixAttributes::from(sa);
                let posix_initialized_condition =
                    if current_service_account_posix.gidnumber.is_some() {
                        Condition {
                            type_: TYPE_POSIX_INITIALIZED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: "PosixInitialized".to_string(),
                            message: "Service account exists with POSIX attributes.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_POSIX_INITIALIZED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: "PosixNotInitialized".to_string(),
                            message: "Service account exists without POSIX attributes.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    };

                let posix_updated_condition = self.spec.posix_attributes.as_ref().map(|posix| {
                    if posix == &current_service_account_posix {
                        Condition {
                            type_: TYPE_POSIX_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTES_MATCH.to_string(),
                            message: "Service account exists with desired POSIX attributes."
                                .to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_POSIX_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                            message: "Service account exists with different POSIX attributes."
                                .to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

                let api_tokens_condition = self.spec.api_tokens.as_ref().map(|tokens| {
                    let b_tree_spec_tokens = api_tokens
                        .clone()
                        .into_iter()
                        .map(KanidmAPIToken::from)
                        .collect::<BTreeSet<_>>();
                    if tokens == &b_tree_spec_tokens {
                        Condition {
                            type_: TYPE_API_TOKENS.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: "APITokensMatch".to_string(),
                            message: "API tokens exists with desired attributes.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_API_TOKENS.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: "APITokensNotMatch".to_string(),
                            message: "API tokens not matching desired attributes.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });

                let validity_condition = {
                    let valid = if let Some(valid_from) = current_service_account_attributes
                        .account_valid_from
                        .as_ref()
                    {
                        now > valid_from.0
                    } else {
                        true
                    } && if let Some(expire) =
                        current_service_account_attributes.account_expire.as_ref()
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
                let conditions = vec![
                    exist_condition,
                    updated_condition,
                    posix_initialized_condition,
                    validity_condition,
                ]
                .into_iter()
                .chain(api_tokens_condition)
                .chain(posix_updated_condition)
                .collect::<Vec<_>>();
                let status = conditions
                    .iter()
                    .filter(|c| c.type_ != TYPE_POSIX_INITIALIZED && c.type_ != TYPE_VALIDITY)
                    .all(|c| c.status == CONDITION_TRUE);
                Ok(KanidmServiceAccountStatus {
                    conditions: Some(conditions),
                    ready: status,
                    gid: current_service_account_posix.gidnumber,
                    kanidm_ref: self.kanidm_ref(),
                    api_tokens,
                })
            }
            None => {
                let conditions = vec![Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_FALSE.to_string(),
                    reason: "NotExists".to_string(),
                    message: "Person is not present.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                }];
                Ok(KanidmServiceAccountStatus {
                    conditions: Some(conditions),
                    ready: false,
                    gid: None,
                    kanidm_ref: self.kanidm_ref(),
                    api_tokens,
                })
            }
        }
    }
}
