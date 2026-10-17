use crate::controller::backup::{
    delete_params_with_uid_precondition, is_discovery_owned, is_precondition_conflict,
    job_start_time,
};
use crate::controller::{
    BACKUP_JOB_TTL_SECONDS, DISCOVER_LOG_LIMIT_BYTES, DISCOVERED_LABEL, DISCOVERY_CONTROLLER_ID,
    RESULT_PATH, background_delete_params, build_discover_data_mover_wrapper, data_mover_image,
    default_resource_requirements, extract_framed_result, hardened_pod_security_context,
    hardened_security_context, select_succeeded_pod,
};
use crate::crd::{
    BackupKanidmRef, BackupRepositoryRef, DiscoveryStatus, KanidmBackup, KanidmBackupPhase,
    KanidmBackupRepository, KanidmBackupSchedule, KanidmBackupSpec,
};

use kaniop_backup_core::auth::{
    AuthRole, build_auth_env_vars, build_auth_volume_mounts, build_auth_volumes,
    build_ca_bundle_volume, build_ca_bundle_volume_mount, ca_bundle_env_var, ca_bundle_path,
};
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{DiscoverResult, ExitCode, ResultDocument};
use kaniop_operator::controller::{ControllerId, State, backup_discovery_stale_threshold};
use kaniop_operator::kanidm::crd::Kanidm;

use std::collections::HashSet;
use std::sync::Arc;

use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, ObjectMeta, Time};
use k8s_openapi::jiff::Timestamp;
use kaniop_k8s_util::error::{Error, Result};
use kube::api::{ListParams, LogParams, Patch, PatchParams};
use kube::client::Client;
use kube::{Api, ResourceExt};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

pub const CONTROLLER_ID: ControllerId = "backup-discovery";
const DISCOVER_JOB_PREFIX: &str = "kaniop-backup-discover";
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(300);
const DEFAULT_DISCOVER_MAX_RESULTS: u32 = 1000;
#[cfg(test)]
const STALE_THRESHOLD: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
struct ScanTickCounters {
    repos_scanned: usize,
    schedules_processed: usize,
    jobs_created: usize,
    jobs_completed: usize,
    backups_discovered: u64,
}

#[derive(Clone)]
pub struct DiscoveryMetrics {
    scan_duration: Histogram<f64>,
    scan_failures: Counter<u64>,
    discover_jobs_created: Counter<u64>,
    backups_discovered: Counter<u64>,
    backups_reconciled: Counter<u64>,
    last_scan_timestamp: Gauge<i64>,
    last_scan_success_timestamp: Gauge<i64>,
    repositories_scanned: Gauge<i64>,
    truncations: Counter<u64>,
    dangling_pruned: Counter<u64>,
}

impl DiscoveryMetrics {
    pub fn new(meter: &Meter) -> Self {
        let scan_duration = meter
            .f64_histogram("backup_discovery_scan_duration_seconds")
            .with_description("Duration of discovery scan loops in seconds")
            .with_boundaries(vec![1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0])
            .build();

        let scan_failures = meter
            .u64_counter("backup_discovery_scan_failures")
            .with_description("Total number of discovery scan loop failures")
            .build();

        let discover_jobs_created = meter
            .u64_counter("backup_discovery_jobs_created")
            .with_description("Total number of discover Jobs created")
            .build();

        let backups_discovered = meter
            .u64_counter("backup_discovery_backups_discovered")
            .with_description("Total number of new backup CRs created from discovery")
            .build();

        let backups_reconciled = meter
            .u64_counter("backup_discovery_backups_reconciled")
            .with_description("Total number of existing backup CRs reconciled from discovery")
            .build();

        let last_scan_timestamp = meter
            .i64_gauge("backup_discovery_last_scan_timestamp")
            .with_description("Unix timestamp of the last discovery scan attempt")
            .build();

        let last_scan_success_timestamp = meter
            .i64_gauge("backup_discovery_last_scan_success_timestamp")
            .with_description("Unix timestamp of the last successful discovery scan")
            .build();

        let repositories_scanned = meter
            .i64_gauge("backup_discovery_repositories_scanned")
            .with_description("Number of repositories scanned in the last discovery loop")
            .build();

        let truncations = meter
            .u64_counter("backup_discovery_truncations")
            .with_description("Total number of discovery scans that returned truncated results")
            .build();

        let dangling_pruned = meter
            .u64_counter("backup_discovery_dangling_pruned")
            .with_description(
                "Total number of discovered backup CRs pruned after their manifest disappeared",
            )
            .build();

        Self {
            scan_duration,
            scan_failures,
            discover_jobs_created,
            backups_discovered,
            backups_reconciled,
            last_scan_timestamp,
            last_scan_success_timestamp,
            repositories_scanned,
            truncations,
            dangling_pruned,
        }
    }

    fn record_scan_duration(&self, secs: f64) {
        self.scan_duration.record(secs, &[]);
    }

    fn inc_scan_failures(&self) {
        self.scan_failures.add(1, &[]);
    }

    fn inc_discover_jobs(&self, repository: &str) {
        self.discover_jobs_created
            .add(1, &[KeyValue::new("repository", repository.to_string())]);
    }

    fn inc_backups_discovered(&self, repository: &str, count: u64) {
        self.backups_discovered.add(
            count,
            &[KeyValue::new("repository", repository.to_string())],
        );
    }

    fn inc_backups_reconciled(&self, repository: &str, count: u64) {
        self.backups_reconciled.add(
            count,
            &[KeyValue::new("repository", repository.to_string())],
        );
    }

    fn set_last_scan(&self, ts: i64) {
        self.last_scan_timestamp.record(ts, &[]);
    }

    fn set_last_scan_success(&self, ts: i64) {
        self.last_scan_success_timestamp.record(ts, &[]);
    }

    fn set_repositories_scanned(&self, count: i64) {
        self.repositories_scanned.record(count, &[]);
    }

    fn inc_truncations(&self, repository: &str) {
        self.truncations
            .add(1, &[KeyValue::new("repository", repository.to_string())]);
    }

    fn inc_dangling_pruned(&self, repository: &str, count: u64) {
        self.dangling_pruned.add(
            count,
            &[KeyValue::new("repository", repository.to_string())],
        );
    }
}

pub async fn run(state: State, client: Client, scan_interval: Option<Duration>) {
    let interval = scan_interval.unwrap_or(DEFAULT_SCAN_INTERVAL);
    let stale_threshold = backup_discovery_stale_threshold();
    let meter = opentelemetry::global::meter("kaniop");
    let metrics = Arc::new(DiscoveryMetrics::new(&meter));

    info!(
        controller = CONTROLLER_ID,
        interval_secs = interval.as_secs(),
        stale_secs = stale_threshold.as_secs(),
        "starting controller loop"
    );

    loop {
        let scan_start = std::time::Instant::now();
        let now_ts = Timestamp::now();
        metrics.set_last_scan(now_ts.as_second());

        match run_discovery_scan(&state, &client, &metrics).await {
            Ok(counters) => {
                let elapsed = scan_start.elapsed().as_secs_f64();
                metrics.record_scan_duration(elapsed);
                metrics.set_repositories_scanned(counters.repos_scanned as i64);
                let success_ts = Timestamp::now();
                metrics.set_last_scan_success(success_ts.as_second());
                info!(
                    repos_scanned = counters.repos_scanned,
                    schedules_processed = counters.schedules_processed,
                    jobs_created = counters.jobs_created,
                    jobs_completed = counters.jobs_completed,
                    backups_discovered = counters.backups_discovered,
                    elapsed_secs = elapsed,
                    "discovery scan tick completed"
                );
            }
            Err(e) => {
                let elapsed = scan_start.elapsed().as_secs_f64();
                metrics.record_scan_duration(elapsed);
                metrics.inc_scan_failures();
                error!(error = %e, "discovery scan failed");
            }
        }

        tokio::time::sleep(interval).await;
    }
}

async fn run_discovery_scan(
    state: &State,
    client: &Client,
    metrics: &DiscoveryMetrics,
) -> Result<ScanTickCounters> {
    let repos = list_accepted_repositories(client).await?;
    let mut counters = ScanTickCounters::default();
    let repos_scanned = repos.len();

    for repo in &repos {
        let namespace = repo.namespace().unwrap_or_default();
        let repo_name = repo.name_any();

        let schedules = list_schedules_for_repository(client, &namespace, &repo_name).await?;

        for schedule in &schedules {
            if schedule.spec.suspend {
                debug!(
                    namespace,
                    schedule = schedule.name_any(),
                    "skipping suspended schedule"
                );
                continue;
            }

            counters.schedules_processed += 1;

            let kanidm = get_kanidm_for_schedule(state, client, &namespace, schedule).await?;
            let Some(kanidm) = kanidm else {
                debug!(
                    namespace,
                    schedule = schedule.name_any(),
                    "kanidm not found for schedule; skipping"
                );
                continue;
            };

            // `namespaceUid` in the operation document carries the namespace *name*, not the
            // Namespace object UID: the producers (`libs/operator/src/kanidm/reconcile/transport.rs`
            // and restore's `legacy.rs`) write `kanidm.namespace()` into the `tenants/` segment, so
            // this consumer must agree or every existing object is orphaned (issue #1084).
            let namespace_name = kanidm.metadata.namespace.as_deref().unwrap_or_default();
            let kanidm_uid = kanidm.metadata.uid.as_deref().unwrap_or_default();

            if namespace_name.is_empty() || kanidm_uid.is_empty() {
                warn!(
                    namespace,
                    kanidm = kanidm.name_any(),
                    "kanidm missing namespace or uid; skipping discovery"
                );
                continue;
            }

            // Discovery writes the discover Job into this namespace, so the Namespace object has
            // to exist and carry a UID before the scan proceeds. A missing Namespace (404) or one
            // whose metadata.uid is not set yet skips discovery for this schedule.
            let ns_obj = get_namespace_object(client, &namespace).await?;
            let ns_uid = ns_obj
                .map(|ns| ns.metadata.uid.unwrap_or_default())
                .unwrap_or_default();

            if ns_uid.is_empty() {
                warn!(namespace, "namespace uid not available; skipping discovery");
                continue;
            }

            match process_discovery_for_schedule(
                client,
                &namespace,
                repo,
                schedule,
                &kanidm.name_any(),
                namespace_name,
                kanidm_uid,
                metrics,
            )
            .await
            {
                Ok(tick) => {
                    counters.jobs_created += tick.jobs_created;
                    counters.jobs_completed += tick.jobs_completed;
                    counters.backups_discovered += tick.backups_discovered;
                }
                Err(e) => {
                    warn!(
                        namespace,
                        schedule = schedule.name_any(),
                        error = %e,
                        "discovery processing failed for schedule"
                    );
                }
            }
        }
    }

    counters.repos_scanned = repos_scanned;
    Ok(counters)
}

async fn list_accepted_repositories(client: &Client) -> Result<Vec<KanidmBackupRepository>> {
    let api: Api<KanidmBackupRepository> = Api::all(client.clone());
    let all = api
        .list(&ListParams::default())
        .await
        .map_err(|e| Error::KubeError("failed to list repositories".to_string(), Box::new(e)))?;

    let accepted = all
        .items
        .into_iter()
        .filter(|r| {
            r.status
                .as_ref()
                .and_then(|s| s.conditions.iter().find(|c| c.type_ == "Ready"))
                .is_some_and(|c| c.status == "True" && c.reason == "Accepted")
        })
        .collect();

    Ok(accepted)
}

async fn list_schedules_for_repository(
    client: &Client,
    namespace: &str,
    repository_name: &str,
) -> Result<Vec<KanidmBackupSchedule>> {
    let api: Api<KanidmBackupSchedule> = Api::namespaced(client.clone(), namespace);
    let all = api.list(&ListParams::default()).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list schedules in {namespace}"),
            Box::new(e),
        )
    })?;

    let matching = all
        .items
        .into_iter()
        .filter(|s| s.spec.repository_ref.name == repository_name)
        .collect();

    Ok(matching)
}

async fn get_kanidm_for_schedule(
    state: &State,
    client: &Client,
    namespace: &str,
    schedule: &KanidmBackupSchedule,
) -> Result<Option<Kanidm>> {
    let kanidm_name = &schedule.spec.kanidm_ref.name;

    if let Some(k) = state.kanidm_store.find(|k| {
        kube::ResourceExt::namespace(k).as_deref() == Some(namespace)
            && k.name_any() == *kanidm_name
    }) {
        return Ok(Some((*k).clone()));
    }

    let api: Api<Kanidm> = Api::namespaced(client.clone(), namespace);
    match api.get(kanidm_name).await {
        Ok(k) => Ok(Some(k)),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(Error::KubeError(
            format!("failed to get Kanidm {namespace}/{kanidm_name}"),
            Box::new(e),
        )),
    }
}

async fn get_namespace_object(
    client: &Client,
    namespace: &str,
) -> Result<Option<k8s_openapi::api::core::v1::Namespace>> {
    let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(client.clone());
    match api.get(namespace).await {
        Ok(ns) => Ok(Some(ns)),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(Error::KubeError(
            format!("failed to get namespace {namespace}"),
            Box::new(e),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_discovery_for_schedule(
    client: &Client,
    namespace: &str,
    repository: &KanidmBackupRepository,
    schedule: &KanidmBackupSchedule,
    kanidm_name: &str,
    namespace_name: &str,
    kanidm_uid: &str,
    metrics: &DiscoveryMetrics,
) -> Result<ScanTickCounters> {
    let repo_name = repository.name_any();
    let schedule_name = schedule.name_any();

    let existing_job = find_discover_job(client, namespace, &repo_name, &schedule_name).await?;

    if let Some(job) = existing_job {
        let job_complete = job
            .status
            .as_ref()
            .is_some_and(|s| s.succeeded.is_some_and(|v| v > 0));
        let job_failed = job
            .status
            .as_ref()
            .is_some_and(|s| s.failed.is_some_and(|v| v > 0));

        if job_failed {
            let failure_message = match read_discover_result(client, namespace, &job).await {
                Ok(Some(result)) => result
                    .error
                    .as_ref()
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| "Discover Job failed".to_string()),
                _ => "Discover Job failed".to_string(),
            };

            debug!(
                namespace,
                job = job.name_any(),
                error = failure_message,
                "discover Job failed; will retry on next scan"
            );
            let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
            job_api
                .delete(&job.name_any(), &background_delete_params())
                .await
                .ok();
            apply_discovery_status(
                client,
                namespace,
                schedule,
                &[DiscoveryStatusUpdate {
                    condition_type: "DiscoveryFailed",
                    condition_status: "False",
                    reason: "DiscoverJobFailed",
                    message: &format!("Discover Job failed: {failure_message}; will retry"),
                    discovered_count: None,
                }],
            )
            .await?;
            return Ok(ScanTickCounters::default());
        }

        if job_complete {
            match read_discover_result(client, namespace, &job).await? {
                Some(result)
                    if result.success
                        && result.exit_code == ExitCode::Success
                        && result.discovery.is_some() =>
                {
                    let discovery = result.discovery.unwrap();
                    // A snapshot can only prove absence for records that existed before the
                    // listing began. Derive the snapshot time from the Job that produced it,
                    // mirroring the `job_start_time` helper in backup.rs.
                    let snapshot_time = job_start_time(&job);
                    let (new_count, reconciled_count, pruned_count) = reconcile_discovered_backups(
                        client,
                        namespace,
                        repository,
                        schedule,
                        kanidm_name,
                        namespace_name,
                        kanidm_uid,
                        &discovery,
                        snapshot_time,
                        metrics,
                    )
                    .await?;

                    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
                    job_api
                        .delete(&job.name_any(), &background_delete_params())
                        .await
                        .ok();

                    let mut msg = format!(
                        "discovered {} manifest(s); created {} new, reconciled {} existing",
                        discovery.total_found, new_count, reconciled_count
                    );
                    if pruned_count > 0 {
                        msg = format!("{msg}, pruned {} dangling", pruned_count);
                    }

                    let truncated_status = if discovery.truncated { "True" } else { "False" };
                    let truncated_reason = if discovery.truncated {
                        "ResultLimitReached"
                    } else {
                        "ResultComplete"
                    };
                    let truncated_message = if discovery.truncated {
                        warn!(
                            namespace,
                            schedule = schedule_name,
                            "discover results were truncated; some backups may not be represented"
                        );
                        metrics.inc_truncations(&repo_name);
                        format!(
                            "Discovery found {} manifests but results were truncated; some backups may not be represented",
                            discovery.total_found
                        )
                    } else {
                        "Discovery completed without truncation".to_string()
                    };

                    // One apply for the whole tick, `Discovered` first: the order of the folded
                    // list is the stored condition order, so it matches the two sequential
                    // applies this replaces. Folding them together is what keeps the second
                    // signal from re-declaring `Discovered` from the stale listed object and
                    // reverting the success timestamp and the count (issue #1082).
                    apply_discovery_status(
                        client,
                        namespace,
                        schedule,
                        &[
                            DiscoveryStatusUpdate {
                                condition_type: "Discovered",
                                condition_status: "True",
                                reason: "DiscoveryComplete",
                                message: &msg,
                                discovered_count: Some(discovery.total_found),
                            },
                            DiscoveryStatusUpdate {
                                condition_type: "DiscoveryTruncated",
                                condition_status: truncated_status,
                                reason: truncated_reason,
                                message: &truncated_message,
                                discovered_count: None,
                            },
                        ],
                    )
                    .await?;

                    debug!(
                        namespace,
                        schedule = schedule_name,
                        total_found = discovery.total_found,
                        new = new_count,
                        reconciled = reconciled_count,
                        pruned = pruned_count,
                        truncated = discovery.truncated,
                        "discover result processed"
                    );

                    return Ok(ScanTickCounters {
                        jobs_completed: 1,
                        backups_discovered: new_count,
                        ..Default::default()
                    });
                }
                Some(result) => {
                    let error_msg = result
                        .error
                        .as_ref()
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "discover returned non-success result".to_string());

                    warn!(
                        namespace,
                        schedule = schedule_name,
                        error = error_msg,
                        "discover result indicates failure"
                    );

                    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
                    job_api
                        .delete(&job.name_any(), &background_delete_params())
                        .await
                        .ok();

                    apply_discovery_status(
                        client,
                        namespace,
                        schedule,
                        &[DiscoveryStatusUpdate {
                            condition_type: "DiscoveryFailed",
                            condition_status: "False",
                            reason: "DiscoverResultInvalid",
                            message: &format!("Discover result invalid: {error_msg}"),
                            discovered_count: None,
                        }],
                    )
                    .await?;
                }
                None => {
                    debug!(
                        namespace,
                        job = job.name_any(),
                        "discover Job completed but result not yet readable"
                    );
                }
            }
            return Ok(ScanTickCounters {
                jobs_completed: 1,
                ..Default::default()
            });
        }

        debug!(
            namespace,
            job = job.name_any(),
            "discover Job still running"
        );
        return Ok(ScanTickCounters::default());
    }

    let last_discovery = schedule
        .status
        .as_ref()
        .and_then(|s| s.discovery.as_ref())
        .and_then(|d| last_discovery_signal(&d.conditions));

    let stale_threshold = backup_discovery_stale_threshold();
    let (is_stale, elapsed_secs, last_scan_time_str) =
        evaluate_discovery_staleness(last_discovery, Timestamp::now(), stale_threshold);

    if !is_stale {
        info!(
            namespace,
            schedule = %schedule_name,
            elapsed_secs = %format!("{elapsed_secs:.0}"),
            stale_secs = %stale_threshold.as_secs(),
            last_scan_time = %last_scan_time_str,
            "discovery is fresh; skipping Job creation"
        );
        if let Some(cond) = last_discovery {
            // Re-declaring the signal exactly as it already is keeps this manager's ownership
            // complete for the tick without moving `lastTransitionTime`, which is what the gate
            // reads, so a fresh tick cannot push the stale window further out.
            apply_discovery_status(
                client,
                namespace,
                schedule,
                &[DiscoveryStatusUpdate {
                    condition_type: &cond.type_,
                    condition_status: &cond.status,
                    reason: &cond.reason,
                    message: &cond.message,
                    discovered_count: None,
                }],
            )
            .await?;
        }
        return Ok(ScanTickCounters::default());
    }

    let discover_job = build_discover_job(
        repository,
        namespace,
        &schedule_name,
        namespace_name,
        kanidm_uid,
    );

    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
    match job_api.create(&Default::default(), &discover_job).await {
        Ok(_) => {
            info!(
                namespace,
                schedule = schedule_name,
                repository = repo_name,
                "created discover Job"
            );
            metrics.inc_discover_jobs(&repo_name);
        }
        Err(kube::Error::Api(ae)) if ae.code == 409 => {
            debug!(
                namespace,
                schedule = schedule_name,
                "discover Job already exists"
            );
        }
        Err(e) => {
            return Err(Error::KubeError(
                format!("failed to create discover Job for {namespace}/{schedule_name}"),
                Box::new(e),
            ));
        }
    }

    apply_discovery_status(
        client,
        namespace,
        schedule,
        &[DiscoveryStatusUpdate {
            condition_type: "Discovered",
            condition_status: "False",
            reason: "Discovering",
            message: "Discover Job created",
            discovered_count: None,
        }],
    )
    .await?;

    Ok(ScanTickCounters {
        jobs_created: 1,
        ..Default::default()
    })
}

fn build_discover_job(
    repository: &KanidmBackupRepository,
    namespace: &str,
    schedule_name: &str,
    namespace_name: &str,
    kanidm_uid: &str,
) -> Job {
    let spec = &repository.spec;
    let endpoint = &spec.s3.endpoint;
    let region = &spec.s3.region;

    let ca_bundle_path = spec.s3.ca_bundle_ref.as_ref().map(|_| ca_bundle_path());

    let operation_json = serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "discover",
        "bucket": spec.s3.bucket,
        "prefix": spec.s3.prefix,
        "endpoint": endpoint,
        "region": region,
        "forcePathStyle": spec.s3.force_path_style,
        "insecure": spec.s3.insecure,
        "caBundlePath": ca_bundle_path,
        "namespaceUid": namespace_name,
        "kanidmUid": kanidm_uid,
        "resultPath": RESULT_PATH,
        "maxResults": DEFAULT_DISCOVER_MAX_RESULTS,
    });

    let auth_method = &spec.authentication.reader;
    let mut env_vars = build_auth_env_vars(auth_method, &repository.name_any(), AuthRole::Reader);
    env_vars.push(EnvVar {
        name: "RUST_LOG".to_string(),
        value: Some("info".to_string()),
        ..Default::default()
    });

    let mut volumes = vec![Volume {
        name: "result".to_string(),
        empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
            ..Default::default()
        }),
        ..Default::default()
    }];
    let mut volume_mounts = vec![VolumeMount {
        name: "result".to_string(),
        mount_path: "/kaniop-result".to_string(),
        ..Default::default()
    }];

    volumes.extend(build_auth_volumes(auth_method));
    volume_mounts.extend(build_auth_volume_mounts(auth_method));

    if let Some(ca_bundle_ref) = &spec.s3.ca_bundle_ref {
        volumes.push(build_ca_bundle_volume(ca_bundle_ref));
        volume_mounts.push(build_ca_bundle_volume_mount());
        env_vars.push(ca_bundle_env_var());
    }

    let job_name = discover_job_name(&repository.name_any(), schedule_name);

    Job {
        metadata: ObjectMeta {
            name: Some(job_name),
            namespace: Some(namespace.to_string()),
            labels: Some(
                [
                    (
                        "app.kubernetes.io/managed-by".to_string(),
                        "kaniop".to_string(),
                    ),
                    ("kaniop.rs/repository".to_string(), repository.name_any()),
                    ("kaniop.rs/schedule".to_string(), schedule_name.to_string()),
                    ("kaniop.rs/operation".to_string(), "discover".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            ttl_seconds_after_finished: Some(BACKUP_JOB_TTL_SECONDS),
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    automount_service_account_token: Some(false),
                    restart_policy: Some("Never".to_string()),
                    security_context: Some(hardened_pod_security_context()),
                    containers: vec![Container {
                        name: "discover".to_string(),
                        image: Some(data_mover_image()),
                        command: Some(vec!["/bin/sh".to_string()]),
                        args: Some(vec![
                            "-c".to_string(),
                            build_discover_data_mover_wrapper(),
                            "--".to_string(),
                            serde_json::to_string(&operation_json).unwrap_or_default(),
                        ]),
                        env: Some(env_vars),
                        security_context: Some(hardened_security_context()),
                        termination_message_policy: Some("FallbackToLogsOnError".to_string()),
                        volume_mounts: Some(volume_mounts),
                        resources: Some(default_resource_requirements()),
                        ..Default::default()
                    }],
                    volumes: Some(volumes),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn discover_job_name(repository_name: &str, schedule_name: &str) -> String {
    let repo_part = &repository_name[..repository_name.len().min(16)];
    let sched_part = &schedule_name[..schedule_name.len().min(16)];
    format!("{DISCOVER_JOB_PREFIX}-{repo_part}-{sched_part}")
}

async fn find_discover_job(
    client: &Client,
    namespace: &str,
    repository_name: &str,
    schedule_name: &str,
) -> Result<Option<Job>> {
    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!(
        "kaniop.rs/repository={repository_name},kaniop.rs/schedule={schedule_name},kaniop.rs/operation=discover"
    ));
    let jobs = job_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list discover jobs for {namespace}/{schedule_name}"),
            Box::new(e),
        )
    })?;
    Ok(jobs.items.into_iter().next())
}

pub(crate) async fn read_discover_result(
    client: &Client,
    namespace: &str,
    job: &Job,
) -> Result<Option<ResultDocument>> {
    let job_name = job.name_any();
    let pod_api: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!("job-name={job_name}"));
    let pods = pod_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list pods for discover job {namespace}/{job_name}"),
            Box::new(e),
        )
    })?;

    let pod = match select_succeeded_pod(&pods.items) {
        Some(p) => p,
        None => return Ok(None),
    };

    let pod_name = pod.name_any();
    let log_params = LogParams {
        limit_bytes: Some(DISCOVER_LOG_LIMIT_BYTES),
        ..Default::default()
    };
    let logs = pod_api.logs(&pod_name, &log_params).await.map_err(|e| {
        Error::KubeError(
            format!("failed to read logs for discover pod {namespace}/{pod_name}"),
            Box::new(e),
        )
    })?;

    let raw = match extract_framed_result(&logs) {
        Some(payload) => payload,
        None => return Ok(None),
    };

    let doc = kaniop_backup_core::result::parse_result_document(&raw).map_err(|e| {
        Error::MissingData(format!(
            "discover result document for {namespace}/{job_name} is invalid: {e}"
        ))
    })?;
    Ok(Some(doc))
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_discovered_backups(
    client: &Client,
    namespace: &str,
    repository: &KanidmBackupRepository,
    _schedule: &KanidmBackupSchedule,
    kanidm_name: &str,
    namespace_name: &str,
    kanidm_uid: &str,
    discovery: &DiscoverResult,
    snapshot_time: Option<&Time>,
    metrics: &DiscoveryMetrics,
) -> Result<(u64, u64, u64)> {
    let repo_name = repository.name_any();
    let backup_api: Api<KanidmBackup> = Api::namespaced(client.clone(), namespace);

    let existing_backups = backup_api
        .list(&ListParams::default().labels(&format!("kaniop.rs/repository={repo_name}")))
        .await
        .map_err(|e| {
            Error::KubeError(
                format!("failed to list backups for repository {namespace}/{repo_name}"),
                Box::new(e),
            )
        })?;

    let existing_manifest_keys: HashSet<String> = existing_backups
        .items
        .iter()
        .filter(|b| b.spec.kanidm_ref.uid == kanidm_uid)
        .map(|b| b.spec.manifest_key.clone())
        .collect();

    let repo_path = RepositoryPath::new(&repository.spec.s3.bucket, &repository.spec.s3.prefix)
        .map_err(|e| Error::MissingData(format!("invalid repository path: {e}")))?;

    let mut pruned_count = 0u64;
    if snapshot_supports_pruning(discovery) {
        // The discover Job lists exactly one tenant subtree, built by the same helper the data
        // mover uses, so the prune can only judge records that snapshot could actually have seen.
        // `namespaceUid` carries the namespace identity the producer was given (see issue #1084);
        // matching it here keeps a keyspace divergence from turning absence-of-evidence into
        // deletes under a path the scan never covered.
        let scanned_prefix = repo_path
            .manifests_prefix(namespace_name, kanidm_uid)
            .map_err(|e| Error::MissingData(format!("invalid discovery prefix: {e}")))?;
        let discovered_keys: HashSet<String> = discovery.manifest_keys.iter().cloned().collect();
        let dangling: HashSet<String> = dangling_discovered_backups(
            &existing_backups.items,
            &discovered_keys,
            kanidm_uid,
            snapshot_time,
            &repo_path,
            &scanned_prefix,
        )
        .into_iter()
        .collect();
        for backup in existing_backups
            .items
            .iter()
            .filter(|b| dangling.contains(&b.name_any()))
        {
            let name = backup.name_any();
            // `dangling_discovered_backups` only selects records that carry a uid, so the
            // precondition below is always satisfiable. Pinning the delete to the uid makes
            // sure the name still points at the record this snapshot read, and not at a
            // different incarnation that reused the deterministic `kb-<id8>` name.
            let Some(uid) = backup.metadata.uid.as_ref() else {
                continue;
            };
            let params = delete_params_with_uid_precondition(uid);
            match backup_api.delete(&name, &params).await {
                Ok(_) => {
                    info!(
                        namespace,
                        backup = name,
                        manifest_key = %backup.spec.manifest_key,
                        "pruned dangling discovered backup"
                    );
                    pruned_count += 1;
                }
                Err(kube::Error::Api(ae)) if ae.code == 404 => {}
                Err(e) if is_precondition_conflict(&e) => {
                    debug!(
                        namespace,
                        backup = name,
                        error = %e,
                        "prune skipped: discovered record changed or was replaced since the snapshot"
                    );
                }
                Err(e) => {
                    warn!(
                        namespace,
                        backup = name,
                        error = %e,
                        "failed to prune dangling discovered backup"
                    );
                }
            }
        }
    }

    let mut new_count = 0u64;
    let mut reconciled_count = 0u64;

    for manifest_key in &discovery.manifest_keys {
        if manifest_key.contains("..") {
            warn!(
                key = manifest_key,
                "skipping manifest key with path traversal"
            );
            continue;
        }

        if !manifest_key.ends_with("/manifest.json") {
            warn!(key = manifest_key, "skipping non-manifest key");
            continue;
        }

        if !repo_path.contains_key(manifest_key) {
            warn!(
                key = manifest_key,
                "skipping manifest key outside repository prefix"
            );
            continue;
        }

        let backup_id = extract_backup_id_from_manifest_key(manifest_key);
        let Some(backup_id) = backup_id else {
            warn!(
                key = manifest_key,
                "could not extract backup_id from manifest key"
            );
            continue;
        };

        if existing_manifest_keys.contains(manifest_key) {
            reconciled_count += 1;
            continue;
        }

        let backup_cr = build_backup_cr(
            manifest_key,
            &backup_id,
            &repo_name,
            kanidm_name,
            kanidm_uid,
        );

        match backup_api.create(&Default::default(), &backup_cr).await {
            Ok(_) => {
                info!(
                    namespace,
                    backup = backup_cr.name_any(),
                    manifest_key,
                    "created KanidmBackup from discovery"
                );
                new_count += 1;
            }
            Err(kube::Error::Api(ae)) if ae.code == 409 => {
                reconciled_count += 1;
                debug!(
                    namespace,
                    backup = backup_cr.name_any(),
                    "KanidmBackup already exists; reconciled"
                );
            }
            Err(e) => {
                warn!(
                    namespace,
                    backup = backup_cr.name_any(),
                    error = %e,
                    "failed to create KanidmBackup from discovery"
                );
            }
        }
    }

    metrics.inc_backups_discovered(&repo_name, new_count);
    metrics.inc_backups_reconciled(&repo_name, reconciled_count);
    if pruned_count > 0 {
        metrics.inc_dangling_pruned(&repo_name, pruned_count);
    }

    Ok((new_count, reconciled_count, pruned_count))
}

/// Whether a scan snapshot may drive the destructive prune. It has to be complete, i.e.
/// `truncated` is false. The producer sets `truncated` when either the caller's result cap or its
/// own page cap is hit (`discover_truncated` in `cmd/data-mover/src/commands/discover.rs`), so a
/// false `truncated` means the listing really did see every manifest under the scanned prefix.
/// The `manifest_keys.len() == total_found` clause is only a cheap sanity check on that producer
/// invariant: `discover.rs` derives both fields from the same list, so they cannot disagree.
fn snapshot_supports_pruning(discovery: &DiscoverResult) -> bool {
    !discovery.truncated && discovery.manifest_keys.len() == discovery.total_found as usize
}

/// Discovered backup records that no longer match anything in a complete repository
/// snapshot. Only records discovery itself created are eligible; user-authored records and
/// records in any other phase are left alone. A snapshot can only prove absence for records
/// that existed before the listing began, so `snapshot_time` is the Job timestamp that
/// bounds the evidence: records created at or after that time are kept, and a missing
/// snapshot time disables pruning entirely. Records without a uid are also kept because
/// the delete path pins the precondition to the uid. The snapshot covers exactly
/// `scanned_prefix` -- the tenant subtree the discover Job listed -- so only records whose key
/// lies under it are judged; `repo_path` additionally confines the comparison to the scanned
/// repository and rejects traversal keys.
fn dangling_discovered_backups(
    backups: &[KanidmBackup],
    discovered_keys: &HashSet<String>,
    kanidm_uid: &str,
    snapshot_time: Option<&Time>,
    repo_path: &RepositoryPath,
    scanned_prefix: &str,
) -> Vec<String> {
    // Without a Job timestamp there is no temporally valid evidence, so nothing is prunable.
    let Some(snapshot_time) = snapshot_time else {
        return Vec::new();
    };
    backups
        .iter()
        .filter(|backup| {
            // A record outside the scanned repository prefix is not covered by this snapshot at
            // all; `contains_key` is also what keeps traversal-shaped keys out.
            repo_path.contains_key(&backup.spec.manifest_key)
                // A record under a different tenant subtree was never listed by this scan either,
                // even though it lives in the same repository.
                && backup.spec.manifest_key.starts_with(scanned_prefix)
                // A record whose creation time is unknown, or that was created at/after the
                // listing started, cannot be proven absent by this snapshot.
                && backup
                    .metadata
                    .creation_timestamp
                    .as_ref()
                    .is_some_and(|created| created.0 < snapshot_time.0)
                // The delete path pins the precondition to the uid; a record without one
                // cannot be addressed safely.
                && backup.metadata.uid.is_some()
                && backup.spec.kanidm_ref.uid == kanidm_uid
                && is_discovery_owned(backup)
                && backup.metadata.deletion_timestamp.is_none()
                && backup
                    .status
                    .as_ref()
                    .is_some_and(|s| s.phase == KanidmBackupPhase::Invalid)
                && !discovered_keys.contains(&backup.spec.manifest_key)
        })
        .map(|backup| backup.name_any())
        .collect()
}

fn extract_backup_id_from_manifest_key(manifest_key: &str) -> Option<String> {
    let parts: Vec<&str> = manifest_key.split('/').collect();
    let backups_idx = parts.iter().position(|&p| p == "backups")?;
    let backup_dir = parts.get(backups_idx + 1)?;
    if backup_dir.is_empty() {
        return None;
    }
    uuid::Uuid::parse_str(backup_dir)
        .ok()
        .map(|u| u.to_string())
}

fn build_backup_cr(
    manifest_key: &str,
    backup_id: &str,
    repository_name: &str,
    kanidm_name: &str,
    kanidm_uid: &str,
) -> KanidmBackup {
    let backup_name = format!("kb-{}", &backup_id[..backup_id.len().min(8)]);
    KanidmBackup {
        metadata: ObjectMeta {
            name: Some(backup_name),
            labels: Some(
                [
                    ("kaniop.rs/backup-id".to_string(), backup_id.to_string()),
                    (
                        "kaniop.rs/repository".to_string(),
                        repository_name.to_string(),
                    ),
                    (DISCOVERED_LABEL.to_string(), "true".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        spec: KanidmBackupSpec {
            backup_id: backup_id.to_string(),
            kanidm_ref: BackupKanidmRef {
                name: kanidm_name.to_string(),
                uid: kanidm_uid.to_string(),
            },
            repository_ref: BackupRepositoryRef {
                name: repository_name.to_string(),
            },
            manifest_key: manifest_key.to_string(),
        },
        status: None,
    }
}

fn merge_conditions(existing: &[Condition], new_conds: &[Condition]) -> Vec<Condition> {
    let new_types: HashSet<&str> = new_conds.iter().map(|c| c.type_.as_str()).collect();
    let mut merged: Vec<Condition> = existing
        .iter()
        .filter(|c| !new_types.contains(c.type_.as_str()))
        .cloned()
        .collect();
    merged.extend(new_conds.iter().cloned());
    merged
}

fn transition_time(
    existing: &[Condition],
    type_: &str,
    new_status: &str,
    new_reason: &str,
    now: Timestamp,
) -> Time {
    existing
        .iter()
        .find(|c| c.type_ == type_ && c.status == new_status && c.reason == new_reason)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Time(now))
}

/// The discovery signal the staleness gate has to honour: whichever of `Discovered` and
/// `DiscoveryFailed` transitioned last.
///
/// Stored order cannot be used here. `merge_conditions` re-appends a replaced condition at the end,
/// so the first `Discovered` apply after a failure parks `Discovered` *behind* the older
/// `DiscoveryFailed`, and a first-match lookup would keep reading the failure timestamp. That
/// timestamp only moves when the failure's status or reason changes, so repeated identical failures
/// preserve it and would leave the schedule stale forever, creating a Job every tick (issue #1082).
fn last_discovery_signal(conditions: &[Condition]) -> Option<&Condition> {
    conditions
        .iter()
        .filter(|c| c.type_ == "Discovered" || c.type_ == "DiscoveryFailed")
        // Ties are irrelevant to the gate: both candidates would report the same elapsed time.
        // `max_by_key` keeps the last of equal maxima, i.e. the later one in stored order.
        .max_by_key(|c| c.last_transition_time.0)
}

/// One signal a discovery tick wants to record in `status.discovery`.
///
/// A tick that has several signals to report -- a completed scan and whether its results were
/// truncated -- describes them as a list and folds it once. Two forced server-side-applies by the
/// same field manager cannot see each other: each is built from the object listed at the start of
/// the tick, so the second re-declares every field from that stale base and wins (issue #1082).
#[derive(Debug, Clone, Copy)]
struct DiscoveryStatusUpdate<'a> {
    condition_type: &'a str,
    condition_status: &'a str,
    reason: &'a str,
    message: &'a str,
    discovered_count: Option<u32>,
}

/// Folds `updates` into `base` in order, so update N sees what update N-1 wrote instead of the
/// stale listed object.
///
/// The per-field semantics are the ones discovery has always had: every tick refreshes
/// `last_scan_time`; only `Discovered=True` advances `last_successful_scan_time`; only
/// `DiscoveryFailed=True` records `last_error`; and `last_discovered_count` is only written when the
/// caller has a count. `now` is injected so the fold is pure and one tick has a single timestamp for
/// both its fields and the conditions it transitions.
fn build_discovery_status(
    base: &DiscoveryStatus,
    updates: &[DiscoveryStatusUpdate<'_>],
    generation: Option<i64>,
    now: Timestamp,
) -> DiscoveryStatus {
    let mut discovery = base.clone();
    discovery.last_scan_time = Some(now.to_string());

    for update in updates {
        let condition = Condition {
            type_: update.condition_type.to_string(),
            status: update.condition_status.to_string(),
            observed_generation: generation,
            last_transition_time: transition_time(
                &discovery.conditions,
                update.condition_type,
                update.condition_status,
                update.reason,
                now,
            ),
            reason: update.reason.to_string(),
            message: update.message.to_string(),
        };

        // Always the complete list this manager owns, updated conditions included.
        // `status.discovery.conditions` is an `x-kubernetes-list-type: map` keyed on `type`, so an
        // apply that omits a condition releases it and the API server deletes it.
        discovery.conditions = merge_conditions(&discovery.conditions, &[condition]);

        if update.condition_type == "Discovered" && update.condition_status == "True" {
            discovery.last_successful_scan_time = Some(now.to_string());
        }
        if update.condition_type == "DiscoveryFailed" && update.condition_status == "True" {
            discovery.last_error = Some(update.message.to_string());
        }
        if let Some(count) = update.discovered_count {
            discovery.last_discovered_count = Some(count);
        }
    }

    discovery
}

/// Evaluates the staleness gate for one tick: whether a discover Job is needed, how many seconds
/// have passed since the last discovery signal transitioned, and that timestamp for logging.
///
/// No signal at all is stale, which is what bootstraps the first Job. The comparison is strict, so
/// an elapsed time exactly equal to the threshold is still fresh.
fn evaluate_discovery_staleness(
    last_signal: Option<&Condition>,
    now: Timestamp,
    threshold: Duration,
) -> (bool, f64, String) {
    let Some(condition) = last_signal else {
        return (true, 0.0, String::new());
    };
    let last_transition = condition.last_transition_time.0;
    let elapsed_secs = now
        .since(last_transition)
        .map(|span| span.get_seconds() as f64)
        .unwrap_or(0.0);
    (
        elapsed_secs > threshold.as_secs_f64(),
        elapsed_secs,
        last_transition.to_string(),
    )
}

/// The single discovery status patch of a tick: declares the whole `status.discovery` object under
/// one field manager, and nothing else. The schedule's own `status.conditions` belong to the
/// schedule controller and must stay out of it.
fn discovery_status_patch(discovery: &DiscoveryStatus) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "kaniop.rs/v1alpha1",
        "kind": "KanidmBackupSchedule",
        "status": {
            "discovery": discovery,
        }
    })
}

/// Records everything a tick learned about discovery in exactly one forced apply, so no signal can
/// clobber another. `schedule` is the object listed at the start of the tick and is only ever read
/// for its name, generation and current `status.discovery`; nothing this function writes depends on
/// an earlier apply of the same tick.
async fn apply_discovery_status(
    client: &Client,
    namespace: &str,
    schedule: &KanidmBackupSchedule,
    updates: &[DiscoveryStatusUpdate<'_>],
) -> Result<()> {
    let api: Api<KanidmBackupSchedule> = Api::namespaced(client.clone(), namespace);
    let name = schedule.name_any();

    let base = schedule
        .status
        .as_ref()
        .and_then(|s| s.discovery.as_ref())
        .cloned()
        .unwrap_or_default();
    let discovery_status = build_discovery_status(
        &base,
        updates,
        schedule.metadata.generation,
        Timestamp::now(),
    );

    api.patch_status(
        &name,
        &PatchParams::apply(DISCOVERY_CONTROLLER_ID).force(),
        &Patch::Apply(discovery_status_patch(&discovery_status)),
    )
    .await
    .map_err(|e| {
        Error::KubeError(
            format!("failed to patch discovery status for {namespace}/{name}"),
            Box::new(e),
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{
        AuthMethod, KanidmBackupPhase, KanidmBackupRepositorySpec, KanidmBackupScheduleSpec,
        KanidmBackupStatus, RepositoryAuthentication, S3Config, ScheduleKanidmRef,
        ScheduleRepositoryRef, SecretRef,
    };
    use http::Response;
    use kube::client::Body;

    fn make_repository(name: &str) -> KanidmBackupRepository {
        KanidmBackupRepository {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "test-bucket".to_string(),
                    prefix: "prod".to_string(),
                    region: "us-east-1".to_string(),
                    endpoint: "https://s3.example.com".to_string(),
                    force_path_style: false,
                    insecure: false,
                    ca_bundle_ref: None,
                },
                authentication: RepositoryAuthentication {
                    writer: AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(SecretRef {
                            name: "writer".to_string(),
                        }),
                    },
                    reader: AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(SecretRef {
                            name: "reader".to_string(),
                        }),
                    },
                    deleter: AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(SecretRef {
                            name: "deleter".to_string(),
                        }),
                    },
                },
                encryption: None,
                limits: None,
            },
            status: None,
        }
    }

    fn make_schedule(name: &str, repo_name: &str, kanidm_name: &str) -> KanidmBackupSchedule {
        KanidmBackupSchedule {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: KanidmBackupScheduleSpec {
                kanidm_ref: ScheduleKanidmRef {
                    name: kanidm_name.to_string(),
                },
                repository_ref: ScheduleRepositoryRef {
                    name: repo_name.to_string(),
                },
                schedule: "0 */6 * * *".to_string(),
                time_zone: "UTC".to_string(),
                suspend: false,
                concurrency_policy: "Forbid".to_string(),
                jitter_seconds: None,
                local_versions: 7,
                retention: None,
            },
            status: None,
        }
    }

    #[test]
    fn discover_job_name_is_deterministic() {
        let name1 = discover_job_name("my-repo", "my-schedule");
        let name2 = discover_job_name("my-repo", "my-schedule");
        assert_eq!(name1, name2);
    }

    #[test]
    fn discover_job_name_differs_for_different_inputs() {
        let name1 = discover_job_name("repo-a", "sched-1");
        let name2 = discover_job_name("repo-b", "sched-1");
        let name3 = discover_job_name("repo-a", "sched-2");
        assert_ne!(name1, name2);
        assert_ne!(name1, name3);
    }

    #[test]
    fn discover_job_name_truncates_long_names() {
        let long_repo = "x".repeat(50);
        let long_sched = "y".repeat(50);
        let name = discover_job_name(&long_repo, &long_sched);
        assert!(name.len() <= DISCOVER_JOB_PREFIX.len() + 1 + 16 + 1 + 16);
    }

    #[test]
    fn build_discover_job_has_hardened_security() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();

        assert_eq!(pod_spec.automount_service_account_token, Some(false));

        let pod_sec = pod_spec.security_context.unwrap();
        assert_eq!(pod_sec.run_as_non_root, Some(true));
        assert_eq!(
            pod_sec.seccomp_profile.as_ref().map(|s| s.type_.as_str()),
            Some("RuntimeDefault")
        );

        let container = &pod_spec.containers[0];
        let sec = container.security_context.as_ref().unwrap();
        assert_eq!(sec.run_as_non_root, Some(true));
        assert_eq!(sec.read_only_root_filesystem, Some(true));
        assert_eq!(sec.allow_privilege_escalation, Some(false));
        let caps = sec.capabilities.as_ref().unwrap();
        assert_eq!(caps.drop, Some(vec!["ALL".to_string()]));
    }

    #[test]
    fn build_discover_job_sets_ttl_seconds_after_finished() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let job_spec = job.spec.unwrap();
        assert_eq!(
            job_spec.ttl_seconds_after_finished,
            Some(BACKUP_JOB_TTL_SECONDS)
        );
    }

    #[test]
    fn build_discover_job_operation_is_discover() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "daily", "production", "k-uid-456");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let args = pod_spec.containers[0].args.as_ref().unwrap();
        assert_eq!(args[0], "-c");

        let op_json = &args[3];
        let op: serde_json::Value = serde_json::from_str(op_json).unwrap();
        assert_eq!(op["operation"], "discover");
        assert_eq!(op["namespaceUid"], "production");
        assert_eq!(op["kanidmUid"], "k-uid-456");
        assert_eq!(op["bucket"], "test-bucket");
        assert_eq!(op["prefix"], "prod");
        assert_eq!(op["maxResults"], DEFAULT_DISCOVER_MAX_RESULTS);
    }

    #[test]
    fn build_discover_job_uses_framed_wrapper() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let args = pod_spec.containers[0].args.as_ref().unwrap();
        let wrapper_script = &args[1];
        assert!(wrapper_script.contains(crate::controller::RESULT_FRAME_BEGIN));
        assert!(wrapper_script.contains(crate::controller::RESULT_FRAME_END));
        assert!(wrapper_script.contains("/bin/kaniop-data-mover discover"));
        assert!(wrapper_script.contains("exit $STATUS"));
    }

    #[test]
    fn framed_discovery_result_exceeding_termination_limit_extracts_and_parses() {
        let keys: Vec<String> = (0..80)
            .map(|i| {
                format!(
                    "prod/v1/tenants/default-ns/clusters/kaniop/backups/{:032x}-f423-7a12-8f41-2bea7588a303/manifest.json",
                    i
                )
            })
            .collect();
        let mut doc = kaniop_backup_core::result::ResultDocument::success("discover");
        doc.discovery = Some(kaniop_backup_core::result::DiscoverResult {
            manifest_keys: keys,
            total_found: 80,
            truncated: false,
        });
        let json = serde_json::to_string_pretty(&doc).unwrap();
        assert!(
            json.len() > crate::controller::TERMINATION_MESSAGE_LIMIT,
            "test payload must exceed the old termination-message limit of {} bytes, got {}",
            crate::controller::TERMINATION_MESSAGE_LIMIT,
            json.len()
        );
        let log = format!(
            "{}\n{}\n{}",
            crate::controller::RESULT_FRAME_BEGIN,
            json,
            crate::controller::RESULT_FRAME_END
        );
        let extracted = crate::controller::extract_framed_result(&log);
        assert!(extracted.is_some());
        let parsed = kaniop_backup_core::result::parse_result_document(&extracted.unwrap());
        assert!(parsed.is_ok());
        let parsed_doc = parsed.unwrap();
        assert_eq!(parsed_doc.discovery.as_ref().unwrap().total_found, 80);
    }

    #[test]
    fn build_discover_job_has_correct_labels() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "my-schedule", "ns-uid", "k-uid");
        let labels = job.metadata.labels.unwrap();
        assert_eq!(labels["app.kubernetes.io/managed-by"], "kaniop");
        assert_eq!(labels["kaniop.rs/repository"], "offsite");
        assert_eq!(labels["kaniop.rs/schedule"], "my-schedule");
        assert_eq!(labels["kaniop.rs/operation"], "discover");
    }

    #[test]
    fn build_backup_cr_has_no_owner_references() {
        let cr = build_backup_cr(
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "corp-idm",
            "k-uid",
        );
        assert!(
            cr.metadata.owner_references.is_none(),
            "backup CR must not have ownerReferences"
        );
    }

    #[test]
    fn build_backup_cr_name_is_deterministic() {
        let cr1 = build_backup_cr(
            "key1/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        let cr2 = build_backup_cr(
            "key2/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        assert_eq!(cr1.metadata.name, cr2.metadata.name);
    }

    #[test]
    fn build_backup_cr_different_ids_produce_different_names() {
        let cr1 = build_backup_cr(
            "m1/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        let cr2 = build_backup_cr(
            "m2/manifest.json",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "repo",
            "kanidm",
            "uid",
        );
        assert_ne!(cr1.metadata.name, cr2.metadata.name);
    }

    #[test]
    fn build_backup_cr_starts_without_status() {
        let cr = build_backup_cr(
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "corp-idm",
            "k-uid",
        );
        assert!(
            cr.status.is_none(),
            "backup CR must not have initial status"
        );
    }

    #[test]
    fn build_backup_cr_has_discovered_label() {
        let cr = build_backup_cr(
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "corp-idm",
            "k-uid",
        );
        let labels = cr.metadata.labels.unwrap();
        assert_eq!(labels["kaniop.rs/discovered"], "true");
        assert_eq!(labels["kaniop.rs/repository"], "offsite");
    }

    #[test]
    fn extract_backup_id_from_valid_key() {
        let key = "prod/v1/tenants/ns/clusters/k/backups/019c7c76-f423-7a12-8f41-2bea7588a303/manifest.json";
        let id = extract_backup_id_from_manifest_key(key);
        assert_eq!(id, Some("019c7c76-f423-7a12-8f41-2bea7588a303".to_string()));
    }

    #[test]
    fn extract_backup_id_from_key_without_backups_segment() {
        let key = "prod/v1/tenants/ns/clusters/k/manifest.json";
        let id = extract_backup_id_from_manifest_key(key);
        assert!(id.is_none());
    }

    #[test]
    fn extract_backup_id_from_key_with_invalid_uuid() {
        let key = "prod/v1/tenants/ns/clusters/k/backups/not-a-uuid/manifest.json";
        let id = extract_backup_id_from_manifest_key(key);
        assert!(id.is_none());
    }

    #[test]
    fn extract_backup_id_from_empty_key() {
        let id = extract_backup_id_from_manifest_key("");
        assert!(id.is_none());
    }

    #[test]
    fn discover_job_has_resource_requirements() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let container = &pod_spec.containers[0];
        assert!(container.resources.is_some());
    }

    #[test]
    fn stale_threshold_is_positive() {
        assert!(STALE_THRESHOLD.as_secs() > 0);
    }

    #[test]
    fn default_scan_interval_is_positive() {
        assert!(DEFAULT_SCAN_INTERVAL.as_secs() > 0);
    }

    #[test]
    fn reconcile_is_idempotent_for_same_manifest_key() {
        let cr1 = build_backup_cr(
            "same/key/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        let cr2 = build_backup_cr(
            "same/key/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        assert_eq!(cr1.metadata.name, cr2.metadata.name);
        assert_eq!(cr1.spec, cr2.spec);
    }

    #[test]
    fn build_backup_cr_manifest_key_is_preserved() {
        let manifest_key = "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json";
        let cr = build_backup_cr(
            manifest_key,
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "corp-idm",
            "k-uid",
        );
        assert_eq!(cr.spec.manifest_key, manifest_key);
        assert!(cr.spec.manifest_key.ends_with("/manifest.json"));
    }

    #[test]
    fn build_backup_cr_does_not_set_ready_phase() {
        let cr = build_backup_cr(
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "corp-idm",
            "k-uid",
        );
        assert!(cr.status.is_none());
        let default_status = KanidmBackupPhase::default();
        assert_eq!(default_status, KanidmBackupPhase::Discovering);
        assert_ne!(default_status, KanidmBackupPhase::Ready);
    }

    #[test]
    fn table_test_manifest_key_validation() {
        let cases = [
            (
                "prod/v1/tenants/ns/clusters/k/backups/b/manifest.json",
                true,
            ),
            (
                "prod/v1/tenants/ns/clusters/k/backups/b/payload/data",
                false,
            ),
            ("prod/../manifest.json", false),
            ("other/repo/prefix/manifest.json", true),
        ];
        for (key, should_be_valid_manifest) in cases {
            let is_valid = key.ends_with("/manifest.json") && !key.contains("..");
            assert_eq!(
                is_valid, should_be_valid_manifest,
                "key '{key}' should be valid={should_be_valid_manifest}"
            );
        }
    }

    #[test]
    fn discover_job_injects_reader_auth_env_vars() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let container = &pod_spec.containers[0];
        let env_vars = container.env.as_ref().unwrap();

        let aws_key = env_vars
            .iter()
            .find(|e| e.name == "AWS_ACCESS_KEY_ID")
            .expect("discover job should inject AWS_ACCESS_KEY_ID from reader auth");
        let key_ref = aws_key
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.name, "reader");
        assert_eq!(key_ref.key, "AWS_ACCESS_KEY_ID");
    }

    #[test]
    fn discover_job_operation_includes_ca_bundle_path() {
        let mut repo = make_repository("offsite");
        repo.spec.s3.ca_bundle_ref = Some("my-ca-cm".to_string());
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let args = pod_spec.containers[0].args.as_ref().unwrap();
        let op: serde_json::Value = serde_json::from_str(&args[3]).unwrap();
        assert_eq!(op["caBundlePath"], "/etc/ssl/certs/ca-certificates.crt");
    }

    #[test]
    fn discover_job_operation_ca_bundle_path_null_when_not_configured() {
        let repo = make_repository("offsite");
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let args = pod_spec.containers[0].args.as_ref().unwrap();
        let op: serde_json::Value = serde_json::from_str(&args[3]).unwrap();
        assert!(op["caBundlePath"].is_null());
    }

    #[test]
    fn discover_job_workload_identity_omits_static_credentials() {
        let mut repo = make_repository("offsite");
        repo.spec.authentication.reader = AuthMethod {
            workload_identity: Some(kaniop_backup_core::crd::WorkloadIdentity {
                audience: Some("sts.amazonaws.com".to_string()),
            }),
            secret_ref: None,
        };
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let container = &pod_spec.containers[0];
        let env_vars = container.env.as_ref().unwrap();

        let has_aws_key = env_vars.iter().any(|e| e.name == "AWS_ACCESS_KEY_ID");
        assert!(
            !has_aws_key,
            "workload identity should not inject static AWS credentials"
        );

        let has_token_file = env_vars
            .iter()
            .any(|e| e.name == "AWS_WEB_IDENTITY_TOKEN_FILE");
        assert!(
            has_token_file,
            "workload identity with audience should inject token file env"
        );

        let has_projected_volume = pod_spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .any(|v| v.name == "projected-token");
        assert!(
            has_projected_volume,
            "workload identity with audience should mount projected token volume"
        );
    }

    #[test]
    fn discover_job_custom_ca_bundle_wiring() {
        let mut repo = make_repository("offsite");
        repo.spec.s3.ca_bundle_ref = Some("custom-ca".to_string());
        let job = build_discover_job(&repo, "default", "daily", "ns-uid", "k-uid");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();

        let has_ca_volume = pod_spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .any(|v| v.name == "ca-bundle");
        assert!(has_ca_volume, "discover job should mount CA bundle volume");

        let has_ca_mount = pod_spec.containers[0]
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .any(|m| m.name == "ca-bundle");
        assert!(
            has_ca_mount,
            "discover job should mount CA bundle in container"
        );

        let has_ssl_env = pod_spec.containers[0]
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.name == "SSL_CERT_FILE");
        assert!(has_ssl_env, "discover job should set SSL_CERT_FILE env var");
    }

    #[test]
    fn schedule_filtering_by_repository() {
        let s1 = make_schedule("s1", "repo-a", "kanidm-1");
        let s2 = make_schedule("s2", "repo-b", "kanidm-2");
        let s3 = make_schedule("s3", "repo-a", "kanidm-3");
        let schedules = vec![s1, s2, s3];
        let matching: Vec<_> = schedules
            .into_iter()
            .filter(|s| s.spec.repository_ref.name == "repo-a")
            .collect();
        assert_eq!(matching.len(), 2);
    }

    #[test]
    fn suspended_schedule_is_identified() {
        let mut s = make_schedule("s1", "repo-a", "kanidm-1");
        assert!(!s.spec.suspend);
        s.spec.suspend = true;
        assert!(s.spec.suspend);
    }

    fn make_condition(type_: &str, status: &str) -> Condition {
        Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::now()),
            reason: "TestReason".to_string(),
            message: "test message".to_string(),
        }
    }

    #[test]
    fn merge_conditions_adds_new_type_to_existing() {
        let existing = vec![
            make_condition("Ready", "True"),
            make_condition("TransportExperimental", "True"),
        ];
        let new_conds = vec![make_condition("Discovered", "True")];
        let merged = merge_conditions(&existing, &new_conds);
        assert_eq!(merged.len(), 3);
        assert!(merged.iter().any(|c| c.type_ == "Ready"));
        assert!(merged.iter().any(|c| c.type_ == "TransportExperimental"));
        assert!(merged.iter().any(|c| c.type_ == "Discovered"));
    }

    #[test]
    fn merge_conditions_replaces_same_type() {
        let existing = vec![
            make_condition("Ready", "True"),
            make_condition("Discovered", "False"),
        ];
        let new_conds = vec![make_condition("Discovered", "True")];
        let merged = merge_conditions(&existing, &new_conds);
        assert_eq!(merged.len(), 2);
        let discovered = merged.iter().find(|c| c.type_ == "Discovered").unwrap();
        assert_eq!(discovered.status, "True");
        assert!(merged.iter().any(|c| c.type_ == "Ready"));
    }

    #[test]
    fn merge_conditions_preserves_existing_when_no_overlap() {
        let existing = vec![
            make_condition("Ready", "True"),
            make_condition("Suspended", "False"),
            make_condition("TransportExperimental", "True"),
        ];
        let new_conds = vec![make_condition("DiscoveryFailed", "False")];
        let merged = merge_conditions(&existing, &new_conds);
        assert_eq!(merged.len(), 4);
        assert!(merged.iter().any(|c| c.type_ == "Ready"));
        assert!(merged.iter().any(|c| c.type_ == "Suspended"));
        assert!(merged.iter().any(|c| c.type_ == "TransportExperimental"));
        assert!(merged.iter().any(|c| c.type_ == "DiscoveryFailed"));
    }

    #[test]
    fn merge_conditions_empty_existing_returns_new() {
        let existing: Vec<Condition> = vec![];
        let new_conds = vec![make_condition("Discovered", "True")];
        let merged = merge_conditions(&existing, &new_conds);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].type_, "Discovered");
    }

    #[test]
    fn merge_conditions_empty_new_returns_existing() {
        let existing = vec![make_condition("Ready", "True")];
        let new_conds: Vec<Condition> = vec![];
        let merged = merge_conditions(&existing, &new_conds);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].type_, "Ready");
    }

    #[test]
    fn merge_conditions_multiple_new_replace_multiple_existing() {
        let existing = vec![
            make_condition("Ready", "True"),
            make_condition("Discovered", "False"),
            make_condition("DiscoveryFailed", "True"),
        ];
        let new_conds = vec![
            make_condition("Discovered", "True"),
            make_condition("DiscoveryFailed", "False"),
        ];
        let merged = merge_conditions(&existing, &new_conds);
        assert_eq!(merged.len(), 3);
        assert!(merged.iter().any(|c| c.type_ == "Ready"));
        let discovered = merged.iter().find(|c| c.type_ == "Discovered").unwrap();
        assert_eq!(discovered.status, "True");
        let failed = merged
            .iter()
            .find(|c| c.type_ == "DiscoveryFailed")
            .unwrap();
        assert_eq!(failed.status, "False");
    }

    #[test]
    fn discovery_status_subobject_is_independent_from_schedule_conditions() {
        let mut schedule = KanidmBackupSchedule {
            metadata: ObjectMeta {
                name: Some("test-schedule".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupScheduleSpec {
                kanidm_ref: crate::crd::ScheduleKanidmRef {
                    name: "test-kanidm".to_string(),
                },
                repository_ref: crate::crd::ScheduleRepositoryRef {
                    name: "test-repo".to_string(),
                },
                schedule: "0 2 * * *".to_string(),
                time_zone: "UTC".to_string(),
                suspend: false,
                concurrency_policy: "Forbid".to_string(),
                jitter_seconds: None,
                local_versions: 7,
                retention: None,
            },
            status: None,
        };

        let discovery_status = DiscoveryStatus {
            last_scan_time: Some("2024-01-01T00:00:00Z".to_string()),
            last_successful_scan_time: Some("2024-01-01T00:00:00Z".to_string()),
            last_discovered_count: Some(5),
            last_error: None,
            conditions: vec![make_condition("Discovered", "True")],
        };

        schedule.status = Some(crate::crd::KanidmBackupScheduleStatus {
            observed_generation: Some(1),
            last_discovered_backup_ref: None,
            last_successful_backup_time: None,
            conditions: vec![make_condition("Ready", "True")],
            discovery: Some(discovery_status),
        });

        let status = schedule.status.as_ref().unwrap();
        assert_eq!(status.conditions.len(), 1);
        assert_eq!(status.conditions[0].type_, "Ready");

        let discovery = status.discovery.as_ref().unwrap();
        assert_eq!(discovery.conditions.len(), 1);
        assert_eq!(discovery.conditions[0].type_, "Discovered");
        assert_eq!(discovery.last_discovered_count, Some(5));
    }

    #[test]
    fn stale_threshold_constant_matches_getter_fallback_default() {
        assert_eq!(
            STALE_THRESHOLD.as_secs(),
            kaniop_operator::controller::backup_discovery_stale_threshold().as_secs(),
            "STALE_THRESHOLD constant must match the OnceLock getter fallback default"
        );
    }

    #[test]
    fn transition_time_preserves_timestamp_when_type_status_reason_match() {
        let fixed_time = Time(Timestamp::from_second(1_700_000_000).unwrap());
        let existing = vec![Condition {
            type_: "Discovered".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: fixed_time.clone(),
            reason: "DiscoveryComplete".to_string(),
            message: "original message".to_string(),
        }];

        let reused = transition_time(
            &existing,
            "Discovered",
            "True",
            "DiscoveryComplete",
            Timestamp::now(),
        );
        assert_eq!(
            reused.0, fixed_time.0,
            "transition_time must preserve lastTransitionTime when type/status/reason match"
        );
    }

    #[test]
    fn transition_time_updates_when_reason_differs() {
        let fixed_time = Time(Timestamp::from_second(1_700_000_000).unwrap());
        let existing = vec![Condition {
            type_: "Discovered".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: fixed_time.clone(),
            reason: "DiscoveryComplete".to_string(),
            message: "original".to_string(),
        }];

        let new_time = transition_time(
            &existing,
            "Discovered",
            "True",
            "Discovering",
            Timestamp::now(),
        );
        assert!(
            new_time.0 >= fixed_time.0,
            "transition_time should produce a new timestamp when reason differs"
        );
    }

    #[test]
    fn scan_tick_counters_default_to_zero() {
        let c = ScanTickCounters::default();
        assert_eq!(c.repos_scanned, 0);
        assert_eq!(c.schedules_processed, 0);
        assert_eq!(c.jobs_created, 0);
        assert_eq!(c.jobs_completed, 0);
        assert_eq!(c.backups_discovered, 0);
    }

    const LISTED_AT: i64 = 1_700_001_000;
    const CREATED_AT: i64 = 1_700_000_000;
    /// The namespace identity segment the producer puts in the `tenants/` position (issue #1084:
    /// this currently carries the namespace *name*, not the uid).
    const NAMESPACE_IDENTITY: &str = "ns";
    /// The Kanidm UID the scan is for, matching the `clusters/<uid>` segment of the keys below.
    const KANIDM_UID: &str = "k";

    fn listing() -> Time {
        Time(Timestamp::from_second(LISTED_AT).unwrap())
    }

    /// Mirrors `make_repository`: bucket `test-bucket`, prefix `prod`, so keys under `prod/v1/`
    /// are inside the scanned repository.
    fn repo_path() -> RepositoryPath {
        RepositoryPath::new("test-bucket", "prod").unwrap()
    }

    /// The exact prefix the discover Job lists, built with the same helper the data mover calls.
    fn scanned_prefix() -> String {
        repo_path()
            .manifests_prefix(NAMESPACE_IDENTITY, KANIDM_UID)
            .unwrap()
    }

    fn discovered_backup(
        name: &str,
        manifest_key: &str,
        kanidm_uid: &str,
        phase: KanidmBackupPhase,
    ) -> KanidmBackup {
        let mut backup = build_backup_cr(
            manifest_key,
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "corp-idm",
            kanidm_uid,
        );
        backup.metadata.name = Some(name.to_string());
        backup.metadata.uid = Some(format!("uid-{name}"));
        backup.metadata.creation_timestamp =
            Some(Time(Timestamp::from_second(CREATED_AT).unwrap()));
        backup.status = Some(KanidmBackupStatus {
            phase,
            ..Default::default()
        });
        backup
    }

    fn keys(items: &[&str]) -> HashSet<String> {
        items.iter().map(|k| (*k).to_string()).collect()
    }

    fn snapshot(keys: &[&str], total_found: u32, truncated: bool) -> DiscoverResult {
        DiscoverResult {
            manifest_keys: keys.iter().map(|key| key.to_string()).collect(),
            total_found,
            truncated,
        }
    }

    #[test]
    fn snapshot_supports_pruning_accepts_a_complete_self_consistent_snapshot() {
        assert!(snapshot_supports_pruning(&snapshot(
            &["prod/a/manifest.json", "prod/b/manifest.json"],
            2,
            false
        )));
        assert!(snapshot_supports_pruning(&snapshot(&[], 0, false)));
    }

    #[test]
    fn snapshot_supports_pruning_rejects_a_truncated_snapshot() {
        assert!(!snapshot_supports_pruning(&snapshot(
            &["prod/a/manifest.json"],
            1,
            true
        )));
    }

    #[test]
    fn snapshot_supports_pruning_rejects_a_snapshot_whose_total_disagrees() {
        assert!(
            !snapshot_supports_pruning(&snapshot(&["prod/a/manifest.json"], 2, false)),
            "a partial listing that reports more manifests than it returned can never justify a delete"
        );
        assert!(!snapshot_supports_pruning(&snapshot(&[], 7, false)));
    }

    #[test]
    fn dangling_discovered_backups_prunes_invalid_absent_record() {
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let backups = vec![discovered_backup(
            "kb-deadbeef",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&["prod/other/manifest.json"]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert_eq!(dangling, vec!["kb-deadbeef".to_string()]);
    }

    #[test]
    fn dangling_discovered_backups_keeps_record_present_in_snapshot() {
        let present = "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json";
        let backups = vec![discovered_backup(
            "kb-019c7c76",
            present,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[present]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_ready_and_discovering_records() {
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let backups = vec![
            discovered_backup("kb-ready", absent, KANIDM_UID, KanidmBackupPhase::Ready),
            discovered_backup(
                "kb-discovering",
                absent,
                KANIDM_UID,
                KanidmBackupPhase::Discovering,
            ),
        ];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_without_status() {
        let mut backup = discovered_backup(
            "kb-nostatus",
            "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json",
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        );
        backup.status = None;
        let dangling = dangling_discovered_backups(
            &[backup],
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_user_authored_records() {
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let mut backup =
            discovered_backup("kb-manual", absent, KANIDM_UID, KanidmBackupPhase::Invalid);
        if let Some(labels) = backup.metadata.labels.as_mut() {
            labels.remove(DISCOVERED_LABEL);
        }
        let dangling = dangling_discovered_backups(
            &[backup],
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_of_other_kanidm_instances() {
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let backups = vec![discovered_backup(
            "kb-deadbeef",
            absent,
            "other-uid",
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_already_deleting() {
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let mut terminating = discovered_backup(
            "kb-terminating",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        );
        terminating.metadata.deletion_timestamp = Some(Time(Timestamp::now()));
        let deleting = discovered_backup(
            "kb-deleting",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Deleting,
        );
        let deleted =
            discovered_backup("kb-deleted", absent, KANIDM_UID, KanidmBackupPhase::Deleted);
        let backups = vec![terminating, deleting, deleted];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_created_at_or_after_the_snapshot_start() {
        // A snapshot can only prove absence for records that existed before the listing began.
        // Records created at or after the listing start are kept, even if otherwise prunable.
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";

        let mut created_at_listing = discovered_backup(
            "kb-at-listing",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        );
        created_at_listing.metadata.creation_timestamp = Some(listing());

        let mut created_after_listing = discovered_backup(
            "kb-after-listing",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        );
        created_after_listing.metadata.creation_timestamp =
            Some(Time(Timestamp::from_second(LISTED_AT + 60).unwrap()));

        let backups = vec![created_at_listing, created_after_listing];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(
            dangling.is_empty(),
            "records created at or after the snapshot start must not be pruned"
        );
    }

    #[test]
    fn dangling_discovered_backups_prunes_only_records_created_before_the_snapshot_start() {
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";

        let mut old = discovered_backup("kb-old", absent, KANIDM_UID, KanidmBackupPhase::Invalid);
        old.metadata.creation_timestamp = Some(Time(Timestamp::from_second(CREATED_AT).unwrap()));

        let mut newer =
            discovered_backup("kb-newer", absent, KANIDM_UID, KanidmBackupPhase::Invalid);
        newer.metadata.creation_timestamp =
            Some(Time(Timestamp::from_second(LISTED_AT + 60).unwrap()));

        let dangling = dangling_discovered_backups(
            &[old, newer],
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert_eq!(dangling, vec!["kb-old".to_string()]);
    }

    #[test]
    fn dangling_discovered_backups_keeps_everything_without_a_snapshot_time() {
        // Without a Job timestamp there is no temporally valid evidence, so nothing is prunable.
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let backups = vec![discovered_backup(
            "kb-deadbeef",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            None,
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_without_a_creation_timestamp() {
        // A record whose creation time is unknown cannot be proven absent by this snapshot.
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let mut backup = discovered_backup(
            "kb-no-created",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        );
        backup.metadata.creation_timestamp = None;
        let dangling = dangling_discovered_backups(
            &[backup],
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_without_a_uid() {
        // The delete path pins the precondition to the uid, so a record without one cannot be
        // addressed safely and is kept.
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let mut backup =
            discovered_backup("kb-no-uid", absent, KANIDM_UID, KanidmBackupPhase::Invalid);
        backup.metadata.uid = None;
        let dangling = dangling_discovered_backups(
            &[backup],
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn dangling_discovered_backups_prunes_records_inside_the_scanned_prefix() {
        // Same record as the baseline prune case, stated explicitly against the scope guard: its
        // key sits inside the exact tenant subtree the discover Job listed.
        let absent = "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let backups = vec![discovered_backup(
            "kb-deadbeef",
            absent,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert_eq!(dangling, vec!["kb-deadbeef".to_string()]);
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_outside_the_repository_prefix() {
        // The snapshot never leaves the scanned repository, so keys under another repository
        // prefix (or escaping it) are kept.
        let other_repo_key = "other/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";
        let traversal_key = "prod/v1/tenants/../clusters/k/backups/deadbeef/manifest.json";
        let backups = vec![
            discovered_backup(
                "kb-other-repo",
                other_repo_key,
                KANIDM_UID,
                KanidmBackupPhase::Invalid,
            ),
            discovered_backup(
                "kb-traversal",
                traversal_key,
                KANIDM_UID,
                KanidmBackupPhase::Invalid,
            ),
        ];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(
            dangling.is_empty(),
            "records outside the scanned repository are never prunable: {dangling:?}"
        );
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_of_a_different_namespace_identity() {
        // Same repository and the same `clusters/<uid>` segment, but under another tenant
        // subtree. The discover Job lists exactly one `manifests_prefix`, so this snapshot can
        // never have contained the key -- the guard that keeps a producer/consumer keyspace
        // divergence (issue #1084) from turning absence-of-evidence into deletes.
        let other_tenant = "prod/v1/tenants/other-ns/clusters/k/backups/deadbeef/manifest.json";
        let backups = vec![discovered_backup(
            "kb-other-tenant",
            other_tenant,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(
            dangling.is_empty(),
            "a record under another namespace identity must survive: {dangling:?}"
        );
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_of_a_different_cluster_segment() {
        // The `clusters/<uid>` segment has to match the scanned subtree too. The record keeps the
        // scan's `kanidm_ref.uid`, so only the scope predicate can save it.
        let other_cluster = "prod/v1/tenants/ns/clusters/other-k/backups/deadbeef/manifest.json";
        let backups = vec![discovered_backup(
            "kb-other-cluster",
            other_cluster,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(
            dangling.is_empty(),
            "a record under another clusters/<uid> segment must survive: {dangling:?}"
        );
    }

    #[test]
    fn dangling_discovered_backups_keeps_records_beside_the_listed_prefix() {
        // `backups/` is confined as a path prefix including its trailing slash, so a sibling
        // directory whose name merely starts with `backups` is outside the listing.
        let sibling = "prod/v1/tenants/ns/clusters/k/backups-extra/deadbeef/manifest.json";
        let backups = vec![discovered_backup(
            "kb-sibling",
            sibling,
            KANIDM_UID,
            KanidmBackupPhase::Invalid,
        )];
        let dangling = dangling_discovered_backups(
            &backups,
            &keys(&[]),
            KANIDM_UID,
            Some(&listing()),
            &repo_path(),
            &scanned_prefix(),
        );
        assert!(
            dangling.is_empty(),
            "a sibling of the listed prefix is not inside it: {dangling:?}"
        );
    }

    #[test]
    fn discovery_metrics_new_creates_all_counters() {
        use opentelemetry::metrics::MeterProvider;
        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder().build();
        let meter = provider.meter("test");
        let metrics = DiscoveryMetrics::new(&meter);
        metrics.inc_truncations("test-repo");
        metrics.inc_discover_jobs("test-repo");
        metrics.inc_backups_discovered("test-repo", 5);
        metrics.inc_backups_reconciled("test-repo", 3);
        metrics.inc_dangling_pruned("test-repo", 2);
        metrics.record_scan_duration(1.5);
        metrics.inc_scan_failures();
        metrics.set_last_scan(1700000000);
        metrics.set_last_scan_success(1700000000);
        metrics.set_repositories_scanned(2);
    }

    // ---------------------------------------------------------------------
    // status.discovery accumulation (issue #1082)
    // ---------------------------------------------------------------------

    const OLD: i64 = 1_700_000_000;
    const NOW: i64 = 1_700_002_000;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    fn condition_at(type_: &str, status: &str, reason: &str, secs: i64) -> Condition {
        Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(ts(secs)),
            reason: reason.to_string(),
            message: format!("{type_} {status} {reason}"),
        }
    }

    fn update<'a>(
        condition_type: &'a str,
        condition_status: &'a str,
        reason: &'a str,
        message: &'a str,
        discovered_count: Option<u32>,
    ) -> DiscoveryStatusUpdate<'a> {
        DiscoveryStatusUpdate {
            condition_type,
            condition_status,
            reason,
            message,
            discovered_count,
        }
    }

    /// A schedule that succeeded at `OLD` and is now parked in "a Job was created, nothing has
    /// reported back since": exactly the frozen production status of issue #1082.
    fn listed_discovery_status() -> DiscoveryStatus {
        DiscoveryStatus {
            last_scan_time: Some(ts(OLD).to_string()),
            last_successful_scan_time: Some(ts(OLD).to_string()),
            last_discovered_count: Some(8),
            last_error: None,
            conditions: vec![
                condition_at("Discovered", "False", "Discovering", OLD),
                condition_at("DiscoveryTruncated", "True", "ResultLimitReached", OLD),
            ],
        }
    }

    fn condition<'a>(status: &'a DiscoveryStatus, type_: &str) -> &'a Condition {
        status
            .conditions
            .iter()
            .find(|c| c.type_ == type_)
            .unwrap_or_else(|| panic!("condition {type_} is missing from {status:?}"))
    }

    #[test]
    fn build_discovery_status_applies_updates_in_order_without_clobbering() {
        let base = listed_discovery_status();
        let now = ts(NOW);
        let updates = [
            update(
                "Discovered",
                "True",
                "DiscoveryComplete",
                "discovered 11 manifest(s); created 4 new, reconciled 7 existing, pruned 2 dangling",
                Some(11),
            ),
            update(
                "DiscoveryTruncated",
                "False",
                "ResultComplete",
                "Discovery completed without truncation",
                None,
            ),
        ];

        let status = build_discovery_status(&base, &updates, Some(3), now);

        // The second update must not re-declare the fields the first one wrote from the stale base.
        assert_eq!(
            status.last_successful_scan_time.as_deref(),
            Some(now.to_string().as_str()),
            "a successful tick must persist lastSuccessfulScanTime"
        );
        assert_eq!(
            status.last_discovered_count,
            Some(11),
            "a successful tick must persist lastDiscoveredCount"
        );
        assert_eq!(
            status.last_scan_time.as_deref(),
            Some(now.to_string().as_str())
        );

        let discovered = condition(&status, "Discovered");
        assert_eq!(discovered.status, "True");
        assert_eq!(discovered.reason, "DiscoveryComplete");
        assert!(
            discovered.message.contains(", pruned 2 dangling"),
            "the prune suffix must survive the tick: {}",
            discovered.message
        );
        assert_eq!(
            discovered.last_transition_time.0, now,
            "the transition must move off the stale listed timestamp"
        );
        assert_eq!(discovered.observed_generation, Some(3));

        let truncated = condition(&status, "DiscoveryTruncated");
        assert_eq!(truncated.status, "False");
        assert_eq!(truncated.reason, "ResultComplete");

        // The order the updates are folded in is the stored order, unchanged from the two
        // sequential applies this replaces.
        assert_eq!(status.conditions.len(), 2);
        assert_eq!(status.conditions[0].type_, "Discovered");
        assert_eq!(status.conditions[1].type_, "DiscoveryTruncated");
    }

    #[test]
    fn build_discovery_status_job_failure_tick_keeps_every_other_field() {
        let base = listed_discovery_status();
        let now = ts(NOW);
        let updates = [update(
            "DiscoveryFailed",
            "False",
            "DiscoverJobFailed",
            "Discover Job failed: discover: timeout; will retry",
            None,
        )];

        let status = build_discovery_status(&base, &updates, Some(1), now);

        assert_eq!(
            status.last_scan_time.as_deref(),
            Some(now.to_string().as_str())
        );
        assert_eq!(
            status.last_successful_scan_time, base.last_successful_scan_time,
            "a failed tick must not advance lastSuccessfulScanTime"
        );
        assert_eq!(status.last_discovered_count, Some(8));
        assert!(
            status.last_error.is_none(),
            "today's polarity keeps last_error to DiscoveryFailed=True only"
        );
        assert_eq!(status.conditions.len(), 3);
        let failed = condition(&status, "DiscoveryFailed");
        assert_eq!(failed.status, "False");
        assert_eq!(failed.reason, "DiscoverJobFailed");
        assert_eq!(failed.last_transition_time.0, now);
        // Untouched siblings keep their timestamps.
        assert_eq!(
            condition(&status, "Discovered").last_transition_time.0,
            ts(OLD)
        );
        assert_eq!(
            condition(&status, "DiscoveryTruncated")
                .last_transition_time
                .0,
            ts(OLD)
        );
    }

    #[test]
    fn build_discovery_status_successful_scan_tick_advances_success_time_and_count() {
        let base = listed_discovery_status();
        let now = ts(NOW);
        let updates = [update(
            "Discovered",
            "True",
            "DiscoveryComplete",
            "discovered 11 manifest(s); created 11 new, reconciled 0 existing",
            Some(11),
        )];

        let status = build_discovery_status(&base, &updates, Some(1), now);

        assert_eq!(
            status.last_successful_scan_time.as_deref(),
            Some(now.to_string().as_str())
        );
        assert_eq!(status.last_discovered_count, Some(11));
        assert_eq!(condition(&status, "Discovered").last_transition_time.0, now);
    }

    #[test]
    fn build_discovery_status_truncated_tick_sets_truncated_true_and_keeps_counters() {
        // The listed object last reported a complete scan; this one hits the result cap.
        let mut base = listed_discovery_status();
        base.conditions[1] = condition_at("DiscoveryTruncated", "False", "ResultComplete", OLD);
        let now = ts(NOW);
        let updates = [update(
            "DiscoveryTruncated",
            "True",
            "ResultLimitReached",
            "Discovery found 1000 manifests but results were truncated; some backups may not be represented",
            None,
        )];

        let status = build_discovery_status(&base, &updates, Some(1), now);

        let truncated = condition(&status, "DiscoveryTruncated");
        assert_eq!(truncated.status, "True");
        assert_eq!(truncated.reason, "ResultLimitReached");
        assert_eq!(
            truncated.last_transition_time.0, now,
            "flipping the truncation state is a transition"
        );
        assert_eq!(status.last_discovered_count, Some(8));
        assert_eq!(
            status.last_successful_scan_time,
            base.last_successful_scan_time
        );
    }

    #[test]
    fn build_discovery_status_complete_tick_sets_truncated_false_and_is_idempotent() {
        let base = listed_discovery_status();
        let now = ts(NOW);
        let complete = update(
            "DiscoveryTruncated",
            "False",
            "ResultComplete",
            "Discovery completed without truncation",
            None,
        );

        let status = build_discovery_status(&base, &[complete], Some(1), now);
        assert_eq!(condition(&status, "DiscoveryTruncated").status, "False");
        assert_eq!(
            condition(&status, "DiscoveryTruncated").reason,
            "ResultComplete"
        );
        assert_eq!(
            condition(&status, "DiscoveryTruncated")
                .last_transition_time
                .0,
            now
        );

        // Re-applying the very same signal must not drag lastTransitionTime along with it, which
        // is what keeps the stale window honest on fresh ticks.
        let later = build_discovery_status(&status, &[complete], Some(1), ts(NOW + 300));
        assert_eq!(
            condition(&later, "DiscoveryTruncated")
                .last_transition_time
                .0,
            now
        );
    }

    #[test]
    fn build_discovery_status_invalid_result_tick_keeps_failure_polarity() {
        let base = listed_discovery_status();
        let now = ts(NOW);
        let updates = [update(
            "DiscoveryFailed",
            "False",
            "DiscoverResultInvalid",
            "Discover result invalid: discover: bad json",
            None,
        )];

        let status = build_discovery_status(&base, &updates, Some(1), now);

        let failed = condition(&status, "DiscoveryFailed");
        assert_eq!(failed.status, "False");
        assert_eq!(failed.reason, "DiscoverResultInvalid");
        assert_eq!(
            condition(&status, "Discovered").status,
            "False",
            "an invalid result must not flip Discovered"
        );
        assert_eq!(status.last_discovered_count, Some(8));
        assert!(status.last_error.is_none());
    }

    #[test]
    fn build_discovery_status_fresh_tick_reapplies_the_signal_verbatim() {
        let base = listed_discovery_status();
        let now = ts(NOW);
        let signal = condition(&base, "Discovered");
        let message = signal.message.clone();
        let updates = [update("Discovered", "False", "Discovering", &message, None)];

        let status = build_discovery_status(&base, &updates, Some(1), now);

        assert_eq!(
            status.last_scan_time.as_deref(),
            Some(now.to_string().as_str())
        );
        let discovered = condition(&status, "Discovered");
        assert_eq!(discovered.status, "False");
        assert_eq!(discovered.reason, "Discovering");
        assert_eq!(discovered.message, signal.message);
        assert_eq!(
            discovered.last_transition_time.0,
            ts(OLD),
            "a fresh tick must not reset the staleness anchor"
        );
    }

    #[test]
    fn build_discovery_status_job_created_tick_marks_discovered_false_discovering() {
        let mut base = listed_discovery_status();
        base.conditions = vec![condition_at("Discovered", "True", "DiscoveryComplete", OLD)];
        let now = ts(NOW);
        let updates = [update(
            "Discovered",
            "False",
            "Discovering",
            "Discover Job created",
            None,
        )];

        let status = build_discovery_status(&base, &updates, Some(1), now);

        let discovered = condition(&status, "Discovered");
        assert_eq!(discovered.status, "False");
        assert_eq!(discovered.reason, "Discovering");
        assert_eq!(discovered.last_transition_time.0, now);
        assert_eq!(
            status.last_successful_scan_time, base.last_successful_scan_time,
            "starting a scan must not rewrite the last success"
        );
        assert_eq!(status.last_discovered_count, Some(8));
    }

    #[test]
    fn discovery_status_never_drops_conditions_it_does_not_update() {
        // `status.discovery.conditions` is an `x-kubernetes-list-type: map` keyed on `type`. An
        // apply that omits a condition the same manager owns releases it, and the API server
        // deletes it, so every apply has to declare the whole merged list.
        let base = DiscoveryStatus {
            last_scan_time: Some(ts(OLD).to_string()),
            last_successful_scan_time: None,
            last_discovered_count: None,
            last_error: None,
            conditions: vec![
                condition_at("Discovered", "False", "Discovering", OLD),
                condition_at("DiscoveryTruncated", "False", "ResultComplete", OLD + 5),
                condition_at("Ready", "True", "Configured", OLD + 7),
            ],
        };

        let status = build_discovery_status(
            &base,
            &[update(
                "Discovered",
                "True",
                "DiscoveryComplete",
                "discovered 3 manifest(s)",
                Some(3),
            )],
            Some(1),
            ts(NOW),
        );

        assert_eq!(status.conditions.len(), 3, "got {:?}", status.conditions);
        let truncated = condition(&status, "DiscoveryTruncated");
        assert_eq!(truncated.status, "False");
        assert_eq!(truncated.last_transition_time.0, ts(OLD + 5));
        let ready = condition(&status, "Ready");
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Configured");
        assert_eq!(ready.last_transition_time.0, ts(OLD + 7));
    }

    #[test]
    fn discovery_status_patch_excludes_schedule_conditions() {
        let base = listed_discovery_status();
        let now = ts(NOW);
        let status = build_discovery_status(
            &base,
            &[
                update(
                    "Discovered",
                    "True",
                    "DiscoveryComplete",
                    "discovered 11 manifest(s); created 4 new, reconciled 7 existing, pruned 2 dangling",
                    Some(11),
                ),
                update(
                    "DiscoveryTruncated",
                    "False",
                    "ResultComplete",
                    "Discovery completed without truncation",
                    None,
                ),
            ],
            Some(1),
            now,
        );
        let patch = discovery_status_patch(&status);
        let keys = patch.as_object().unwrap().keys().collect::<Vec<_>>();
        assert_eq!(keys, vec!["apiVersion", "kind", "status"]);
        assert_eq!(patch["apiVersion"], "kaniop.rs/v1alpha1");
        assert_eq!(patch["kind"], "KanidmBackupSchedule");

        // Only `status.discovery` is declared; the schedule's own `status.conditions`,
        // `observedGeneration` and backup counters belong to the schedule controller.
        let status_keys = patch["status"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>();
        assert_eq!(
            status_keys,
            vec!["discovery"],
            "the discovery apply must not declare the schedule's own status fields"
        );

        // And it declares the whole discovery object, including every condition it owns.
        let discovery = &patch["status"]["discovery"];
        assert!(discovery["lastScanTime"].is_string());
        assert!(discovery["lastSuccessfulScanTime"].is_string());
        assert_eq!(discovery["lastDiscoveredCount"], 11u32);
        assert!(discovery["lastError"].is_null(), "unset fields stay out");
        let types: Vec<&str> = discovery["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, vec!["Discovered", "DiscoveryTruncated"]);
    }

    // ---------------------------------------------------------------------
    // staleness gate
    // ---------------------------------------------------------------------

    fn gate(conditions: &[Condition], now_secs: i64) -> (bool, f64, String) {
        evaluate_discovery_staleness(
            last_discovery_signal(conditions),
            ts(now_secs),
            STALE_THRESHOLD,
        )
    }

    #[test]
    fn staleness_gate_uses_the_most_recent_of_discovered_and_failed() {
        let failed = condition_at("DiscoveryFailed", "False", "DiscoverJobFailed", NOW - 3_000);
        let discovered = condition_at("Discovered", "True", "DiscoveryComplete", NOW - 100);

        // Order A is what a successful scan leaves behind: `merge_conditions` re-appends the
        // replaced `Discovered` at the end, so the older `DiscoveryFailed` comes first.
        let order_a = vec![failed.clone(), discovered.clone()];
        assert_eq!(
            last_discovery_signal(&order_a).unwrap().type_,
            "Discovered",
            "the gate must follow the latest transition, not the stored order"
        );
        let (is_stale, elapsed, _) = gate(&order_a, NOW);
        assert!(!is_stale, "discovery succeeded 100 s ago: {elapsed}");
        assert_eq!(elapsed, 100.0);

        // Order B is the same facts with the conditions the other way round.
        let order_b = vec![discovered.clone(), failed.clone()];
        assert_eq!(last_discovery_signal(&order_b).unwrap().type_, "Discovered");
        assert!(!gate(&order_b, NOW).0);

        // A newer failure is the signal too: retries are measured from the failure.
        let failed_later = condition_at("DiscoveryFailed", "False", "DiscoverJobFailed", NOW - 50);
        let order_c = vec![discovered.clone(), failed_later.clone()];
        assert_eq!(
            last_discovery_signal(&order_c).unwrap().type_,
            "DiscoveryFailed"
        );
        assert_eq!(gate(&order_c, NOW).1, 50.0);

        // Repeated identical failures preserve `failed`'s timestamp, so a stale `Discovered`
        // behind it must not keep the schedule pinned to the failure forever.
        let (is_stale, elapsed, _) = gate(&order_a, NOW + 2_000);
        assert!(is_stale, "{elapsed} since the newest signal");
        assert_eq!(elapsed, 2_100.0);
    }

    #[test]
    fn staleness_gate_other_conditions_are_not_signals() {
        let conditions = vec![
            condition_at("Ready", "True", "Configured", NOW - 10_000),
            condition_at(
                "DiscoveryTruncated",
                "False",
                "ResultComplete",
                NOW - 10_000,
            ),
            condition_at("Discovered", "True", "DiscoveryComplete", NOW - 60),
        ];
        let signal = last_discovery_signal(&conditions).unwrap();
        assert_eq!(signal.type_, "Discovered");
        assert!(!gate(&conditions, NOW).0);
    }

    #[test]
    fn staleness_gate_boundary_equal_threshold_is_not_stale() {
        let signal = condition_at("Discovered", "True", "DiscoveryComplete", OLD);
        let threshold = STALE_THRESHOLD.as_secs() as i64;

        let (is_stale, elapsed, last_scan) = gate(std::slice::from_ref(&signal), OLD + threshold);
        assert!(
            !is_stale,
            "elapsed {elapsed} exactly equal to the threshold stays fresh"
        );
        assert_eq!(elapsed, threshold as f64);
        assert_eq!(last_scan, ts(OLD).to_string());

        let (is_stale, elapsed, _) = gate(&[signal], OLD + threshold + 1);
        assert!(is_stale, "one second past the threshold is stale");
        assert_eq!(elapsed, threshold as f64 + 1.0);
    }

    #[test]
    fn staleness_gate_without_any_signal_is_stale() {
        // No signal at all is what bootstraps the very first discover Job.
        let (is_stale, elapsed, last_scan) = gate(&[], NOW);
        assert!(is_stale);
        assert_eq!(elapsed, 0.0);
        assert_eq!(last_scan, "");

        let no_signals = vec![condition_at("Ready", "True", "Configured", NOW)];
        assert!(last_discovery_signal(&no_signals).is_none());
        assert!(gate(&no_signals, NOW).0);
    }

    #[test]
    fn healthy_schedule_recreates_a_job_on_the_stale_window_not_every_tick() {
        // Cadence documentation (issue #1082 behaviour change): a successful scan stamps
        // `Discovered` on its tick, and the next Job is created on the first tick strictly past the
        // stale threshold. With the defaults (300 s tick, 300 s threshold) that is 2 ticks after
        // the success stamp, i.e. 3 ticks from Job creation to Job creation (900 s).
        let success = condition_at("Discovered", "True", "DiscoveryComplete", 0);
        let tick = DEFAULT_SCAN_INTERVAL.as_secs() as i64;
        let ticks_until_stale = (1..=10)
            .find(|n| gate(std::slice::from_ref(&success), tick * n).0)
            .expect("a schedule that never scans must eventually go stale");

        assert_eq!(ticks_until_stale, 2);
        assert_eq!(tick * ticks_until_stale, 600);
        // One more tick to consume the Job that tick created: the real period is 3 ticks.
        assert_eq!(tick * (ticks_until_stale + 1), 900);
        assert_eq!(STALE_THRESHOLD.as_secs(), 300);
    }

    /// Seam contract test (issue #1084): exercises the full `run_discovery_scan` flow with mocked
    /// API responses to verify the discover Job's operation document carries the namespace *name*
    /// in `namespaceUid`, not the Namespace object's metadata.uid. The producers
    /// (`libs/operator/src/kanidm/reconcile/transport.rs` and restore's `legacy.rs`) write
    /// `kanidm.namespace()` into the `tenants/` segment, so this consumer must agree or every
    /// existing object is orphaned. This test uses fixtures where the namespace name, namespace
    /// UID, and Kanidm UID are all unmistakably different, and verifies the Job creation request
    /// contains the namespace name. If the code were to flip to passing the actual UID, this test
    /// would fail.
    #[tokio::test]
    async fn run_discovery_scan_passes_namespace_name_not_uid_to_job() {
        use k8s_openapi::api::core::v1::Namespace;
        use kube::runtime::reflector::store::Writer;
        use kube::runtime::watcher;
        use opentelemetry::metrics::MeterProvider;

        // Fixtures with unmistakably distinct identities
        let namespace_name = "production";
        let namespace_uid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let kanidm_name = "corp-idm";
        let kanidm_uid = "11111111-2222-3333-4444-555555555555";
        let repo_name = "offsite";
        let schedule_name = "daily";

        // Repository with Ready=True, reason=Accepted status
        let mut repo = make_repository(repo_name);
        repo.metadata.namespace = Some(namespace_name.to_string());
        repo.status = Some(crate::crd::KanidmBackupRepositoryStatus {
            observed_generation: Some(1),
            conditions: vec![Condition {
                type_: "Ready".to_string(),
                status: "True".to_string(),
                observed_generation: Some(1),
                last_transition_time: Time(Timestamp::now()),
                reason: "Accepted".to_string(),
                message: "Repository accepted".to_string(),
            }],
        });

        // Schedule referencing the repo and Kanidm
        let mut schedule = make_schedule(schedule_name, repo_name, kanidm_name);
        schedule.metadata.namespace = Some(namespace_name.to_string());

        // Kanidm with namespace and uid
        let mut kanidm = kaniop_operator::kanidm::crd::Kanidm::default();
        kanidm.metadata.name = Some(kanidm_name.to_string());
        kanidm.metadata.namespace = Some(namespace_name.to_string());
        kanidm.metadata.uid = Some(kanidm_uid.to_string());

        // Namespace with name and uid
        let mut namespace = Namespace::default();
        namespace.metadata.name = Some(namespace_name.to_string());
        namespace.metadata.uid = Some(namespace_uid.to_string());

        // Reuse the shared `tower_test` mock-service harness from `controller/testutil.rs` for the
        // client, request handle and meter provider. `run_discovery_scan` takes a `State` (not a
        // `Context`), so the mock client is taken from the context and a `State` with the Kanidm
        // pre-populated in its store is built alongside it.
        let (ctx, mut handle, meter_provider) = crate::controller::testutil::status_context::<
            KanidmBackupSchedule,
        >(DISCOVERY_CONTROLLER_ID);
        let mock_client = ctx.client.clone();

        // Populate kanidm_store so get_kanidm_for_schedule finds it without an API call.
        let mut kanidm_writer = Writer::default();
        kanidm_writer.apply_watcher_event(&watcher::Event::Apply(kanidm.clone()));

        let meter = meter_provider.meter("test");
        let metrics = kaniop_operator::metrics::Metrics::new(&meter, &[DISCOVERY_CONTROLLER_ID]);
        let state = State::new(
            metrics,
            Writer::default().as_reader(),
            kanidm_writer.as_reader(),
            None,
        );

        let discovery_metrics = DiscoveryMetrics::new(&meter_provider.meter("discovery"));

        // Spawn mock API server
        let mock_srv = tokio::spawn(async move {
            // 1. List repositories (all namespaces)
            let (request, send) = handle.next_request().await.expect("list repos");
            assert_eq!(request.method(), http::Method::GET);
            assert!(
                request
                    .uri()
                    .to_string()
                    .contains("/apis/kaniop.rs/v1alpha1/kanidmbackuprepositories")
            );
            let repo_list = serde_json::json!({
                "apiVersion": "kaniop.rs/v1alpha1",
                "kind": "KanidmBackupRepositoryList",
                "metadata": {},
                "items": [repo]
            });
            send.send_response(
                Response::builder()
                    .body(Body::from(serde_json::to_vec(&repo_list).unwrap()))
                    .unwrap(),
            );

            // 2. List schedules (in namespace)
            let (request, send) = handle.next_request().await.expect("list schedules");
            assert_eq!(request.method(), http::Method::GET);
            assert!(request.uri().to_string().contains(&format!(
                "/apis/kaniop.rs/v1alpha1/namespaces/{}/kanidmbackupschedules",
                namespace_name
            )));
            let schedule_list = serde_json::json!({
                "apiVersion": "kaniop.rs/v1alpha1",
                "kind": "KanidmBackupScheduleList",
                "metadata": {},
                "items": [schedule]
            });
            send.send_response(
                Response::builder()
                    .body(Body::from(serde_json::to_vec(&schedule_list).unwrap()))
                    .unwrap(),
            );

            // 3. Get Namespace
            let (request, send) = handle.next_request().await.expect("get namespace");
            assert_eq!(request.method(), http::Method::GET);
            assert!(
                request
                    .uri()
                    .to_string()
                    .contains(&format!("/api/v1/namespaces/{}", namespace_name))
            );
            send.send_response(
                Response::builder()
                    .body(Body::from(serde_json::to_vec(&namespace).unwrap()))
                    .unwrap(),
            );

            // 4. List discover Jobs (with labels)
            let (request, send) = handle.next_request().await.expect("list jobs");
            assert_eq!(request.method(), http::Method::GET);
            assert!(request.uri().to_string().contains(&format!(
                "/apis/batch/v1/namespaces/{}/jobs",
                namespace_name
            )));
            assert!(request.uri().to_string().contains("labelSelector"));
            let job_list = serde_json::json!({
                "apiVersion": "batch/v1",
                "kind": "JobList",
                "metadata": {},
                "items": []
            });
            send.send_response(
                Response::builder()
                    .body(Body::from(serde_json::to_vec(&job_list).unwrap()))
                    .unwrap(),
            );

            // 5. Create Job - capture and verify
            let (request, send) = handle.next_request().await.expect("create job");
            assert_eq!(request.method(), http::Method::POST);
            assert!(request.uri().to_string().contains(&format!(
                "/apis/batch/v1/namespaces/{}/jobs",
                namespace_name
            )));

            let req_body = request.into_body().collect_bytes().await.unwrap();
            let job: Job = serde_json::from_slice(&req_body).expect("job is valid JSON");

            // Extract operation JSON from Job spec
            let pod_spec = job.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
            let args = pod_spec.containers[0].args.as_ref().unwrap();
            let op_json = &args[3];
            let op: serde_json::Value = serde_json::from_str(op_json).unwrap();

            // THE CRITICAL ASSERTION: namespaceUid must be the namespace NAME, not the UID
            assert_eq!(
                op["namespaceUid"], namespace_name,
                "namespaceUid must carry the namespace name '{}', not the UID '{}'",
                namespace_name, namespace_uid
            );
            assert_ne!(
                op["namespaceUid"], namespace_uid,
                "namespaceUid must not carry the Namespace object UID"
            );

            // Send back the Job as if created
            send.send_response(
                Response::builder()
                    .body(Body::from(serde_json::to_vec(&job).unwrap()))
                    .unwrap(),
            );

            // 6. Patch schedule status
            let (request, send) = handle.next_request().await.expect("patch status");
            assert_eq!(request.method(), http::Method::PATCH);
            assert!(request.uri().to_string().contains(&format!(
                "/apis/kaniop.rs/v1alpha1/namespaces/{}/kanidmbackupschedules/{}/status",
                namespace_name, schedule_name
            )));
            let mut schedule_with_status = schedule.clone();
            schedule_with_status.status = Some(crate::crd::KanidmBackupScheduleStatus {
                observed_generation: Some(1),
                last_discovered_backup_ref: None,
                last_successful_backup_time: None,
                conditions: vec![],
                discovery: Some(DiscoveryStatus::default()),
            });
            send.send_response(
                Response::builder()
                    .body(Body::from(
                        serde_json::to_vec(&schedule_with_status).unwrap(),
                    ))
                    .unwrap(),
            );
        });

        // Run the discovery scan
        let result = run_discovery_scan(&state, &mock_client, &discovery_metrics).await;
        assert!(
            result.is_ok(),
            "run_discovery_scan failed: {:?}",
            result.err()
        );

        // Wait for mock server to complete
        tokio::time::timeout(std::time::Duration::from_secs(2), mock_srv)
            .await
            .expect("timeout on mock apiserver")
            .expect("mock server completed");
    }
}
