use crate::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule, RetentionPolicySpec};

use kaniop_backup_core::retention::{
    BackupEntry, RetentionPolicy, parse_timestamp, select_deletion_candidates,
};
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{ControllerId, State, check_api_queryable, error_policy};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::reconcile::transport::backup_target_validation_error;
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
use serde::Serialize;
use tokio::time::Duration;
use tracing::{debug, info, warn};

pub const CONTROLLER_ID: ControllerId = "backup-schedule";
const REQUEUE_NORMAL: Duration = Duration::from_secs(300);
const REQUEUE_SUSPENDED: Duration = Duration::from_secs(600);

pub async fn run(state: State, client: Client) {
    let schedule = check_api_queryable::<KanidmBackupSchedule>(client.clone()).await;

    let ctx = Arc::new(state.to_context(client, CONTROLLER_ID));

    info!("starting {CONTROLLER_ID} controller");
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

fn validate_cron_schedule(schedule: &str) -> std::result::Result<(), String> {
    use cron::Schedule;
    use std::str::FromStr;

    if Schedule::from_str(schedule).is_ok() {
        return Ok(());
    }
    let with_seconds = format!("0 {schedule}");
    if Schedule::from_str(&with_seconds).is_ok() {
        return Ok(());
    }
    Err(format!(
        "invalid cron schedule '{schedule}': must be a valid 5-field or 6-field cron expression"
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleStatusPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_generation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_discovered_backup_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_successful_backup_time: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    conditions: Vec<Condition>,
}

fn transition_time(
    existing: &[Condition],
    type_: &str,
    new_status: &str,
    new_reason: &str,
) -> Time {
    existing
        .iter()
        .find(|c| c.type_ == type_ && c.status == new_status && c.reason == new_reason)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Time(Timestamp::now()))
}

async fn reconcile_schedule(
    obj: Arc<KanidmBackupSchedule>,
    ctx: Arc<kaniop_operator::controller::context::Context<KanidmBackupSchedule>>,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    debug!(%namespace, %name, "reconciling KanidmBackupSchedule");

    let spec = &obj.spec;

    if spec.schedule.is_empty() {
        return Err(Error::MissingData("schedule is required".to_string()));
    }

    if let Err(msg) = validate_cron_schedule(&spec.schedule) {
        let mut status = obj.status.clone().unwrap_or_default();
        status.observed_generation = obj.metadata.generation;
        let existing_conditions = status.conditions.clone();
        status.conditions.retain(|c| c.type_ != "Ready");
        status.conditions.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: transition_time(
                &existing_conditions,
                "Ready",
                "False",
                "InvalidSchedule",
            ),
            reason: "InvalidSchedule".to_string(),
            message: msg,
        });
        let patch_payload = ScheduleStatusPatch {
            observed_generation: status.observed_generation,
            last_discovered_backup_ref: status.last_discovered_backup_ref.clone(),
            last_successful_backup_time: status.last_successful_backup_time.clone(),
            conditions: status.conditions.clone(),
        };
        let api: Api<KanidmBackupSchedule> = Api::namespaced(ctx.client.clone(), &namespace);
        let patch = serde_json::json!({
            "apiVersion": "kaniop.rs/v1alpha1",
            "kind": "KanidmBackupSchedule",
            "status": patch_payload
        });
        api.patch_status(
            &name,
            &kube::api::PatchParams::apply(CONTROLLER_ID).force(),
            &kube::api::Patch::Apply(patch),
        )
        .await
        .map_err(|e| {
            Error::KubeError(
                format!("failed to patch status for {namespace}/{name}"),
                Box::new(e),
            )
        })?;
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_SUSPENDED),
            false,
        ));
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
    let backup_target_error = kanidm.as_deref().and_then(backup_target_validation_error);

    let effective_suspend = spec.suspend || restore_active;

    if spec.suspend && restore_active {
        warn!(
            namespace,
            name, "schedule is suspended and restore is in progress; transport will not run"
        );
    }

    let existing_conditions = status.conditions.clone();
    let mut conditions_to_set: Vec<Condition> = Vec::new();

    if kanidm.is_none() {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: transition_time(
                &existing_conditions,
                "Ready",
                "False",
                "KanidmNotFound",
            ),
            reason: "KanidmNotFound".to_string(),
            message: format!(
                "Referenced Kanidm '{}' does not exist in namespace '{}'",
                spec.kanidm_ref.name, namespace
            ),
        });
    } else if let Some(message) = backup_target_error.as_ref() {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: transition_time(
                &existing_conditions,
                "Ready",
                "False",
                "InvalidKanidmBackupTarget",
            ),
            reason: "InvalidKanidmBackupTarget".to_string(),
            message: message.clone(),
        });
    } else if repository.is_none() {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: transition_time(
                &existing_conditions,
                "Ready",
                "False",
                "RepositoryNotFound",
            ),
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
            last_transition_time: transition_time(
                &existing_conditions,
                "Ready",
                "False",
                "RepositoryConfigNotAccepted",
            ),
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
            last_transition_time: transition_time(&existing_conditions, "Ready", "False", reason),
            reason: reason.to_string(),
            message: message.to_string(),
        });
        conditions_to_set.push(Condition {
            type_: "Suspended".to_string(),
            status: "True".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: transition_time(
                &existing_conditions,
                "Suspended",
                "True",
                reason,
            ),
            reason: reason.to_string(),
            message: message.to_string(),
        });
    } else {
        conditions_to_set.push(Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: transition_time(&existing_conditions, "Ready", "True", "Configured"),
            reason: "Configured".to_string(),
            message: "Schedule is configured; online transport is rendered into Kanidm [online_backup] configuration only. No backup completion is claimed without an atomic commit contract.".to_string(),
        });
        conditions_to_set.push(Condition {
            type_: "TransportExperimental".to_string(),
            status: "True".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: transition_time(&existing_conditions, "TransportExperimental", "True", "NoCompletionContract"),
            reason: "NoCompletionContract".to_string(),
            message: "Online backup transport is experimental. Kanidm has no documented completion contract; Kaniop does not report production backup success.".to_string(),
        });
    }

    for cond in &conditions_to_set {
        status.conditions.retain(|c| c.type_ != cond.type_);
    }
    status.conditions.extend(conditions_to_set);

    let status_changed = obj.status.as_ref().is_none_or(|s| {
        s.observed_generation != status.observed_generation
            || s.conditions != status.conditions
            || s.last_discovered_backup_ref != status.last_discovered_backup_ref
            || s.last_successful_backup_time != status.last_successful_backup_time
    });

    if status_changed {
        let patch_payload = ScheduleStatusPatch {
            observed_generation: status.observed_generation,
            last_discovered_backup_ref: status.last_discovered_backup_ref.clone(),
            last_successful_backup_time: status.last_successful_backup_time.clone(),
            conditions: status.conditions.clone(),
        };
        let api: Api<KanidmBackupSchedule> = Api::namespaced(ctx.client.clone(), &namespace);
        let patch = serde_json::json!({
            "apiVersion": "kaniop.rs/v1alpha1",
            "kind": "KanidmBackupSchedule",
            "status": patch_payload
        });
        api.patch_status(
            &name,
            &kube::api::PatchParams::apply(CONTROLLER_ID).force(),
            &kube::api::Patch::Apply(patch),
        )
        .await
        .map_err(|e| {
            Error::KubeError(
                format!("failed to patch status for {namespace}/{name}"),
                Box::new(e),
            )
        })?;
    }

    let requeue = if effective_suspend {
        REQUEUE_SUSPENDED
    } else {
        REQUEUE_NORMAL
    };

    if !effective_suspend && repo_config_accepted && backup_target_error.is_none() {
        if let (Some(repo), Some(kanidm_obj)) = (&repository, &kanidm) {
            let kanidm_uid = &kanidm_obj.metadata.uid.clone().unwrap_or_default();
            if let Err(e) = reconcile_retention(&ctx, &obj, repo, kanidm_uid, &namespace).await {
                warn!(error = %e, namespace, name, "retention reconciliation failed");
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
        debug!(namespace, "retention: no deletion candidates");
        return Ok(());
    }

    info!(
        namespace,
        count = result.delete.len(),
        "retention: deleting candidates"
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
    use std::str::FromStr;

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
    fn schedule_status_patch_excludes_discovery() {
        let patch = ScheduleStatusPatch {
            observed_generation: Some(1),
            last_discovered_backup_ref: Some("kb-abc123".to_string()),
            last_successful_backup_time: Some("2024-01-01T00:00:00Z".to_string()),
            conditions: vec![Condition {
                type_: "Ready".to_string(),
                status: "True".to_string(),
                observed_generation: Some(1),
                last_transition_time: Time(Timestamp::now()),
                reason: "Configured".to_string(),
                message: "test".to_string(),
            }],
        };
        let json = serde_json::to_value(&patch).unwrap();
        assert!(
            json.get("discovery").is_none(),
            "schedule status patch must not include discovery; found keys: {:?}",
            json.as_object().unwrap().keys().collect::<Vec<_>>()
        );
        assert!(json.get("observedGeneration").is_some());
        assert!(json.get("conditions").is_some());
        assert!(json.get("lastDiscoveredBackupRef").is_some());
        assert!(json.get("lastSuccessfulBackupTime").is_some());
    }

    #[test]
    fn schedule_status_patch_omits_none_fields() {
        let patch = ScheduleStatusPatch {
            observed_generation: Some(1),
            last_discovered_backup_ref: None,
            last_successful_backup_time: None,
            conditions: vec![],
        };
        let json = serde_json::to_value(&patch).unwrap();
        assert!(json.get("discovery").is_none());
        assert!(json.get("lastDiscoveredBackupRef").is_none());
        assert!(json.get("lastSuccessfulBackupTime").is_none());
    }

    #[test]
    fn transition_time_preserves_when_unchanged() {
        let existing = vec![Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::from_str("2024-01-01T00:00:00Z").unwrap()),
            reason: "Configured".to_string(),
            message: "old message".to_string(),
        }];
        let result = transition_time(&existing, "Ready", "True", "Configured");
        assert_eq!(
            result,
            Time(Timestamp::from_str("2024-01-01T00:00:00Z").unwrap())
        );
    }

    #[test]
    fn transition_time_updates_when_status_changes() {
        let existing = vec![Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::from_str("2024-01-01T00:00:00Z").unwrap()),
            reason: "Configured".to_string(),
            message: "old".to_string(),
        }];
        let before = Timestamp::now();
        let result = transition_time(&existing, "Ready", "False", "KanidmNotFound");
        assert!(result.0 >= before);
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

    #[test]
    fn validate_cron_schedule_accepts_standard_5_field() {
        assert!(super::validate_cron_schedule("0 0 * * *").is_ok());
        assert!(super::validate_cron_schedule("*/15 * * * *").is_ok());
        assert!(super::validate_cron_schedule("0 0 1 JAN *").is_ok());
        assert!(super::validate_cron_schedule("0 0 * * MON-FRI").is_ok());
    }

    #[test]
    fn validate_cron_schedule_accepts_6_field_with_seconds() {
        assert!(super::validate_cron_schedule("0 0 0 * * *").is_ok());
        assert!(super::validate_cron_schedule("0 */15 * * * *").is_ok());
    }

    #[test]
    fn validate_cron_schedule_rejects_invalid() {
        assert!(super::validate_cron_schedule("not-a-cron").is_err());
        assert!(super::validate_cron_schedule("60 * * * *").is_err());
        assert!(super::validate_cron_schedule("@@@@@").is_err());
        assert!(super::validate_cron_schedule("").is_err());
    }
}
