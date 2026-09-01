use kaniop_group::crd::{
    CredentialTypeMinimum, KanidmGroup, KanidmGroupAccountPolicy, KanidmGroupSpec,
};
use kaniop_operator::{crd::KanidmRef, kanidm::crd::Kanidm};

use kaniop_person::crd::KanidmPersonAccount;
use kube::{ResourceExt, api::ObjectMeta};

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
            kanidm_name: None,
            members: Some(vec![person.name_any()]),
            entry_managed_by: Some(person.name_any()),
            mail: Some(vec![
                format!("{name}@{}", kanidm.spec.domain),
                format!("alias-{name}@{}", kanidm.spec.domain),
            ]),
            posix_attributes: Some(Default::default()),
            account_policy: Some(KanidmGroupAccountPolicy {
                auth_session_expiry: Some(86400),
                credential_type_minimum: Some(CredentialTypeMinimum::Mfa),
                password_minimum_length: Some(12),
                privilege_expiry: Some(900),
                webauthn_attestation_ca_list: None,
                allow_primary_cred_fallback: Some(false),
                limit_search_max_results: None,
                limit_search_max_filter_test: None,
            }),
        },
        status: Default::default(),
    }
}
