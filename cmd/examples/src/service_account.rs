use std::collections::BTreeSet;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kaniop_operator::{
    crd::{KanidmAccountPosixAttributes, KanidmRef, SecretRotation},
    kanidm::crd::Kanidm,
};
use kaniop_service_account::crd::{
    KanidmAPIToken, KanidmApiTokenPurpose, KanidmServiceAccount, KanidmServiceAccountAttributes,
    KanidmServiceAccountSpec,
};

use kube::{ResourceExt, api::ObjectMeta};

pub fn example(kanidm: &Kanidm) -> KanidmServiceAccount {
    let name = "demo-service";
    KanidmServiceAccount {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: KanidmServiceAccountSpec {
            kanidm_ref: KanidmRef {
                name: kanidm.name_any(),
                namespace: kanidm.namespace(),
            },
            kanidm_name: None,
            service_account_attributes: KanidmServiceAccountAttributes {
                displayname: "Demo Service Account".to_string(),
                entry_managed_by: "my-group".to_string(),
                mail: Some(vec![
                    format!("{name}@{}", kanidm.spec.domain),
                    format!("alias-{name}@{}", kanidm.spec.domain),
                ]),
                account_valid_from: Some(Time("2021-01-01T00:00:00Z".parse().unwrap())),
                account_expire: Some(Time("2030-01-01T00:00:00Z".parse().unwrap())),
            },
            posix_attributes: Some(KanidmAccountPosixAttributes {
                gidnumber: Some(1000),
                loginshell: Some("/bin/bash".to_string()),
            }),
            api_tokens: Some(BTreeSet::from_iter(vec![KanidmAPIToken {
                label: "default-token".to_string(),
                purpose: KanidmApiTokenPurpose::ReadWrite,
                expiry: Some(Time("2024-01-01T00:00:00Z".parse().unwrap())),
                secret_name: Some("demo-service-token".to_string()),
            }])),
            generate_credentials: true,
            credentials_rotation: Some(SecretRotation {
                enabled: true,
                period_days: 90,
            }),
            api_token_rotation: Some(SecretRotation {
                enabled: true,
                period_days: 90,
            }),
        },
        status: Default::default(),
    }
}
