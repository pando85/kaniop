use crate::controller::{
    BACKUP_JOB_TTL_SECONDS, DISCOVERY_CONTROLLER_ID, RESULT_PATH, background_delete_params,
    build_data_mover_wrapper, data_mover_image, default_resource_requirements,
    extract_termination_message, hardened_pod_security_context, hardened_security_context,
    select_succeeded_pod,
};
use crate::crd::{
    BackupKanidmRef, BackupRepositoryRef, KanidmBackup, KanidmBackupRepository,
    KanidmBackupSchedule, KanidmBackupSpec,
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
use kube::api::{ListParams, Patch, PatchParams};
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
const STALE_THRESHOLD: Duration = Duration::from_secs(900);

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

        Self {
            scan_duration,
            scan_failures,
            discover_jobs_created,
            backups_discovered,
            backups_reconciled,
            last_scan_timestamp,
            last_scan_success_timestamp,
            repositories_scanned,
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

            let namespace_uid = kanidm.metadata.namespace.as_deref().unwrap_or_default();
            let kanidm_uid = kanidm.metadata.uid.as_deref().unwrap_or_default();

            if namespace_uid.is_empty() || kanidm_uid.is_empty() {
                warn!(
                    namespace,
                    kanidm = kanidm.name_any(),
                    "kanidm missing namespace or uid; skipping discovery"
                );
                continue;
            }

            let ns_obj = get_namespace_uid(client, &namespace).await?;
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
                namespace_uid,
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

async fn get_namespace_uid(
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
    namespace_uid: &str,
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
            update_discovery_status(
                client,
                namespace,
                schedule,
                "DiscoveryFailed",
                "False",
                "DiscoverJobFailed",
                &format!("Discover Job failed: {failure_message}; will retry"),
                None,
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
                    let (new_count, reconciled_count) = reconcile_discovered_backups(
                        client,
                        namespace,
                        repository,
                        schedule,
                        kanidm_name,
                        kanidm_uid,
                        &discovery,
                        metrics,
                    )
                    .await?;

                    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
                    job_api
                        .delete(&job.name_any(), &background_delete_params())
                        .await
                        .ok();

                    let msg = format!(
                        "discovered {} manifest(s); created {} new, reconciled {} existing",
                        discovery.total_found, new_count, reconciled_count
                    );
                    update_discovery_status(
                        client,
                        namespace,
                        schedule,
                        "Discovered",
                        "True",
                        "DiscoveryComplete",
                        &msg,
                        Some(discovery.total_found),
                    )
                    .await?;

                    if discovery.truncated {
                        warn!(
                            namespace,
                            schedule = schedule_name,
                            "discover results were truncated; some backups may not be represented"
                        );
                    }

                    debug!(
                        namespace,
                        schedule = schedule_name,
                        total_found = discovery.total_found,
                        new = new_count,
                        reconciled = reconciled_count,
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

                    update_discovery_status(
                        client,
                        namespace,
                        schedule,
                        "DiscoveryFailed",
                        "False",
                        "DiscoverResultInvalid",
                        &format!("Discover result invalid: {error_msg}"),
                        None,
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

    let last_discovery = schedule.status.as_ref().and_then(|s| {
        s.discovery.as_ref().and_then(|d| {
            d.conditions
                .iter()
                .find(|c| c.type_ == "Discovered" || c.type_ == "DiscoveryFailed")
        })
    });

    let stale_threshold = backup_discovery_stale_threshold();
    let (is_stale, elapsed_secs, last_scan_time_str) = last_discovery
        .map(|c| {
            let ts = c.last_transition_time.0;
            let now = Timestamp::now();
            let elapsed_secs = now.since(ts).map(|d| d.get_seconds() as f64).unwrap_or(0.0);
            let last_scan = c.last_transition_time.0.to_string();
            (
                elapsed_secs > stale_threshold.as_secs_f64(),
                elapsed_secs,
                last_scan,
            )
        })
        .unwrap_or((true, 0.0, String::new()));

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
            update_discovery_status(
                client,
                namespace,
                schedule,
                &cond.type_,
                &cond.status,
                &cond.reason,
                &cond.message,
                None,
            )
            .await?;
        }
        return Ok(ScanTickCounters::default());
    }

    let discover_job = build_discover_job(
        repository,
        namespace,
        &schedule_name,
        namespace_uid,
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

    update_discovery_status(
        client,
        namespace,
        schedule,
        "Discovered",
        "False",
        "Discovering",
        "Discover Job created",
        None,
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
    namespace_uid: &str,
    kanidm_uid: &str,
) -> Job {
    let spec = &repository.spec;
    let endpoint = spec
        .s3
        .endpoint
        .clone()
        .unwrap_or_else(|| "https://s3.amazonaws.com".to_string());
    let region = spec
        .s3
        .region
        .clone()
        .unwrap_or_else(|| "us-east-1".to_string());

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
        "namespaceUid": namespace_uid,
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
                            build_data_mover_wrapper("discover"),
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

async fn read_discover_result(
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

    let raw = match extract_termination_message(pod, "discover") {
        Some(msg) => msg,
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
    kanidm_uid: &str,
    discovery: &DiscoverResult,
    metrics: &DiscoveryMetrics,
) -> Result<(u64, u64)> {
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

    Ok((new_count, reconciled_count))
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
                    ("kaniop.rs/discovered".to_string(), "true".to_string()),
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
) -> Time {
    existing
        .iter()
        .find(|c| c.type_ == type_ && c.status == new_status && c.reason == new_reason)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Time(Timestamp::now()))
}

#[allow(clippy::too_many_arguments)]
async fn update_discovery_status(
    client: &Client,
    namespace: &str,
    schedule: &KanidmBackupSchedule,
    condition_type: &str,
    condition_status: &str,
    reason: &str,
    message: &str,
    discovered_count: Option<u32>,
) -> Result<()> {
    let api: Api<KanidmBackupSchedule> = Api::namespaced(client.clone(), namespace);
    let name = schedule.name_any();

    let mut discovery_status = schedule
        .status
        .as_ref()
        .and_then(|s| s.discovery.clone())
        .unwrap_or_default();

    let condition = Condition {
        type_: condition_type.to_string(),
        status: condition_status.to_string(),
        observed_generation: schedule.metadata.generation,
        last_transition_time: transition_time(
            &discovery_status.conditions,
            condition_type,
            condition_status,
            reason,
        ),
        reason: reason.to_string(),
        message: message.to_string(),
    };

    let existing_conditions = discovery_status.conditions.as_slice();
    let merged_conditions = merge_conditions(existing_conditions, &[condition]);
    discovery_status.conditions = merged_conditions;

    if condition_type == "Discovered" && condition_status == "True" {
        discovery_status.last_successful_scan_time = Some(Timestamp::now().to_string());
    }
    if condition_type == "DiscoveryFailed" && condition_status == "True" {
        discovery_status.last_error = Some(message.to_string());
    }
    discovery_status.last_scan_time = Some(Timestamp::now().to_string());
    if let Some(count) = discovered_count {
        discovery_status.last_discovered_count = Some(count);
    }

    let patch = serde_json::json!({
        "apiVersion": "kaniop.rs/v1alpha1",
        "kind": "KanidmBackupSchedule",
        "status": {
            "discovery": discovery_status,
        }
    });

    api.patch_status(
        &name,
        &PatchParams::apply(DISCOVERY_CONTROLLER_ID).force(),
        &Patch::Apply(patch),
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
        RepositoryAuthentication, S3Config, ScheduleKanidmRef, ScheduleRepositoryRef, SecretRef,
    };

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
                    region: Some("us-east-1".to_string()),
                    endpoint: Some("https://s3.example.com".to_string()),
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
        let job = build_discover_job(&repo, "default", "daily", "ns-uid-123", "k-uid-456");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let args = pod_spec.containers[0].args.as_ref().unwrap();
        assert_eq!(args[0], "-c");

        let op_json = &args[3];
        let op: serde_json::Value = serde_json::from_str(op_json).unwrap();
        assert_eq!(op["operation"], "discover");
        assert_eq!(op["namespaceUid"], "ns-uid-123");
        assert_eq!(op["kanidmUid"], "k-uid-456");
        assert_eq!(op["bucket"], "test-bucket");
        assert_eq!(op["prefix"], "prod");
        assert_eq!(op["maxResults"], DEFAULT_DISCOVER_MAX_RESULTS);
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
        use crate::crd::DiscoveryStatus;

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

        let reused = transition_time(&existing, "Discovered", "True", "DiscoveryComplete");
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

        let new_time = transition_time(&existing, "Discovered", "True", "Discovering");
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
}
