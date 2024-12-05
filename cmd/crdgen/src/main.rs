use kaniop_group::crd::KanidmGroup;
use kaniop_kanidm::crd::Kanidm;
use kaniop_person::crd::KanidmPersonAccount;

use kube::CustomResourceExt;

fn main() {
    for crd in vec![
        Kanidm::crd(),
        KanidmGroup::crd(),
        KanidmPersonAccount::crd(),
    ] {
        // safe unwrap: we know CRD is serializable
        print!("---\n{}\n", serde_yaml::to_string(&crd).unwrap());
    }
}
