mod backup;
mod group;
mod kanidm;
mod kanidm_restore;
mod oauth2;
mod person;
mod service_account;
mod yaml;

use schemars::schema_for;
use yaml::{write_to_file, write_to_file_with_overrides};

fn main() {
    let kanidm = kanidm::example();
    let restore = kanidm_restore::example();
    let person = person::example(&kanidm);
    let group = group::example(&kanidm, &person);
    let oauth2 = oauth2::example();
    let service_account = service_account::example(&kanidm);
    let repository = backup::repository_example();
    let schedule = backup::schedule_example();
    let backup = backup::backup_example();

    // Generate schemas and serialize examples to YAML with comments
    let kanidm_schema = schema_for!(kaniop_operator::kanidm::crd::Kanidm);
    let kanidm_schema_json = serde_json::to_value(&kanidm_schema).unwrap();

    write_to_file_with_overrides(
        &kanidm,
        &kanidm_schema_json,
        "examples/kanidm.yaml",
        &[
            (
                "Requests describes the minimum amount of compute resources required",
                "Requests represents the minimum amount of storage the volume should have",
            ),
            (
                "for a\n  #         # container,",
                "for a\n  #         # volume,",
            ),
            (
                "https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/",
                "https://kubernetes.io/docs/concepts/storage/persistent-volumes#resources",
            ),
        ],
    )
    .unwrap();

    let restore_schema = schema_for!(kaniop_operator::kanidm::restore::KanidmRestore);
    let restore_schema_json = serde_json::to_value(&restore_schema).unwrap();
    write_to_file(
        &restore,
        &restore_schema_json,
        "examples/kanidm-restore.yaml",
    )
    .unwrap();

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

    let repository_schema = schema_for!(kaniop_backup::crd::KanidmBackupRepository);
    let repository_schema_json = serde_json::to_value(&repository_schema).unwrap();
    write_to_file(
        &repository,
        &repository_schema_json,
        "examples/backup-repository.yaml",
    )
    .unwrap();

    let schedule_schema = schema_for!(kaniop_backup::crd::KanidmBackupSchedule);
    let schedule_schema_json = serde_json::to_value(&schedule_schema).unwrap();
    write_to_file(
        &schedule,
        &schedule_schema_json,
        "examples/backup-schedule.yaml",
    )
    .unwrap();

    let backup_schema = schema_for!(kaniop_backup::crd::KanidmBackup);
    let backup_schema_json = serde_json::to_value(&backup_schema).unwrap();
    write_to_file(&backup, &backup_schema_json, "examples/backup.yaml").unwrap();
}
