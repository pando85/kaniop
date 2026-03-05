use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;

use kube::CustomResourceExt;

fn main() {
    for crd in [
        Kanidm::crd(),
        KanidmGroup::crd(),
        KanidmOAuth2Client::crd(),
        KanidmPersonAccount::crd(),
        KanidmServiceAccount::crd(),
    ] {
        // safe unwrap: we know CRD is serializable
        print!("---\n{}\n", serde_yaml::to_string(&crd).unwrap());
    }
}
