use kaniop_group::crd::{KanidmGroup, KanidmGroupSpec};
use kaniop_operator::{crd::KanidmRef, kanidm::crd::Kanidm};

use kaniop_person::crd::KanidmPersonAccount;
use kube::{api::ObjectMeta, ResourceExt};
use schemars::{gen::SchemaGenerator, schema::RootSchema};

pub fn example(kanidm: &Kanidm, person: &KanidmPersonAccount) -> KanidmGroup {
    let name = "my-group";
    KanidmGroup {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: KanidmGroupSpec {
            kanidm_ref: KanidmRef {
                name: kanidm.name_any(),
                namespace: kanidm.namespace(),
            },
            members: Some(vec![person.name_any()]),
            entry_managed_by: Some(person.name_any()),
            mail: Some(vec![
                format!("{name}@{}", kanidm.spec.domain),
                format!("alias-{name}@{}", kanidm.spec.domain),
            ]),
            posix_attributes: Some(Default::default()),
        },
        status: Default::default(),
    }
}

pub fn schema(gen: &SchemaGenerator) -> RootSchema {
    gen.clone().into_root_schema_for::<KanidmGroup>()
}
