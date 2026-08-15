use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::controller::kanidm::KanidmClients;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;

use std::sync::Arc;

use kube::Client;
use kube::runtime::reflector::Store;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct WebhookState {
    pub group_store: Store<KanidmGroup>,
    pub person_store: Store<KanidmPersonAccount>,
    pub oauth2_store: Store<KanidmOAuth2Client>,
    pub service_account_store: Store<KanidmServiceAccount>,
    pub kube_client: Client,
    pub kanidm_clients: Arc<RwLock<KanidmClients>>,
}

impl WebhookState {
    pub fn new(
        group_store: Store<KanidmGroup>,
        person_store: Store<KanidmPersonAccount>,
        oauth2_store: Store<KanidmOAuth2Client>,
        service_account_store: Store<KanidmServiceAccount>,
        kube_client: Client,
    ) -> Self {
        Self {
            group_store,
            person_store,
            oauth2_store,
            service_account_store,
            kube_client,
            kanidm_clients: Arc::new(RwLock::new(KanidmClients::default())),
        }
    }
}
