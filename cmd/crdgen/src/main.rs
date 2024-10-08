use kaniop_kanidm::crd::Kanidm;

use kube::CustomResourceExt;

fn main() {
    // safe unwrap: we know CRD is serializable
    print!("---\n{}\n", serde_yaml::to_string(&Kanidm::crd()).unwrap())
}
