mod secret;
mod status;

use self::secret::{SecretExt, needs_rotation};
use self::status::{
    CONDITION_FALSE, CONDITION_TRUE, StatusExt, TYPE_API_TOKENS, TYPE_EXISTS,
    TYPE_POSIX_INITIALIZED, TYPE_POSIX_UPDATED, TYPE_UPDATED,
};

use crate::controller::Context;
use crate::crd::{
    KanidmAPIToken, KanidmApiTokenPurpose, KanidmServiceAccount, KanidmServiceAccountStatus,
};
use crate::reconcile::secret::{CREDENTIAL_LABEL, TOKEN_LABEL};
use crate::reconcile::status::TYPE_CREDENTIALS_INITIALIZED;

use kaniop_k8s_util::error::{Error, Result};
use kaniop_operator::controller::INSTANCE_LABEL;
use kaniop_operator::controller::context::KubeOperations;
use kaniop_operator::controller::kanidm::{KanidmResource, is_resource_watched};
use kaniop_operator::controller::{DEFAULT_RECONCILE_INTERVAL, context::IdmClientContext};
use kaniop_operator::telemetry;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Not;
use std::sync::Arc;
use std::time::Duration;

use futures::future::TryJoinAll;
use futures::{TryFutureExt, try_join};
use k8s_openapi::NamespaceResourceScope;
use kanidm_client::KanidmClient;
use kanidm_proto::constants::{ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM};
use kanidm_proto::v1::Entry;
use kube::api::Api;
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use kube::{Resource, ResourceExt};
use serde::{Deserialize, Serialize};
use tracing::{Span, debug, field, info, instrument, trace, warn};
use uuid::Uuid;

pub static SERVICE_ACCOUNT_OPERATOR_NAME: &str = "kanidmservicesaccounts.kaniop.rs";
pub static SERVICE_ACCOUNT_FINALIZER: &str = "kanidmservicesaccounts.kaniop.rs/finalizer";

pub fn watched_resource(service_account: &KanidmServiceAccount, ctx: Arc<Context>) -> bool {
    let kanidm = if let Some(k) = ctx.kaniop_ctx.get_kanidm(service_account) {
        k
    } else {
        trace!(msg = "no kanidm found");
        return false;
    };

    is_resource_watched(service_account, &kanidm, &ctx.kaniop_ctx.namespace_store)
}

#[instrument(skip(ctx, service_account))]
pub async fn reconcile_service_account(
    service_account: Arc<KanidmServiceAccount>,
    ctx: Arc<Context>,
) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx
        .kaniop_ctx
        .metrics
        .reconcile_count_and_measure(&trace_id);
    let kanidm_client = ctx.get_idm_client(&service_account).await?;

    if !watched_resource(&service_account, ctx.clone()) {
        debug!(msg = "resource not watched, skipping reconcile");
        ctx.kaniop_ctx
            .recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "ResourceNotWatched".to_string(),
                    note: Some("configure `serviceAccountNamespaceSelector` on Kanidm resource to watch this namespace".to_string()),
                    action: "Reconcile".to_string(),
                    secondary: None,
                },
                &service_account.object_ref(&()),
            )
            .await
            .map_err(|e| {
                warn!(msg = "failed to publish ResourceNotWatched event", %e);
                Error::KubeError("failed to publish event".to_string(), Box::new(e))
            })?;
        return Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL));
    }
    info!(msg = "reconciling service account");

    let namespace = service_account.get_namespace();
    let status = service_account
        .update_status(kanidm_client.clone(), ctx.clone())
        .await
        .map_err(|e| {
            debug!(msg = "failed to reconcile status", %e);
            ctx.kaniop_ctx.metrics.status_update_errors_inc();
            e
        })?;
    let service_accounts_api: Api<KanidmServiceAccount> =
        Api::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
    finalizer(
        &service_accounts_api,
        SERVICE_ACCOUNT_FINALIZER,
        service_account,
        |event| async {
            match event {
                Finalizer::Apply(p) => p.reconcile(kanidm_client, status, ctx).await,
                Finalizer::Cleanup(p) => p.cleanup(kanidm_client, status).await,
            }
        },
    )
    .await
    .map_err(|e| {
        Error::FinalizerError(
            "failed on service account finalizer".to_string(),
            Box::new(e),
        )
    })
}

impl KanidmServiceAccount {
    // Convenience methods that handle context and operator name
    pub async fn delete<K>(&self, ctx: &Context, resource: &K) -> Result<()>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        self.kube_delete(
            ctx.kaniop_ctx.client.clone(),
            &ctx.kaniop_ctx.metrics,
            resource,
        )
        .await
    }

    pub async fn patch<K>(&self, ctx: &Context, resource: K) -> Result<K>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        self.kube_patch(
            ctx.clone().kaniop_ctx.client.clone(),
            &ctx.kaniop_ctx.metrics,
            resource,
            SERVICE_ACCOUNT_OPERATOR_NAME,
        )
        .await
    }

    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: service account is namespaced scoped
        self.namespace().unwrap()
    }

    #[inline]
    async fn reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmServiceAccountStatus,
        ctx: Arc<Context>,
    ) -> Result<Action> {
        match self
            .internal_reconcile(kanidm_client, status, ctx.clone())
            .await
        {
            Ok(action) => Ok(action),
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
                            warn!(msg = "failed to publish KanidmError event", %e);
                            Error::KubeError("failed to publish event".to_string(), Box::new(e))
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
        status: KanidmServiceAccountStatus,
        ctx: Arc<Context>,
    ) -> Result<Action> {
        let name = &self.name_any();

        let mut require_status_update = false;
        if is_service_account_false(TYPE_EXISTS, status.clone()) {
            self.create(&kanidm_client, name).await?;
            require_status_update = true;
        }
        if is_service_account_false(TYPE_UPDATED, status.clone()) {
            self.update(&kanidm_client, name).await?;
            require_status_update = true;
        }

        if is_service_account_false(TYPE_POSIX_UPDATED, status.clone())
            || (is_service_account_false(TYPE_POSIX_INITIALIZED, status.clone())
                && is_service_account(TYPE_POSIX_UPDATED, status.clone()))
        {
            self.update_posix_attributes(&kanidm_client, name).await?;
            require_status_update = true;
        }

        self.clean_undesired_secrets(ctx.clone()).await?;

        // Check if any API token secrets need rotation
        let api_token_needs_rotation = self.check_api_tokens_rotation(&ctx);

        if is_service_account_false(TYPE_API_TOKENS, status.clone()) || api_token_needs_rotation {
            if api_token_needs_rotation {
                info!(msg = "rotating API tokens due to rotation policy");
            }
            self.update_api_tokens(&kanidm_client, name, &status, ctx.clone())
                .await?;
            require_status_update = true;
        }

        // Check if credentials secret needs to be generated or rotated
        let should_generate_credentials =
            is_service_account_false(TYPE_CREDENTIALS_INITIALIZED, status.clone());

        let should_rotate_credentials = {
            let secret_state = ctx.secret_store.state();
            secret_state
                .iter()
                .find(|secret| {
                    secret.metadata.labels.as_ref().is_some_and(|l| {
                        l.get(INSTANCE_LABEL) == Some(&self.name_any())
                            && l.get(CREDENTIAL_LABEL) == Some(&self.name_any())
                    })
                })
                .map(|s| needs_rotation(s, self.spec.credentials_rotation.as_ref()))
                .unwrap_or(false)
        };

        if should_generate_credentials || should_rotate_credentials {
            if should_rotate_credentials {
                info!(msg = "rotating credentials secret due to rotation policy");
            }
            let secret = self
                .generate_credentials_secret(
                    &kanidm_client,
                    self.spec.credentials_rotation.as_ref(),
                )
                .await?;
            self.patch(&ctx, secret).await?;
            require_status_update = true;
        } else if is_service_account_missing_type(TYPE_CREDENTIALS_INITIALIZED, status.clone())
            && !self.spec.generate_credentials
        {
            if let Some(secret) = ctx.secret_store.state().iter().find(|secret| {
                secret.metadata.labels.as_ref().is_some_and(|l| {
                    l.get(INSTANCE_LABEL) == Some(&self.name_any())
                        && l.get(CREDENTIAL_LABEL) == Some(&self.name_any())
                })
            }) {
                self.delete(&ctx, secret.as_ref()).await?;
            }
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
            .idm_service_account_create(
                name,
                &self.spec.service_account_attributes.displayname,
                &self.spec.service_account_attributes.entry_managed_by,
            )
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

    async fn update(&self, kanidm_client: &KanidmClient, name: &str) -> Result<()> {
        debug!(msg = "update");
        trace!(
            msg = format!(
                "update service account attributes {:?}",
                self.spec.service_account_attributes
            )
        );
        kanidm_client
            .idm_service_account_update(
                name,
                None,
                Some(&self.spec.service_account_attributes.displayname),
                Some(&self.spec.service_account_attributes.entry_managed_by),
                self.spec.service_account_attributes.mail.as_deref(),
            )
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to update {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        let mut update_entry = Entry {
            attrs: BTreeMap::new(),
        };
        if let Some(account_expire) = self.spec.service_account_attributes.account_expire.as_ref() {
            update_entry.attrs.insert(
                ATTR_ACCOUNT_EXPIRE.to_string(),
                vec![account_expire.0.to_rfc3339()],
            );
        }
        if let Some(account_valid_from) = self
            .spec
            .service_account_attributes
            .account_valid_from
            .as_ref()
        {
            update_entry.attrs.insert(
                ATTR_ACCOUNT_VALID_FROM.to_string(),
                vec![account_valid_from.0.to_rfc3339()],
            );
        }

        if update_entry.attrs.is_empty().not() {
            let _: Entry = kanidm_client
                .perform_patch_request(&format!("/v1/service_account/{name}"), update_entry)
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {name} from {namespace}/{kanidm}",
                            namespace = self.kanidm_namespace(),
                            kanidm = self.kanidm_name(),
                        ),
                        Box::new(e),
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
        debug!(msg = "update posix attributes");
        trace!(msg = format!("update posix attributes {:?}", self.spec.posix_attributes));
        kanidm_client
            .idm_service_account_unix_extend(
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
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        Ok(())
    }

    async fn update_api_tokens(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        status: &KanidmServiceAccountStatus,
        ctx: Arc<Context>,
    ) -> Result<()> {
        debug!(msg = "update API tokens");
        let api_tokens = self.spec.api_tokens.clone().unwrap_or_default();
        trace!(msg = format!("API tokens to update: {:?}", api_tokens));

        let tokens_to_rotate = match self
            .spec
            .api_token_rotation
            .as_ref()
            .filter(|config| config.enabled)
        {
            Some(rotation_config) => ctx
                .secret_store
                .state()
                .iter()
                .filter(|secret| {
                    secret.metadata.labels.as_ref().is_some_and(|l| {
                        l.get(INSTANCE_LABEL) == Some(&self.name_any())
                            && l.get(TOKEN_LABEL).is_some()
                    })
                })
                .filter(|secret| needs_rotation(secret.as_ref(), Some(rotation_config)))
                .filter_map(|secret| {
                    secret
                        .metadata
                        .labels
                        .as_ref()
                        .and_then(|l| l.get(TOKEN_LABEL))
                        .cloned()
                })
                .collect::<BTreeSet<_>>(),
            None => BTreeSet::new(),
        };

        let delete_futures: TryJoinAll<_> = status
            .api_tokens
            .clone()
            .into_iter()
            .filter(|t| {
                tokens_to_rotate.contains(&t.label)
                    || !api_tokens.contains(&KanidmAPIToken::from(t.clone()))
            })
            .map(|t| {
                let token_id = Uuid::parse_str(&t.token_id).map_err(|e| {
                    Error::ParseError(format!(
                        "This should never happen, please report a bug: invalid UUID '{}' for token '{}': {e}",
                        t.token_id, t.label
                    ))
                })?;
                Ok(kanidm_client
                    .idm_service_account_destroy_api_token(name, token_id)
                    .map_err(move |e| {
                        Error::KanidmClientError(
                            format!("failed to delete API token '{}'", t.label),
                            Box::new(e),
                        )
                    }))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .collect();

        let api_tokens_set = status
            .api_tokens
            .clone()
            .into_iter()
            .map(KanidmAPIToken::from)
            .collect::<BTreeSet<_>>();
        trace!(msg = format!("API tokens present: {:?}", &api_tokens_set));

        let tokens_to_create = api_tokens
            .difference(&api_tokens_set)
            .cloned()
            .chain(
                api_tokens
                    .iter()
                    .filter(|t| tokens_to_rotate.contains(&t.label))
                    .cloned(),
            )
            .collect::<BTreeSet<_>>();

        // Ensure we never try to create the same label twice.
        let add_futures = tokens_to_create
            .iter()
            .map(|t| {
                let expiry = t.expiry.as_ref().and_then(|time| {
                    time::OffsetDateTime::from_unix_timestamp(time.0.timestamp()).ok()
                });
                let label = t.label.clone();
                let secret_name = t.secret_name.clone();
                kanidm_client
                    .idm_service_account_generate_api_token(
                        name,
                        &t.label,
                        expiry,
                        t.purpose == KanidmApiTokenPurpose::ReadWrite,
                    )
                    .map_ok(move |token| (token, label, secret_name))
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!("failed to create API token '{}'", t.label),
                            Box::new(e),
                        )
                    })
            })
            .collect::<TryJoinAll<_>>();

        // Delete first, then (re)create to avoid label collisions in Kanidm.
        // Kanidm enforces unique token labels per service account, so we must destroy
        // existing tokens before creating new ones with the same label during rotation.
        // This cannot be parallelized with try_join! as it would cause label conflicts.
        delete_futures.await?;
        let add_results = add_futures.await?;

        let secret_futures = add_results
            .iter()
            .map(|(token, label, secret_name)| {
                self.generate_token_secret(
                    label,
                    token,
                    secret_name.as_deref(),
                    self.spec.api_token_rotation.as_ref(),
                )
            })
            .map(|secret| self.patch(&ctx, secret))
            .collect::<TryJoinAll<_>>();
        secret_futures.await?;
        Ok(())
    }

    /// Check if any API token secrets need rotation based on rotation policy.
    fn check_api_tokens_rotation(&self, ctx: &Arc<Context>) -> bool {
        let rotation_config = match &self.spec.api_token_rotation {
            Some(config) if config.enabled => config,
            _ => return false,
        };

        // Check all token secrets managed by this service account
        ctx.secret_store
            .state()
            .iter()
            .filter(|secret| {
                secret.metadata.labels.as_ref().is_some_and(|l| {
                    l.get(INSTANCE_LABEL) == Some(&self.name_any()) && l.get(TOKEN_LABEL).is_some()
                })
            })
            .any(|secret| needs_rotation(secret.as_ref(), Some(rotation_config)))
    }

    async fn clean_undesired_secrets(&self, ctx: Arc<Context>) -> Result<()> {
        let desired_secrets = self
            .spec
            .api_tokens
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|t| match t.secret_name {
                Some(s) => s,
                None => self.generate_token_secret_name(&t.label),
            })
            .collect::<BTreeSet<_>>();
        let undesired_secrets = ctx
            .secret_store
            .state()
            .into_iter()
            .filter(|secret| {
                secret.metadata.labels.as_ref().is_some_and(|l| {
                    l.get(INSTANCE_LABEL) == Some(&self.name_any())
                        && l.get(TOKEN_LABEL).is_some()
                        && !desired_secrets.contains(&secret.name_any())
                })
            })
            .collect::<Vec<_>>();
        let delete_secrets_future = undesired_secrets
            .iter()
            .map(|s| self.delete(&ctx, s.as_ref()))
            .collect::<TryJoinAll<_>>();
        try_join!(delete_secrets_future)?;
        Ok(())
    }

    async fn cleanup(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmServiceAccountStatus,
    ) -> Result<Action> {
        let name = &self.name_any();

        if is_service_account(TYPE_EXISTS, status.clone()) {
            debug!(msg = "delete");
            kanidm_client
                .idm_service_account_delete(name)
                .await
                .map_err(|e| {
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
}

pub fn is_service_account(type_: &str, status: KanidmServiceAccountStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_TRUE)
}

pub fn is_service_account_false(type_: &str, status: KanidmServiceAccountStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_FALSE)
}

pub fn is_service_account_missing_type(type_: &str, status: KanidmServiceAccountStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .all(|c| c.type_ != type_)
}
