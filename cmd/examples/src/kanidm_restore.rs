use kaniop_operator::kanidm::restore::{
    KanidmRestore, KanidmRestoreLocalSource, KanidmRestoreSource, KanidmRestoreSpec,
    KanidmRestoreTargetRef,
};
use kube::api::ObjectMeta;

pub fn example() -> KanidmRestore {
    KanidmRestore {
        metadata: ObjectMeta {
            name: Some("my-idm-restore".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: KanidmRestoreSpec {
            target_ref: KanidmRestoreTargetRef {
                name: "my-idm".to_string(),
                uid: "replace-with-kanidm-uid".to_string(),
            },
            source: KanidmRestoreSource {
                local: KanidmRestoreLocalSource {
                    file_name: "backup.json.gz".to_string(),
                },
            },
            restore_image: "kanidm/server:1.10.0".to_string(),
        },
        status: None,
    }
}
