use kaniop_oauth2::crd::{
    KanidmClaimMap, KanidmClaimMapJoinStrategy, KanidmClaimsValuesMap, KanidmOAuth2Client,
    KanidmOAuth2ClientSpec, KanidmScopeMap,
};
use kaniop_operator::crd::KanidmRef;

use std::collections::BTreeSet;

use kube::api::ObjectMeta;
use schemars::{gen::SchemaGenerator, schema::RootSchema};

pub fn example() -> KanidmOAuth2Client {
    KanidmOAuth2Client {
        metadata: ObjectMeta {
            name: Some("my-service".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: KanidmOAuth2ClientSpec {
            kanidm_ref: KanidmRef {
                name: "my-idm".to_string(),
                // namespace: Some("default".to_string()), // Uncomment if needed
                ..Default::default()
            },
            displayname: "My Service".to_string(),
            origin: "https://my-service.localhost".to_string(),
            redirect_url: vec!["https://my-service.localhost/oauth2/callback".to_string()],
            public: false,
            scope_map: Some(BTreeSet::from([KanidmScopeMap {
                group: "my-service-users".to_string(),
                scopes: vec![
                    "openid".to_string(),
                    "profile".to_string(),
                    "email".to_string(),
                ],
            }])),
            sup_scope_map: Some(BTreeSet::from([KanidmScopeMap {
                group: "my-service-admins".to_string(),
                scopes: vec!["admin".to_string()],
            }])),
            claim_map: Some(BTreeSet::from([KanidmClaimMap {
                name: "account_role".to_string(),
                values_map: BTreeSet::from([KanidmClaimsValuesMap {
                    group: "nextcloud_admins".to_string(),
                    values: vec!["admin".to_string(), "login".to_string()],
                }]),
                join_strategy: KanidmClaimMapJoinStrategy::Array,
            }])),
            strict_redirect_url: Some(true),
            prefer_short_username: Some(false),
            allow_localhost_redirect: Some(false),
            allow_insecure_client_disable_pkce: Some(false),
            jwt_legacy_crypto_enable: Some(false),
        },
        status: Default::default(),
    }
}

pub fn schema(gen: &SchemaGenerator) -> RootSchema {
    gen.clone().into_root_schema_for::<KanidmOAuth2Client>()
}
