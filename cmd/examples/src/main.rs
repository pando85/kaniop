mod group;
mod kanidm;
mod oauth2;
mod person;
mod service_account;
mod yaml;

use schemars::schema_for;
use yaml::write_to_file;

fn main() {
    let kanidm = kanidm::example();
    let person = person::example(&kanidm);
    let group = group::example(&kanidm, &person);
    let oauth2 = oauth2::example();
    let service_account = service_account::example(&kanidm);

    // Generate schemas and serialize examples to YAML with comments
    let kanidm_schema = schema_for!(kaniop_operator::kanidm::crd::Kanidm);
    let kanidm_schema_json = serde_json::to_value(&kanidm_schema).unwrap();

    write_to_file(&kanidm, &kanidm_schema_json, "examples/kanidm.yaml").unwrap();

    let person_schema = schema_for!(kaniop_person::crd::KanidmPersonAccount);
    let person_schema_json = serde_json::to_value(&person_schema).unwrap();
    write_to_file(&person, &person_schema_json, "examples/person.yaml").unwrap();

    let group_schema = schema_for!(kaniop_group::crd::KanidmGroup);
    let group_schema_json = serde_json::to_value(&group_schema).unwrap();
    write_to_file(&group, &group_schema_json, "examples/group.yaml").unwrap();

    let oauth2_schema = schema_for!(kaniop_oauth2::crd::KanidmOAuth2Client);
    let oauth2_schema_json = serde_json::to_value(&oauth2_schema).unwrap();
    write_to_file(&oauth2, &oauth2_schema_json, "examples/oauth2.yaml").unwrap();

    let service_account_schema = schema_for!(kaniop_service_account::crd::KanidmServiceAccount);
    let service_account_schema_json = serde_json::to_value(&service_account_schema).unwrap();
    write_to_file(
        &service_account,
        &service_account_schema_json,
        "examples/service-account.yaml",
    )
    .unwrap();
}
