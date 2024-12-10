use crate::crd::{KanidmClaimMap, KanidmOAuth2Client, KanidmOAuth2ClientStatus, KanidmScopeMap};

use kaniop_k8s_util::events::{Event, EventType};
use kaniop_k8s_util::types::{compare_urls, get_first_as_bool, get_first_cloned, normalize_url};
use kaniop_operator::controller::{Context, ContextKanidmClient, DEFAULT_RECONCILE_INTERVAL};
use kaniop_operator::error::{Error, Result};
use kaniop_operator::telemetry;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::future::TryJoinAll;
use futures::{try_join, TryFutureExt};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kanidm_client::KanidmClient;
use kanidm_proto::constants::{
    ATTR_DISPLAYNAME, ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE,
    ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT, ATTR_OAUTH2_JWT_LEGACY_CRYPTO_ENABLE,
    ATTR_OAUTH2_PREFER_SHORT_USERNAME, ATTR_OAUTH2_RS_CLAIM_MAP, ATTR_OAUTH2_RS_ORIGIN,
    ATTR_OAUTH2_RS_ORIGIN_LANDING, ATTR_OAUTH2_RS_SCOPE_MAP, ATTR_OAUTH2_RS_SUP_SCOPE_MAP,
    ATTR_OAUTH2_STRICT_REDIRECT_URI,
};
use kanidm_proto::v1::Entry;
use kube::api::{Api, Patch, PatchParams};
use kube::core::{Selector, SelectorExt};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{finalizer, Event as Finalizer};
use kube::runtime::reflector::ObjectRef;
use kube::{Resource, ResourceExt};
use tracing::{debug, field, info, instrument, trace, warn, Span};

pub static OAUTH2_OPERATOR_NAME: &str = "kanidmoauth2clients.kaniop.rs";
pub static OAUTH2_FINALIZER: &str = "kanidms.kaniop.rs/oauth2-client";

const TYPE_EXISTS: &str = "Exists";
const TYPE_UPDATED: &str = "Updated";
const TYPE_REDIRECT_URL_UPDATED: &str = "RedirectUrlUpdated";
const TYPE_SCOPE_MAP_UPDATED: &str = "ScopeMapUpdated";
const TYPE_SUP_SCOPE_MAP_UPDATED: &str = "SupScopeMapUpdated";
const TYPE_CLAIMS_MAP_UPDATED: &str = "ClaimMapUpdated";
const TYPE_STRICT_REDIRECT_URL_UPDATED: &str = "StrictRedirectUrlUpdated";
const TYPE_DISABLE_PKCE_UPDATED: &str = "DisablePkceUpdated";
const TYPE_PREFER_SHORT_NAME_UPDATED: &str = "PreferShortNameUpdated";
const TYPE_ALLOW_LOCALHOST_REDIRECT_UPDATED: &str = "AllowLocalhostRedirectUpdated";
const TYPE_LEGACY_CRYPTO_UPDATED: &str = "LegacyCryptoUpdated";
const REASON_ATTRIBUTE_MATCH: &str = "AttributeMatch";
const REASON_ATTRIBUTE_NOT_MATCH: &str = "AttributeNotMatch";
const REASON_ATTRIBUTES_MATCH: &str = "AttributesMatch";
const REASON_ATTRIBUTES_NOT_MATCH: &str = "AttributesNotMatch";
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

pub fn watched_resource(
    oauth2: &KanidmOAuth2Client,
    ctx: Arc<Context<KanidmOAuth2Client>>,
) -> bool {
    let namespace = oauth2.get_namespace();
    trace!(msg = "check if resource is watched");
    let kanidm = if let Some(k) = ctx.get_kanidm(oauth2) {
        k
    } else {
        trace!(msg = "no kanidm found");
        return false;
    };

    let namespace_selector = if let Some(l) = kanidm.spec.oauth2_client_namespace_selector.clone() {
        l
    } else {
        trace!(msg = "no namespace selector found, defaulting to current namespace");
        // A null label selector (default value) matches the current namespace only
        return kanidm.namespace().unwrap() == namespace;
    };

    let selector: Selector = if let Ok(s) = namespace_selector.try_into() {
        s
    } else {
        trace!(msg = "failed to parse namespace selector, defaulting to current namespace");
        return kanidm.namespace().unwrap() == namespace;
    };
    trace!(msg = "namespace selector", ?selector);
    ctx.namespace_store
        .state()
        .iter()
        .filter(|n| selector.matches(n.metadata.labels.as_ref().unwrap_or(&Default::default())))
        .any(|n| n.name_any() == namespace)
}

#[instrument(skip(ctx, oauth2))]
pub async fn reconcile_oauth2(
    oauth2: Arc<KanidmOAuth2Client>,
    ctx: Arc<Context<KanidmOAuth2Client>>,
) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    let kanidm_client = ctx.get_idm_client(&oauth2).await?;

    if !watched_resource(&oauth2, ctx.clone()) {
        debug!(msg = "resource not watched, skipping reconcile");
        ctx.recorder
        .publish(
            Event {
                type_: EventType::Warning,
                reason: "ResourceNotWatched".to_string(),
                note: Some("configure `oauth2ClientNamespaceSelector` on Kanidm resource to watch this namespace".to_string()),
                action: "Reconcile".to_string(),
                secondary: None,
            },
            &oauth2.object_ref(&()),
        )
        .await
        .map_err(|e| {
            warn!(msg = "failed to publish KanidmError event", %e);
            Error::KubeError("failed to publish event".to_string(), e)
        })?;
        return Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL));
    }

    info!(msg = "reconciling oauth2 client");
    let namespace = oauth2.get_namespace();
    let status = oauth2
        .update_status(kanidm_client.clone(), ctx.clone())
        .await?;
    let persons_api: Api<KanidmOAuth2Client> = Api::namespaced(ctx.client.clone(), &namespace);
    finalizer(&persons_api, OAUTH2_FINALIZER, oauth2, |event| async {
        match event {
            Finalizer::Apply(p) => p.reconcile(kanidm_client, status, ctx).await,
            Finalizer::Cleanup(p) => p.cleanup(kanidm_client, status, ctx).await,
        }
    })
    .await
    .map_err(|e| {
        Error::FinalizerError("failed on oauth2 client finalizer".to_string(), Box::new(e))
    })
}

impl KanidmOAuth2Client {
    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: oauth2 is namespaced scoped
        self.namespace().unwrap()
    }

    #[inline]
    async fn reconcile(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmOAuth2ClientStatus,
        ctx: Arc<Context<KanidmOAuth2Client>>,
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
        status: KanidmOAuth2ClientStatus,
    ) -> Result<Action> {
        let name = &self.name_any();
        let namespace = self.get_namespace();

        let mut require_status_update = false;

        // TODO: sync client secret

        if is_oauth2_false(TYPE_EXISTS, status.clone()) {
            self.create(&kanidm_client, name, &namespace).await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_UPDATED, status.clone()) {
            self.update(&kanidm_client, name, &namespace).await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_REDIRECT_URL_UPDATED, status.clone()) {
            self.update_redirect_url(&kanidm_client, name, &namespace, &status)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_SCOPE_MAP_UPDATED, status.clone()) {
            self.update_scope_map(&kanidm_client, name, &namespace, &status)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_SUP_SCOPE_MAP_UPDATED, status.clone()) {
            self.update_sup_scope_map(&kanidm_client, name, &namespace, &status)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_CLAIMS_MAP_UPDATED, status.clone()) {
            self.update_claims_map(&kanidm_client, name, &namespace, &status)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_STRICT_REDIRECT_URL_UPDATED, status.clone()) {
            self.update_strict_redirect_url(&kanidm_client, name, &namespace)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_DISABLE_PKCE_UPDATED, status.clone()) {
            self.update_disable_pkce(&kanidm_client, name, &namespace)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_PREFER_SHORT_NAME_UPDATED, status.clone()) {
            self.update_prefer_short_name(&kanidm_client, name, &namespace)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_ALLOW_LOCALHOST_REDIRECT_UPDATED, status.clone()) {
            self.update_allow_localhost_redirect(&kanidm_client, name, &namespace)
                .await?;
            require_status_update = true;
        }

        if is_oauth2_false(TYPE_LEGACY_CRYPTO_UPDATED, status.clone()) {
            self.update_legacy_crypto(&kanidm_client, name, &namespace)
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
        if self.spec.public {
            debug!(msg = "create public client");
            kanidm_client
                .idm_oauth2_rs_public_create(name, &self.spec.displayname, &self.spec.origin)
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
        } else {
            kanidm_client
                .idm_oauth2_rs_basic_create(name, &self.spec.displayname, &self.spec.origin)
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
        Ok(())
    }

    async fn update(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = "update");
        kanidm_client
            .idm_oauth2_rs_update(
                name,
                None,
                Some(&self.spec.displayname),
                Some(&self.spec.origin),
                false,
                false,
                false,
            )
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

    async fn update_redirect_url(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
        status: &KanidmOAuth2ClientStatus,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_RS_ORIGIN} attribute"));

        let current_urls: BTreeSet<_> = status
            .origin
            .as_ref()
            .map(|o| o.iter().map(|u| url::Url::parse(u).unwrap()).collect())
            .unwrap_or_default();

        let redirect_url = self
            .spec
            .redirect_url
            .iter()
            .map(|u| {
                url::Url::parse(u).map_err(|e| {
                    Error::ParseError(
                        format!(
                            "failed to parse redirect URL {u} for {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        e,
                    )
                })
            })
            .collect::<Result<BTreeSet<_>>>()?;

        let delete_futures = current_urls
            .difference(&redirect_url)
            .map(|url| kanidm_client.idm_oauth2_client_remove_origin(name, url))
            .collect::<TryJoinAll<_>>();

        let add_futures = redirect_url
            .difference(&current_urls)
            .map(|url| kanidm_client.idm_oauth2_client_add_origin(name, url))
            .collect::<TryJoinAll<_>>();

        futures::try_join!(delete_futures, add_futures).map_err(|e| {
            Error::KanidmClientError(
                format!(
                    "failed to modify {ATTR_OAUTH2_RS_ORIGIN} for {name} from {namespace}/{kanidm}",
                    kanidm = self.spec.kanidm_ref.name
                ),
                Box::new(e),
            )
        })?;

        Ok(())
    }

    async fn update_scope_map(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
        status: &KanidmOAuth2ClientStatus,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_RS_SCOPE_MAP} attribute"));

        let current_scope_map: BTreeSet<_> = status
            .scope_map
            .as_ref()
            .map(|v| v.iter().filter_map(|v| KanidmScopeMap::from(v)).collect())
            .unwrap_or_default();

        let scope_map: BTreeSet<KanidmScopeMap> = self
            .spec
            .scope_map
            .clone()
            .unwrap_or_default()
            .clone()
            .into_iter()
            .collect();

        let delete_futures = current_scope_map
            .difference(&scope_map)
            .map(|s| kanidm_client.idm_oauth2_rs_delete_scope_map(name, &s.group))
            .collect::<TryJoinAll<_>>();

        let add_futures = scope_map
            .difference(&current_scope_map)
            .map(|s| {
                kanidm_client.idm_oauth2_rs_update_scope_map(
                    name,
                    &s.group,
                    s.scopes.iter().map(|s| s.as_str()).collect(),
                )
            })
            .collect::<TryJoinAll<_>>();

        try_join!(delete_futures, add_futures).map_err(|e| {
            Error::KanidmClientError(
                format!(
                    "failed to modify {ATTR_OAUTH2_RS_SCOPE_MAP} for {name} from {namespace}/{kanidm}",
                    kanidm = self.spec.kanidm_ref.name
                ),
                Box::new(e),
            )
        })?;
        Ok(())
    }

    async fn update_sup_scope_map(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
        status: &KanidmOAuth2ClientStatus,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_RS_SUP_SCOPE_MAP} attribute"));

        let current_sup_scope_map: BTreeSet<_> = status
            .sup_scope_map
            .as_ref()
            .map(|v| v.iter().filter_map(|v| KanidmScopeMap::from(v)).collect())
            .unwrap_or_default();

        let sup_scope_map: BTreeSet<KanidmScopeMap> = self
            .spec
            .sup_scope_map
            .clone()
            .unwrap_or_default()
            .clone()
            .into_iter()
            .collect();

        let delete_futures = current_sup_scope_map
            .difference(&sup_scope_map)
            .map(|s| kanidm_client.idm_oauth2_rs_delete_sup_scope_map(name, &s.group))
            .collect::<TryJoinAll<_>>();

        let add_futures = sup_scope_map
            .difference(&current_sup_scope_map)
            .map(|s| {
                kanidm_client.idm_oauth2_rs_update_sup_scope_map(
                    name,
                    &s.group,
                    s.scopes.iter().map(|s| s.as_str()).collect(),
                )
            })
            .collect::<TryJoinAll<_>>();

        try_join!(delete_futures, add_futures).map_err(|e| {
            Error::KanidmClientError(
                format!(
                    "failed to modify {ATTR_OAUTH2_RS_SUP_SCOPE_MAP} for {name} from {namespace}/{kanidm}",
                    kanidm = self.spec.kanidm_ref.name
                ),
                Box::new(e),
            )
        })?;
        Ok(())
    }

    async fn update_claims_map(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
        status: &KanidmOAuth2ClientStatus,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_RS_CLAIM_MAP} attribute"));

        let current_claims_map: BTreeSet<_> = status
            .claims_map
            .as_ref()
            .map(|v| {
                KanidmClaimMap::group(
                    &v.iter()
                        .filter_map(|c| KanidmClaimMap::from(c))
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();

        let claims_map: BTreeSet<KanidmClaimMap> = self
            .spec
            .claim_map
            .clone()
            .unwrap_or_default()
            .clone()
            .into_iter()
            .collect();

        let delete_futures = current_claims_map
            .difference(&claims_map)
            .flat_map(|c| {
                c.values_map
                    .iter()
                    .map(|v| kanidm_client.idm_oauth2_rs_delete_claim_map(name, &c.name, &v.group))
            })
            .collect::<TryJoinAll<_>>();

        let claims_to_add = claims_map.difference(&current_claims_map);
        let add_futures = claims_to_add
            .clone()
            .flat_map(|c| {
                c.values_map.iter().map(|v| {
                    kanidm_client.idm_oauth2_rs_update_claim_map(name, &c.name, &v.group, &v.values)
                })
            })
            .collect::<TryJoinAll<_>>();

        let join_strategy_futures = claims_to_add
            .map(|c| {
                kanidm_client.idm_oauth2_rs_update_claim_map_join(
                    name,
                    &c.name,
                    c.join_strategy.to_oauth2_claim_map_join(),
                )
            })
            .collect::<TryJoinAll<_>>();

        try_join!(delete_futures, add_futures, join_strategy_futures).map_err(|e| {
            Error::KanidmClientError(
                format!(
                    "failed to modify {ATTR_OAUTH2_RS_CLAIM_MAP} for {name} from {namespace}/{kanidm}",
                    kanidm = self.spec.kanidm_ref.name
                ),
                Box::new(e),
            )
        })?;
        Ok(())
    }

    async fn update_strict_redirect_url(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_STRICT_REDIRECT_URI} attribute"));
        if let Some(strict_redirect_url_enabled) = self.spec.strict_redirect_url {
            if strict_redirect_url_enabled {
                kanidm_client
                    .idm_oauth2_rs_enable_strict_redirect_uri(
                        name,
                    )
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to update {ATTR_OAUTH2_STRICT_REDIRECT_URI} for {name} from {namespace}/{kanidm}",
                                kanidm = self.spec.kanidm_ref.name
                            ),
                            Box::new(e),
                        )
                    })?;
            } else {
                kanidm_client
                .idm_oauth2_rs_disable_strict_redirect_uri(
                    name,
                )
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {ATTR_OAUTH2_STRICT_REDIRECT_URI} for {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
            }
        };
        Ok(())
    }

    async fn update_disable_pkce(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE} attribute"));
        if let Some(disable_pkce) = self.spec.allow_insecure_client_disable_pkce {
            if disable_pkce {
                kanidm_client
                    .idm_oauth2_rs_disable_pkce(
                        name,
                    )
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to update {ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE} for {name} from {namespace}/{kanidm}",
                                kanidm = self.spec.kanidm_ref.name
                            ),
                            Box::new(e),
                        )
                    })?;
            } else {
                kanidm_client
                .idm_oauth2_rs_enable_pkce(
                    name,
                )
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE} for {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
            }
        };
        Ok(())
    }

    async fn update_prefer_short_name(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_PREFER_SHORT_USERNAME} attribute"));
        if let Some(prefer_short_username) = self.spec.prefer_short_username {
            if prefer_short_username {
                kanidm_client
                    .idm_oauth2_rs_prefer_short_username(
                        name,
                    )
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to update {ATTR_OAUTH2_PREFER_SHORT_USERNAME} for {name} from {namespace}/{kanidm}",
                                kanidm = self.spec.kanidm_ref.name
                            ),
                            Box::new(e),
                        )
                    })?;
            } else {
                kanidm_client
                .idm_oauth2_rs_prefer_spn_username(
                    name,
                )
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {ATTR_OAUTH2_PREFER_SHORT_USERNAME} for {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
            }
        };
        Ok(())
    }

    async fn update_allow_localhost_redirect(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT} attribute"));
        if let Some(allow_localhost_redirect) = self.spec.allow_localhost_redirect {
            if allow_localhost_redirect {
                kanidm_client
                    .idm_oauth2_rs_enable_public_localhost_redirect(
                        name,
                    )
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to update {ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT} for {name} from {namespace}/{kanidm}",
                                kanidm = self.spec.kanidm_ref.name
                            ),
                            Box::new(e),
                        )
                    })?;
            } else {
                kanidm_client
                .idm_oauth2_rs_disable_public_localhost_redirect(
                    name,
                )
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT} for {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
            }
        };
        Ok(())
    }

    async fn update_legacy_crypto(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        namespace: &str,
    ) -> Result<()> {
        debug!(msg = format!("update {ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT} attribute"));
        if let Some(legacy_crypto) = self.spec.jwt_legacy_crypto_enable {
            if legacy_crypto {
                kanidm_client
                    .idm_oauth2_rs_enable_legacy_crypto(
                        name,
                    )
                    .await
                    .map_err(|e| {
                        Error::KanidmClientError(
                            format!(
                                "failed to update {ATTR_OAUTH2_JWT_LEGACY_CRYPTO_ENABLE} for {name} from {namespace}/{kanidm}",
                                kanidm = self.spec.kanidm_ref.name
                            ),
                            Box::new(e),
                        )
                    })?;
            } else {
                kanidm_client
                .idm_oauth2_rs_disable_legacy_crypto(
                    name,
                )
                .await
                .map_err(|e| {
                    Error::KanidmClientError(
                        format!(
                            "failed to update {ATTR_OAUTH2_JWT_LEGACY_CRYPTO_ENABLE} for {name} from {namespace}/{kanidm}",
                            kanidm = self.spec.kanidm_ref.name
                        ),
                        Box::new(e),
                    )
                })?;
            }
        };
        Ok(())
    }

    async fn cleanup(
        &self,
        kanidm_client: Arc<KanidmClient>,
        status: KanidmOAuth2ClientStatus,
        ctx: Arc<Context<KanidmOAuth2Client>>,
    ) -> Result<Action> {
        let name = &self.name_any();
        let namespace = self.get_namespace();

        if is_oauth2(TYPE_EXISTS, status.clone()) {
            debug!(msg = "delete");
            kanidm_client
                .idm_oauth2_rs_delete(name)
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
                .remove(&ObjectRef::from(self));
        }
        Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
    }

    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context<KanidmOAuth2Client>>,
    ) -> Result<KanidmOAuth2ClientStatus> {
        // safe unwrap: person is namespaced scoped
        let namespace = self.get_namespace();
        let name = self.name_any();
        let current_oauth2 = kanidm_client
            .idm_oauth2_rs_get(&name)
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

        let status = self.generate_status(current_oauth2)?;
        let status_patch = Patch::Apply(KanidmOAuth2Client {
            status: Some(status.clone()),
            ..KanidmOAuth2Client::default()
        });
        debug!(msg = "updating status");
        trace!(msg = format!("status patch {:?}", status_patch));
        let patch = PatchParams::apply(OAUTH2_OPERATOR_NAME).force();
        let kanidm_api = Api::<KanidmOAuth2Client>::namespaced(ctx.client.clone(), &namespace);
        let _o = kanidm_api
            .patch_status(&name, &patch, &status_patch)
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to patch KanidmOAuth2Client/status {namespace}/{name}"),
                    e,
                )
            })?;
        Ok(status)
    }

    fn generate_status(&self, oauth2_opt: Option<Entry>) -> Result<KanidmOAuth2ClientStatus> {
        let now = Utc::now();
        let conditions = match oauth2_opt.clone() {
            Some(oauth2) => {
                let exist_condition = Condition {
                    type_: TYPE_EXISTS.to_string(),
                    status: CONDITION_TRUE.to_string(),
                    reason: "Exists".to_string(),
                    message: "OAuth2 client exists.".to_string(),
                    last_transition_time: Time(now),
                    observed_generation: self.metadata.generation,
                };

                let updated_condition = if Some(&self.spec.displayname)
                    == get_first_cloned(&oauth2, ATTR_DISPLAYNAME).as_ref()
                    && get_first_cloned(&oauth2, ATTR_OAUTH2_RS_ORIGIN_LANDING)
                        .map(|url| normalize_url(&self.spec.origin) == url)
                        .unwrap_or(false)
                {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: REASON_ATTRIBUTES_MATCH.to_string(),
                        message: "OAuth2 client exists with desired attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: REASON_ATTRIBUTES_NOT_MATCH.to_string(),
                        message: "OAuth2 client exists with different attributes.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                };

                let redirect_url_condition = if compare_urls(
                    &self.spec.redirect_url,
                    oauth2
                        .attrs
                        .get(ATTR_OAUTH2_RS_ORIGIN)
                        .unwrap_or(&Vec::new()),
                ) {
                    Condition {
                        type_: TYPE_REDIRECT_URL_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: REASON_ATTRIBUTE_MATCH.to_string(),
                        message: format!(
                            "OAuth2 client exists with desired {ATTR_OAUTH2_RS_ORIGIN} attribute."
                        ),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                } else {
                    Condition {
                        type_: TYPE_REDIRECT_URL_UPDATED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                        message: format!(
                            "OAuth2 client exists with different {ATTR_OAUTH2_RS_ORIGIN} attribute."
                        ),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }
                };
                let scope_map_condition = self.spec.scope_map.as_ref().map(|scope_map| {
                    let current_scope_map: BTreeSet<_> = oauth2
                        .attrs
                        .get(ATTR_OAUTH2_RS_SCOPE_MAP)
                        .map(|v| v.iter().filter_map(|v| KanidmScopeMap::from(v)).map(|s| s.normalize()).collect()).unwrap_or_default();
                    if current_scope_map == scope_map.iter().map(|s| s.clone().normalize()).collect::<BTreeSet<_>>()
                    {
                        Condition {
                            type_: TYPE_SCOPE_MAP_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_RS_SCOPE_MAP} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_SCOPE_MAP_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_RS_SCOPE_MAP} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let sup_scope_map_condition = self.spec.sup_scope_map.as_ref().map(|sup_scope_map| {
                    let current_sup_scope_map: BTreeSet<_> = oauth2
                        .attrs
                        .get(ATTR_OAUTH2_RS_SUP_SCOPE_MAP)
                        .map(|v| v.iter().filter_map(|v| KanidmScopeMap::from(v)).map(|s| s.normalize()).collect()).unwrap_or_default();
                    if current_sup_scope_map == sup_scope_map.iter().map(|s| s.clone().normalize()).collect::<BTreeSet<_>>()
                    {
                        Condition {
                            type_: TYPE_SUP_SCOPE_MAP_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_RS_SUP_SCOPE_MAP} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_SUP_SCOPE_MAP_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_RS_SUP_SCOPE_MAP} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let claims_map_condition = self.spec.claim_map.as_ref().map(|claims_map| {
                    let current_claims_map: BTreeSet<_> = oauth2
                        .attrs
                        .get(ATTR_OAUTH2_RS_CLAIM_MAP)
                        .map(|v| KanidmClaimMap::group(&v.iter().filter_map(|v| KanidmClaimMap::from(v)).map(|s| s.normalize()).collect::<Vec<_>>())).unwrap_or_default();
                    if current_claims_map == claims_map.iter().map(|s| s.clone().normalize()).collect::<BTreeSet<_>>()
                    {
                        Condition {
                            type_: TYPE_CLAIMS_MAP_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_RS_CLAIM_MAP} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_CLAIMS_MAP_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_RS_CLAIM_MAP} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let strict_condition = self.spec.strict_redirect_url.as_ref().map(|s| {
                    if Some(s)
                        == get_first_as_bool(&oauth2, ATTR_OAUTH2_STRICT_REDIRECT_URI).as_ref()
                    {
                        Condition {
                            type_: TYPE_STRICT_REDIRECT_URL_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_STRICT_REDIRECT_URI} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_STRICT_REDIRECT_URL_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_STRICT_REDIRECT_URI} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let disable_pkce_condition = self.spec.allow_insecure_client_disable_pkce.as_ref().map(|disable_pkce| {
                    if Some(disable_pkce) == get_first_as_bool(&oauth2, ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE).as_ref()
                    || (!disable_pkce && !oauth2.attrs.contains_key(ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE))
                    {
                        Condition {
                            type_: TYPE_DISABLE_PKCE_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_DISABLE_PKCE_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let prefer_short_name_condition = self.spec.prefer_short_username.as_ref().map(|prefer_short_name| {
                    if Some(prefer_short_name)
                        == get_first_as_bool(&oauth2, ATTR_OAUTH2_PREFER_SHORT_USERNAME).as_ref()
                    {
                        Condition {
                            type_: TYPE_PREFER_SHORT_NAME_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_PREFER_SHORT_USERNAME} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_PREFER_SHORT_NAME_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_PREFER_SHORT_USERNAME} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let allow_localhost_redirect_condition = self.spec.allow_localhost_redirect.as_ref().map(|allow_localhost_redirect| {
                    if Some(allow_localhost_redirect)
                        == get_first_as_bool(&oauth2, ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT).as_ref()
                    {
                        Condition {
                            type_: TYPE_ALLOW_LOCALHOST_REDIRECT_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_ALLOW_LOCALHOST_REDIRECT_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                let jwt_legacy_crypto_enable_condition = self.spec.jwt_legacy_crypto_enable.as_ref().map(|legacy_crypto| {
                    if Some(legacy_crypto)
                        == get_first_as_bool(&oauth2, ATTR_OAUTH2_JWT_LEGACY_CRYPTO_ENABLE).as_ref()
                    {
                        Condition {
                            type_: TYPE_LEGACY_CRYPTO_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: REASON_ATTRIBUTE_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with desired {ATTR_OAUTH2_JWT_LEGACY_CRYPTO_ENABLE} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_LEGACY_CRYPTO_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: REASON_ATTRIBUTE_NOT_MATCH.to_string(),
                            message: format!(
                                "OAuth2 client exists with different {ATTR_OAUTH2_JWT_LEGACY_CRYPTO_ENABLE} attribute."
                            ),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
                vec![exist_condition, updated_condition, redirect_url_condition]
                    .into_iter()
                    .chain(scope_map_condition)
                    .chain(sup_scope_map_condition)
                    .chain(claims_map_condition)
                    .chain(strict_condition)
                    .chain(disable_pkce_condition)
                    .chain(prefer_short_name_condition)
                    .chain(allow_localhost_redirect_condition)
                    .chain(jwt_legacy_crypto_enable_condition)
                    .collect()
            }
            None => vec![Condition {
                type_: TYPE_EXISTS.to_string(),
                status: CONDITION_FALSE.to_string(),
                reason: "NotExists".to_string(),
                message: "OAuth2 client is not present.".to_string(),
                last_transition_time: Time(now),
                observed_generation: self.metadata.generation,
            }],
        };
        Ok(KanidmOAuth2ClientStatus {
            conditions: Some(conditions),
            origin: oauth2_opt
                .clone()
                .and_then(|o| o.attrs.get(ATTR_OAUTH2_RS_ORIGIN).cloned()),
            scope_map: oauth2_opt
                .clone()
                .and_then(|o| o.attrs.get(ATTR_OAUTH2_RS_SCOPE_MAP).cloned()),
            sup_scope_map: oauth2_opt
                .clone()
                .and_then(|o| o.attrs.get(ATTR_OAUTH2_RS_SUP_SCOPE_MAP).cloned()),
            claims_map: oauth2_opt.and_then(|o| o.attrs.get(ATTR_OAUTH2_RS_CLAIM_MAP).cloned()),
        })
    }
}

pub fn is_oauth2(type_: &str, status: KanidmOAuth2ClientStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_TRUE)
}

pub fn is_oauth2_false(type_: &str, status: KanidmOAuth2ClientStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == type_ && c.status == CONDITION_FALSE)
}
