use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kaniop_operator::{
    crd::{KanidmAccountPosixAttributes, KanidmRef},
    kanidm::crd::Kanidm,
};
use kaniop_person::crd::{KanidmPersonAccount, KanidmPersonAccountSpec, KanidmPersonAttributes};

use kube::{ResourceExt, api::ObjectMeta};

pub fn example(kanidm: &Kanidm) -> KanidmPersonAccount {
    let name = "me";
    KanidmPersonAccount {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: KanidmPersonAccountSpec {
            kanidm_ref: KanidmRef {
                name: kanidm.name_any(),
                namespace: kanidm.namespace(),
            },
            kanidm_name: None,
            person_attributes: KanidmPersonAttributes {
                displayname: "Me".to_string(),
                mail: Some(vec![
                    format!("{name}@{}", kanidm.spec.domain),
                    format!("alias-{name}@{}", kanidm.spec.domain),
                ]),
                legalname: Some("Me".to_string()),
                account_valid_from: Some(Time("2021-01-01T00:00:00Z".parse().unwrap())),
                account_expire: Some(Time("2030-01-01T00:00:00Z".parse().unwrap())),
            },
            posix_attributes: Some(KanidmAccountPosixAttributes {
                gidnumber: Some(1000),
                loginshell: Some("/bin/bash".to_string()),
            }),
            credentials_token_ttl: 3600,
        },
        status: Default::default(),
    }
}
