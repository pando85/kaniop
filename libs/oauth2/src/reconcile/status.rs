use super::{OAUTH2_OPERATOR_NAME, secret::SecretExt};

use crate::controller::Context;
use crate::crd::{
    KanidmClaimMap, KanidmOAuth2Client, KanidmOAuth2ClientStatus, KanidmScopeMap,
    OAuth2ClientImageStatus,
};

use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::rotation::needs_rotation as rotation_check;
use kaniop_k8s_util::types::{compare_urls, get_first_as_bool, get_first_cloned, normalize_url};
use kaniop_operator::controller::kanidm::KanidmResource;

use std::collections::BTreeSet;
use std::sync::Arc;

use futures::TryFutureExt;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kanidm_client::KanidmClient;
use kanidm_proto::constants::{
    ATTR_DISPLAYNAME, ATTR_OAUTH2_ALLOW_INSECURE_CLIENT_DISABLE_PKCE,
    ATTR_OAUTH2_ALLOW_LOCALHOST_REDIRECT, ATTR_OAUTH2_JWT_LEGACY_CRYPTO_ENABLE,
    ATTR_OAUTH2_PREFER_SHORT_USERNAME, ATTR_OAUTH2_RS_CLAIM_MAP, ATTR_OAUTH2_RS_ORIGIN,
    ATTR_OAUTH2_RS_ORIGIN_LANDING, ATTR_OAUTH2_RS_SCOPE_MAP, ATTR_OAUTH2_RS_SUP_SCOPE_MAP,
    ATTR_OAUTH2_STRICT_REDIRECT_URI,
};
use kanidm_proto::v1::Entry;
use kube::ResourceExt;
use kube::api::{Api, PartialObjectMeta, Patch, PatchParams};
use tracing::{debug, trace};

pub const TYPE_EXISTS: &str = "Exists";
pub const TYPE_SECRET_INITIALIZED: &str = "SecretInitialized";
pub const TYPE_SECRET_ROTATED: &str = "SecretRotated";
pub const TYPE_UPDATED: &str = "Updated";
pub const TYPE_REDIRECT_URL_UPDATED: &str = "RedirectUrlUpdated";
pub const TYPE_SCOPE_MAP_UPDATED: &str = "ScopeMapUpdated";
pub const TYPE_SUP_SCOPE_MAP_UPDATED: &str = "SupScopeMapUpdated";
pub const TYPE_CLAIMS_MAP_UPDATED: &str = "ClaimMapUpdated";
pub const TYPE_STRICT_REDIRECT_URL_UPDATED: &str = "StrictRedirectUrlUpdated";
pub const TYPE_DISABLE_PKCE_UPDATED: &str = "DisablePkceUpdated";
pub const TYPE_PREFER_SHORT_NAME_UPDATED: &str = "PreferShortNameUpdated";
pub const TYPE_ALLOW_LOCALHOST_REDIRECT_UPDATED: &str = "AllowLocalhostRedirectUpdated";
pub const TYPE_LEGACY_CRYPTO_UPDATED: &str = "LegacyCryptoUpdated";
pub const TYPE_IMAGE_UPDATED: &str = "ImageUpdated";
pub const CONDITION_TRUE: &str = "True";
pub const CONDITION_FALSE: &str = "False";
const REASON_ATTRIBUTE_MATCH: &str = "AttributeMatch";
const REASON_ATTRIBUTE_NOT_MATCH: &str = "AttributeNotMatch";
const REASON_ATTRIBUTES_MATCH: &str = "AttributesMatch";
const REASON_ATTRIBUTES_NOT_MATCH: &str = "AttributesNotMatch";
const REASON_ROTATION_NEEDED: &str = "RotationNeeded";
const REASON_ROTATION_NOT_NEEDED: &str = "RotationNotNeeded";

#[allow(async_fn_in_trait)]
pub trait StatusExt {
    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context>,
    ) -> Result<KanidmOAuth2ClientStatus>;
}

impl StatusExt for KanidmOAuth2Client {
    async fn update_status(
        &self,
        kanidm_client: Arc<KanidmClient>,
        ctx: Arc<Context>,
    ) -> Result<KanidmOAuth2ClientStatus> {
        // safe unwrap: person is namespaced scoped
        let namespace = self.get_namespace();
        let name = self.kanidm_entity_name();
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

        let (secret, secret_meta) = if self.spec.public {
            (None, None)
        } else {
            ctx.secret_store
                .find(|s| {
                    s.name_any() == self.secret_name() && s.namespace().as_ref() == Some(&namespace)
                })
                .map(|s| (Some(s.name_any()), Some(s)))
                .unwrap_or((None, None))
        };
        let current_image_status = self.status.as_ref().and_then(|s| s.image.clone());
        let status = self.generate_status(
            current_oauth2,
            secret,
            secret_meta.as_ref(),
            current_image_status,
        )?;
        let status_patch = Patch::Apply(KanidmOAuth2Client {
            status: Some(status.clone()),
            ..KanidmOAuth2Client::default()
        });
        debug!(msg = "updating status");
        trace!(msg = format!("status patch {:?}", status_patch));
        let patch = PatchParams::apply(OAUTH2_OPERATOR_NAME).force();
        let kanidm_api =
            Api::<KanidmOAuth2Client>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
        let _o = kanidm_api
            .patch_status(&name, &patch, &status_patch)
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to patch KanidmOAuth2Client/status {namespace}/{name}"),
                    Box::new(e),
                )
            })?;
        Ok(status)
    }
}

impl KanidmOAuth2Client {
    fn generate_status(
        &self,
        oauth2_opt: Option<Entry>,
        secret: Option<String>,
        secret_meta: Option<&Arc<PartialObjectMeta<Secret>>>,
        current_image_status: Option<OAuth2ClientImageStatus>,
    ) -> Result<KanidmOAuth2ClientStatus> {
        let now = Timestamp::now();
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

                let secret_initialized_condition = if self.spec.public {
                    None
                } else if secret.is_some() {
                    Some(Condition {
                        type_: TYPE_SECRET_INITIALIZED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "SecretExists".to_string(),
                        message: "Secret exists.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    })
                } else {
                    Some(Condition {
                        type_: TYPE_SECRET_INITIALIZED.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: "SecretNotExists".to_string(),
                        message: "Secret does not exist.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    })
                };
                // Check if secret rotation is needed (only for confidential clients with rotation enabled)
                let secret_rotated_condition = if self.spec.public {
                    None
                } else {
                    self.spec.secret_rotation.as_ref().and_then(|config| {
                        if !config.enabled {
                            return None;
                        }
                        match secret_meta {
                            Some(meta) => {
                                if rotation_check(meta, config.enabled, config.period_days) {
                                    Some(Condition {
                                        type_: TYPE_SECRET_ROTATED.to_string(),
                                        status: CONDITION_FALSE.to_string(),
                                        reason: REASON_ROTATION_NEEDED.to_string(),
                                        message: "Secret rotation period has elapsed.".to_string(),
                                        last_transition_time: Time(now),
                                        observed_generation: self.metadata.generation,
                                    })
                                } else {
                                    Some(Condition {
                                        type_: TYPE_SECRET_ROTATED.to_string(),
                                        status: CONDITION_TRUE.to_string(),
                                        reason: REASON_ROTATION_NOT_NEEDED.to_string(),
                                        message: "Secret is within rotation period.".to_string(),
                                        last_transition_time: Time(now),
                                        observed_generation: self.metadata.generation,
                                    })
                                }
                            }
                            // Secret doesn't exist yet, will be created with rotation annotations
                            None => None,
                        }
                    })
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
                let image_condition = match &self.spec.image {
                    None => Some(Condition {
                        type_: TYPE_IMAGE_UPDATED.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "NoImageRequired".to_string(),
                        message: "No image URL specified in spec.".to_string(),
                        last_transition_time: Time(now),
                        observed_generation: self.metadata.generation,
                    }),
                    Some(image_spec) => match &current_image_status {
                        Some(cached) if cached.url == image_spec.url => Some(Condition {
                            type_: TYPE_IMAGE_UPDATED.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: "ImageSynced".to_string(),
                            message: "Image URL matches cached status.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }),
                        _ => Some(Condition {
                            type_: TYPE_IMAGE_UPDATED.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: "ImageNeedsUpdate".to_string(),
                            message: "Image URL has changed or not yet synced.".to_string(),
                            last_transition_time: Time(now),
                            observed_generation: self.metadata.generation,
                        }),
                    },
                };
                vec![exist_condition, updated_condition, redirect_url_condition]
                    .into_iter()
                    .chain(secret_initialized_condition)
                    .chain(secret_rotated_condition)
                    .chain(scope_map_condition)
                    .chain(sup_scope_map_condition)
                    .chain(claims_map_condition)
                    .chain(strict_condition)
                    .chain(disable_pkce_condition)
                    .chain(prefer_short_name_condition)
                    .chain(allow_localhost_redirect_condition)
                    .chain(jwt_legacy_crypto_enable_condition)
                    .chain(image_condition)
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
        let status = conditions
            .clone()
            .iter()
            .all(|c| c.status == CONDITION_TRUE);

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
            ready: status,
            secret_name: secret,
            kanidm_ref: self.kanidm_ref(),
            image: current_image_status,
        })
    }
}
