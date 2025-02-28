use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kaniop_operator::{
    crd::{KanidmPersonPosixAttributes, KanidmRef},
    kanidm::crd::Kanidm,
};
use kaniop_person::crd::{KanidmPersonAccount, KanidmPersonAccountSpec, KanidmPersonAttributes};

use kube::{ResourceExt, api::ObjectMeta};
use schemars::{r#gen::SchemaGenerator, schema::RootSchema};

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
            posix_attributes: Some(KanidmPersonPosixAttributes {
                gidnumber: Some(1000),
                loginshell: Some("/bin/bash".to_string()),
            }),
        },
        status: Default::default(),
    }
}

pub fn schema(generator: &SchemaGenerator) -> RootSchema {
    generator
        .clone()
        .into_root_schema_for::<KanidmPersonAccount>()
}
