use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;

use kube::runtime::reflector::Store;

#[derive(Clone)]
pub struct WebhookState {
    pub group_store: Store<KanidmGroup>,
    pub person_store: Store<KanidmPersonAccount>,
    pub oauth2_store: Store<KanidmOAuth2Client>,
    pub service_account_store: Store<KanidmServiceAccount>,
}

impl WebhookState {
    pub fn new(
        group_store: Store<KanidmGroup>,
        person_store: Store<KanidmPersonAccount>,
        oauth2_store: Store<KanidmOAuth2Client>,
        service_account_store: Store<KanidmServiceAccount>,
    ) -> Self {
        Self {
            group_store,
            person_store,
            oauth2_store,
            service_account_store,
        }
    }
}
