use crate::validator::HasKanidmRef;

use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::KanidmRef;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;

impl HasKanidmRef for KanidmGroup {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    fn kanidm_entity_name(&self) -> String {
        KanidmResource::kanidm_entity_name(self)
    }
}

impl HasKanidmRef for KanidmPersonAccount {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    fn kanidm_entity_name(&self) -> String {
        KanidmResource::kanidm_entity_name(self)
    }
}

impl HasKanidmRef for KanidmOAuth2Client {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    fn kanidm_entity_name(&self) -> String {
        KanidmResource::kanidm_entity_name(self)
    }
}

impl HasKanidmRef for KanidmServiceAccount {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    fn kanidm_entity_name(&self) -> String {
        KanidmResource::kanidm_entity_name(self)
    }
}
