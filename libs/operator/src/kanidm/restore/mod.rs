pub(crate) use super::{crd, reconcile};

mod legacy {
    pub(super) mod hardening {
        include!("cleanup.rs");
    }

    pub(super) mod controller {
        include!("controller.rs");
    }

    include!("legacy.rs");
}

pub use legacy::{
    BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION, CONTROLLER_ID,
    KanidmRestore, KanidmRestoreBackupRefSource, KanidmRestoreLocalSource, KanidmRestorePhase,
    KanidmRestoreSource, KanidmRestoreSpec, KanidmRestoreStatus, KanidmRestoreTargetRef,
    RESTORE_ANNOTATION, ReplicaCountEntry, SafetyBackupConfig, SafetyBackupRepositoryRef,
};

pub async fn run(client: kube::Client) {
    legacy::controller::run(client).await;
}
