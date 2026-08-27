use crate::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule, RetentionPolicySpec};

use kaniop_backup_core::retention::{
    BackupEntry, RetentionPolicy, parse_timestamp, select_deletion_candidates,
};
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{ControllerId, State, check_api_queryable, error_policy};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::RESTORE_ANNOTATION;

use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kaniop_k8s_util::error::{Error, Result};
use kube::api::ListParams;
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::watcher::Config;
use kube::{Api, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, info, warn};

pub const CONTROLLER_ID: ControllerId = "backup-schedule";
const REQUEUE_NORMAL: Duration = Duration::from_secs(300);
const REQUEUE_SUSPENDED: Duration = Duration::from_secs(600);

pub async fn run(state: State, client: Client) {
    let schedule = check_api_queryable::<KanidmBackupSchedule>(client.clone()).await;

    let ctx = Arc::new(state.to_context(client, CONTROLLER_ID));

    info!(msg = format!("starting {CONTROLLER_ID} controller"));
    let schedule_controller = Controller::new(schedule, Config::default().any_semantic())
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_schedule),
            error_policy,
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.metrics.ready_set(1);
    tokio::join!(schedule_controller);
}

async fn check_unique_kanidm_target(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackupSchedule>,
    schedule: &KanidmBackupSchedule,
) -> Result<()> {
    let namespace = schedule.namespace().unwrap_or_default();
    let kanidm_ref = &schedule.spec.kanidm_ref.name;

    let api: Api<KanidmBackupSchedule> = Api::namespaced(ctx.client.clone(), &namespace);
    let all_schedules = api.list(&ListParams::default()).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list schedules in {namespace}"),
            Box::new(e),
        )
    })?;

    let conflict = all_schedules
        .items
        .iter()
        .find(|other| other.uid() != schedule.uid() && other.spec.kanidm_ref.name == *kanidm_ref);

    if let Some(conflicting) = conflict {
        return Err(Error::MissingData(format!(
            "only one KanidmBackupSchedule may target Kanidm '{kanidm_ref}' in namespace '{namespace}'; conflicting schedule: {}",
            conflicting.name_any()
        )));
    }

    Ok(())
}

async fn check_repository_exists(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackupSchedule>,
    schedule: &KanidmBackupSchedule,
) -> Result<Option<KanidmBackupRepository>> {
    let namespace = schedule.namespace().unwrap_or_default();
    let repo_name = &schedule.spec.repository_ref.name;

    let api: Api<KanidmBackupRepository> = Api::namespaced(ctx.client.clone(), &namespace);
    match api.get(repo_name).await {
        Ok(repo) => Ok(Some(repo)),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(Error::KubeError(
            format!("failed to get repository {namespace}/{repo_name}"),
            Box::new(e),
        )),
    }
}

async fn check_kanidm_exists(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackupSchedule>,
    schedule: &KanidmBackupSchedule,
) -> Result<Option<Arc<Kanidm>>> {
    let namespace = schedule.namespace().unwrap_or_default();
    let kanidm_name = &schedule.spec.kanidm_ref.name;

    let api: Api<Kanidm> = Api::namespaced(ctx.client.clone(), &namespace);
    match api.get(kanidm_name).await {
        Ok(kanidm) => Ok(Some(Arc::new(kanidm))),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(Error::KubeError(
            format!("failed to get Kanidm {namespace}/{kanidm_name}"),
            Box::new(e),
        )),
    }
}

fn is_restore_in_progress(kanidm: &Kanidm) -> bool {
    kanidm.annotations().contains_key(RESTORE_ANNOTATION)
}

fn is_repository_config_accepted(repo: &KanidmBackupRepository) -> bool {
    repo.status
        .as_ref()
        .and_then(|s| s.conditions.iter().find(|c| c.type_ == "Ready"))
        .is_some_and(|c| c.status == "True" && c.reason == "Accepted")
}

async fn reconcile_schedule(
    obj: Arc<KanidmBackupSchedule>,
    ctx: Arc<kaniop_operator::controller::context::Context<KanidmBackupSchedule>>,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    debug!(msg = "reconciling KanidmBackupSchedule", %namespace, %name);

    let spec = &obj.spec;

    if spec.schedule.is_empty() {
        return Err(Error::MissingData("schedule is required".to_string()));
    }

    if spec.local_versions < 2 {
        return Err(Error::MissingData(
            "localVersions must be at least 2".to_string(),
        ));
    }

    if spec.concurrency_policy != "Forbid" {
        return Err(Error::MissingData(
            "concurrencyPolicy must be Forbid".to_string(),
        ));
    }

    check_unique_kanidm_target(&ctx, &obj).await?;

    let repository = check_repository_exists(&ctx, &obj).await?;
    let repo_config_accepted = repository
        .as_ref()
        .is_some_and(is_repository_config_accepted);
    let kanidm = check_kanidm_exists(&ctx, &obj).await?;

    let mut status = obj.status.clone().unwrap_or_default();
    status.observed_generation = obj.metadata.generation;

    let restore_active = kanidm.as_ref().is_some_and(|k| is_restore_in_progress(k));

    let effective_suspend = spec.suspend || restore_active;

    if spec.suspend && restore_active {
        warn!(
            msg = "schedule is suspended and restore is in progress; transport will not run",
            namespace, name,
        );
    }

    let mut conditions_to_set: Vec<Condition> = Vec::new();

    if kanidm.is_none() {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "KanidmNotFound".to_string(),
            message: format!(
                "Referenced Kanidm '{}' does not exist in namespace '{}'",
                spec.kanidm_ref.name, namespace
            ),
        });
    } else if repository.is_none() {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "RepositoryNotFound".to_string(),
            message: format!(
                "Referenced repository '{}' does not exist in namespace '{}'",
                spec.repository_ref.name, namespace
            ),
        });
    } else if !repo_config_accepted {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "RepositoryConfigNotAccepted".to_string(),
            message: format!(
                "Referenced repository '{}' configuration has not been accepted in namespace '{}'",
                spec.repository_ref.name, namespace
            ),
        });
    } else if effective_suspend {
        let reason = if restore_active {
            "SuspendedRestore"
        } else {
            "Suspended"
        };
        let message = if restore_active {
            "Schedule is suspended because a restore is in progress on the target Kanidm"
        } else {
            "Schedule is suspended by spec"
        };
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: reason.to_string(),
            message: message.to_string(),
        });
        conditions_to_set.push(Condition {
            type_: "Suspended".to_string(),
            status: "True".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: reason.to_string(),
            message: message.to_string(),
        });
    } else {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "Configured".to_string(),
            message: "Schedule is configured; online transport is rendered into Kanidm [online_backup] configuration only. No backup completion is claimed without an atomic commit contract.".to_string(),
        });
        conditions_to_set.push(Condition {
            type_: "TransportExperimental".to_string(),
            status: "True".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "NoCompletionContract".to_string(),
            message: "Online backup transport is experimental. Kanidm has no documented completion contract; Kaniop does not report production backup success.".to_string(),
        });
    }

    for cond in &conditions_to_set {
        status.conditions.retain(|c| c.type_ != cond.type_);
    }
    status.conditions.extend(conditions_to_set);

    let api: Api<KanidmBackupSchedule> = Api::namespaced(ctx.client.clone(), &namespace);
    let patch = serde_json::json!({
        "apiVersion": "kaniop.rs/v1alpha1",
        "kind": "KanidmBackupSchedule",
        "status": status
    });
    api.patch_status(
        &name,
        &kube::api::PatchParams::apply(CONTROLLER_ID),
        &kube::api::Patch::Apply(patch),
    )
    .await
    .map_err(|e| {
        Error::KubeError(
            format!("failed to patch status for {namespace}/{name}"),
            Box::new(e),
        )
    })?;

    let requeue = if effective_suspend {
        REQUEUE_SUSPENDED
    } else {
        REQUEUE_NORMAL
    };

    if !effective_suspend && repo_config_accepted {
        if let (Some(repo), Some(kanidm_obj)) = (&repository, &kanidm) {
            let kanidm_uid = &kanidm_obj.metadata.uid.clone().unwrap_or_default();
            if let Err(e) = reconcile_retention(&ctx, &obj, repo, kanidm_uid, &namespace).await {
                warn!(msg = "retention reconciliation failed", error = %e, namespace, name);
            }
        }
    }

    Ok((
        kube::runtime::controller::Action::requeue(requeue),
        !effective_suspend && repo_config_accepted && kanidm.is_some(),
    ))
}

fn retention_policy_from_spec(spec: &RetentionPolicySpec) -> RetentionPolicy {
    let min_age_hours = parse_duration_hours(&spec.min_age).unwrap_or(24);
    RetentionPolicy {
        keep_last: spec.keep_last,
        daily: spec.daily,
        weekly: spec.weekly,
        monthly: spec.monthly,
        min_age_hours,
    }
}

fn parse_duration_hours(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_suffix('h') {
        h.parse().ok()
    } else if let Some(d) = s.strip_suffix('d') {
        d.parse::<u32>().ok().map(|d| d * 24)
    } else {
        s.parse().ok()
    }
}

fn safety_retention_hours(repo: &KanidmBackupRepository) -> u32 {
    repo.spec
        .limits
        .as_ref()
        .and_then(|l| parse_duration_hours(&l.safety_backup_min_retention))
        .unwrap_or(720)
}

async fn reconcile_retention(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackupSchedule>,
    schedule: &KanidmBackupSchedule,
    repository: &KanidmBackupRepository,
    kanidm_uid: &str,
    namespace: &str,
) -> Result<()> {
    let retention_spec = schedule
        .spec
        .retention
        .clone()
        .unwrap_or(RetentionPolicySpec {
            keep_last: 8,
            daily: 7,
            weekly: 4,
            monthly: 12,
            min_age: "24h".to_string(),
        });
    let policy = retention_policy_from_spec(&retention_spec);
    let safety_hours = safety_retention_hours(repository);

    let backup_api: Api<KanidmBackup> = Api::namespaced(ctx.client.clone(), namespace);
    let all_backups = backup_api
        .list(
            &ListParams::default()
                .labels(&format!("kaniop.rs/repository={}", repository.name_any())),
        )
        .await
        .map_err(|e| {
            Error::KubeError(
                format!("failed to list backups for retention in {namespace}"),
                Box::new(e),
            )
        })?;

    let restore_active = {
        let kanidm_api: Api<Kanidm> = Api::namespaced(ctx.client.clone(), namespace);
        match kanidm_api.get(&schedule.spec.kanidm_ref.name).await {
            Ok(k) => k.annotations().contains_key(RESTORE_ANNOTATION),
            Err(_) => false,
        }
    };

    let now = chrono::Utc::now().naive_utc();

    let entries: Vec<BackupEntry> = all_backups
        .items
        .iter()
        .filter(|b| b.spec.kanidm_ref.uid == kanidm_uid)
        .filter_map(|b| {
            let status = b.status.as_ref()?;
            let created_str = status.created_at.as_deref()?;
            let created_at = parse_timestamp(created_str)?;
            Some(BackupEntry {
                id: b.name_any(),
                created_at,
                consistency: status.consistency.clone().unwrap_or_default(),
                reason: status.reason.clone().unwrap_or_default(),
                referenced_by_active_restore: restore_active,
                safety_backup_min_retention_hours: if status.reason.as_deref()
                    == Some("restore-safety")
                {
                    Some(safety_hours)
                } else {
                    None
                },
            })
        })
        .collect();

    let result = select_deletion_candidates(&entries, &policy, &now);

    if result.delete.is_empty() {
        debug!(msg = "retention: no deletion candidates", namespace);
        return Ok(());
    }

    info!(
        msg = "retention: deleting candidates",
        namespace,
        count = result.delete.len()
    );

    for backup_name in &result.delete {
        match backup_api.delete(backup_name, &Default::default()).await {
            Ok(_) => {
                info!(backup = %backup_name, "retention: deleted backup CR");
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => {}
            Err(e) => {
                warn!(backup = %backup_name, error = %e, "retention: failed to delete backup CR");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
    use k8s_openapi::jiff::Timestamp;
    use kaniop_backup_core::crd::{
        AuthMethod, KanidmBackupRepositorySpec, RepositoryAuthentication, S3Config, SecretRef,
    };
    use kube::api::ObjectMeta;

    fn make_repo_with_condition(
        status: Option<crate::crd::KanidmBackupRepositoryStatus>,
    ) -> KanidmBackupRepository {
        KanidmBackupRepository {
            metadata: ObjectMeta {
                name: Some("test-repo".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "bucket".to_string(),
                    prefix: "prefix".to_string(),
                    region: None,
                    endpoint: None,
                    force_path_style: false,
                    insecure: false,
                    ca_bundle_ref: None,
                },
                authentication: RepositoryAuthentication {
                    writer: AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(SecretRef {
                            name: "w".to_string(),
                        }),
                    },
                    reader: AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                    deleter: AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                },
                encryption: None,
                limits: None,
            },
            status,
        }
    }

    #[test]
    fn is_repository_config_accepted_true_when_accepted() {
        let status = crate::crd::KanidmBackupRepositoryStatus {
            conditions: vec![Condition {
                type_: "Ready".to_string(),
                status: "True".to_string(),
                reason: "Accepted".to_string(),
                message: "Repository configuration accepted".to_string(),
                last_transition_time: Time(Timestamp::now()),
                observed_generation: None,
            }],
            ..Default::default()
        };
        let repo = make_repo_with_condition(Some(status));
        assert!(is_repository_config_accepted(&repo));
    }

    #[test]
    fn is_repository_config_accepted_false_when_no_status() {
        let repo = make_repo_with_condition(None);
        assert!(!is_repository_config_accepted(&repo));
    }

    #[test]
    fn is_repository_config_accepted_false_when_wrong_reason() {
        let status = crate::crd::KanidmBackupRepositoryStatus {
            conditions: vec![Condition {
                type_: "Ready".to_string(),
                status: "True".to_string(),
                reason: "SomeOtherReason".to_string(),
                message: String::new(),
                last_transition_time: Time(Timestamp::now()),
                observed_generation: None,
            }],
            ..Default::default()
        };
        let repo = make_repo_with_condition(Some(status));
        assert!(!is_repository_config_accepted(&repo));
    }

    #[test]
    fn is_repository_config_accepted_false_when_status_false() {
        let status = crate::crd::KanidmBackupRepositoryStatus {
            conditions: vec![Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                reason: "Accepted".to_string(),
                message: String::new(),
                last_transition_time: Time(Timestamp::now()),
                observed_generation: None,
            }],
            ..Default::default()
        };
        let repo = make_repo_with_condition(Some(status));
        assert!(!is_repository_config_accepted(&repo));
    }

    #[test]
    fn is_repository_config_accepted_false_when_empty_conditions() {
        let status = crate::crd::KanidmBackupRepositoryStatus {
            conditions: vec![],
            ..Default::default()
        };
        let repo = make_repo_with_condition(Some(status));
        assert!(!is_repository_config_accepted(&repo));
    }

    #[test]
    fn parse_duration_hours_variants() {
        assert_eq!(parse_duration_hours("24h"), Some(24));
        assert_eq!(parse_duration_hours("720h"), Some(720));
        assert_eq!(parse_duration_hours("30d"), Some(720));
        assert_eq!(parse_duration_hours("0h"), Some(0));
        assert_eq!(parse_duration_hours("invalid"), None);
    }

    #[test]
    fn retention_policy_from_spec_defaults() {
        let spec = RetentionPolicySpec {
            keep_last: 8,
            daily: 7,
            weekly: 4,
            monthly: 12,
            min_age: "24h".to_string(),
        };
        let policy = retention_policy_from_spec(&spec);
        assert_eq!(policy.keep_last, 8);
        assert_eq!(policy.min_age_hours, 24);
    }

    #[test]
    fn retention_active_restore_protects_all_entries() {
        use chrono::Utc;
        let now = Utc::now().naive_utc();
        let entries = vec![
            BackupEntry {
                id: "old".to_string(),
                created_at: now - chrono::Duration::days(90),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: true,
                safety_backup_min_retention_hours: None,
            },
            BackupEntry {
                id: "new".to_string(),
                created_at: now - chrono::Duration::hours(1),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
        ];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"old".to_string()));
        assert!(!result.delete.contains(&"old".to_string()));
    }

    #[test]
    fn retention_safety_backup_protected_within_window() {
        use chrono::Utc;
        let now = Utc::now().naive_utc();
        let entries = vec![
            BackupEntry {
                id: "safety".to_string(),
                created_at: now - chrono::Duration::hours(10),
                consistency: "kanidm-offline".to_string(),
                reason: "restore-safety".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: Some(720),
            },
            BackupEntry {
                id: "normal".to_string(),
                created_at: now - chrono::Duration::hours(10),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
        ];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"safety".to_string()));
        assert!(result.delete.contains(&"normal".to_string()));
    }

    #[test]
    fn retention_empty_entries_no_deletions() {
        use chrono::Utc;
        let now = Utc::now().naive_utc();
        let policy = RetentionPolicy::default();
        let result = select_deletion_candidates(&[], &policy, &now);
        assert!(result.delete.is_empty());
    }

    #[test]
    fn table_test_retention_keep_last_boundaries() {
        use chrono::Utc;
        let now = Utc::now().naive_utc();
        for keep in [0, 1, 3, 5, 10] {
            let entries: Vec<BackupEntry> = (0..5)
                .map(|i| BackupEntry {
                    id: format!("b{i}"),
                    created_at: now - chrono::Duration::days(i as i64 + 1),
                    consistency: "kanidm-offline".to_string(),
                    reason: "scheduled".to_string(),
                    referenced_by_active_restore: false,
                    safety_backup_min_retention_hours: None,
                })
                .collect();
            let policy = RetentionPolicy {
                keep_last: keep,
                daily: 0,
                weekly: 0,
                monthly: 0,
                min_age_hours: 0,
            };
            let result = select_deletion_candidates(&entries, &policy, &now);
            let retained_count = result.retain.len();
            let expected = (keep as usize).min(5);
            assert_eq!(
                retained_count, expected,
                "keep_last={keep} should retain {expected}, got {retained_count}"
            );
        }
    }
}
