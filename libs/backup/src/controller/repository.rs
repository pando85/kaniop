use crate::controller::{
    RESULT_PATH, build_data_mover_wrapper, data_mover_image, default_resource_requirements,
    extract_termination_message, hardened_pod_security_context, hardened_security_context,
    select_succeeded_pod,
};
use crate::crd::{KanidmBackupRepository, RepositoryCapabilities};

use kaniop_backup_core::auth::{
    AuthRole, build_auth_env_vars, build_auth_volume_mounts, build_auth_volumes,
    build_ca_bundle_volume, build_ca_bundle_volume_mount, ca_bundle_env_var, ca_bundle_path,
};
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{ControllerId, State, check_api_queryable, error_policy};

use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kaniop_k8s_util::error::{Error, Result};
use kube::api::{ListParams, ObjectMeta, Patch, PatchParams};
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::watcher::Config;
use kube::{Api, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, info};

pub const CONTROLLER_ID: ControllerId = "backup-repository";
const PROBE_JOB_PREFIX: &str = "kaniop-repo-probe";
const REQUEUE_PROBE_PENDING: Duration = Duration::from_secs(10);
const REQUEUE_PROBE_COMPLETE: Duration = Duration::from_secs(300);

pub async fn run(state: State, client: Client) {
    let repository = check_api_queryable::<KanidmBackupRepository>(client.clone()).await;

    let ctx = Arc::new(state.to_context(client, CONTROLLER_ID));

    info!(msg = format!("starting {CONTROLLER_ID} controller"));
    let repository_controller = Controller::new(repository, Config::default().any_semantic())
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_repository),
            error_policy,
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.metrics.ready_set(1);
    tokio::join!(repository_controller);
}

fn probe_job_name(repository_name: &str) -> String {
    let suffix = repository_name
        .trim_matches('-')
        .chars()
        .take(20)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string();
    format!("{PROBE_JOB_PREFIX}-{suffix}")
}

fn build_probe_job(repository: &KanidmBackupRepository, namespace: &str) -> Job {
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

    let repo_path = RepositoryPath::new(&spec.s3.bucket, &spec.s3.prefix);
    let _probe_key = repo_path
        .map(|p| p.probe_key())
        .unwrap_or_else(|_| format!("{}/v1/.kaniop-probe", spec.s3.prefix));

    let ca_bundle_path = spec.s3.ca_bundle_ref.as_ref().map(|_| ca_bundle_path());

    let operation_json = serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "probe",
        "bucket": spec.s3.bucket,
        "prefix": spec.s3.prefix,
        "endpoint": endpoint,
        "region": region,
        "forcePathStyle": spec.s3.force_path_style,
        "caBundlePath": ca_bundle_path,
        "resultPath": RESULT_PATH,
    });

    let auth_method = &spec.authentication.writer;
    let mut env_vars = build_auth_env_vars(auth_method, &repository.name_any(), AuthRole::Writer);
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

    Job {
        metadata: ObjectMeta {
            name: Some(probe_job_name(&repository.name_any())),
            namespace: Some(namespace.to_string()),
            labels: Some(
                [
                    (
                        "app.kubernetes.io/managed-by".to_string(),
                        "kaniop".to_string(),
                    ),
                    ("kaniop.rs/repository".to_string(), repository.name_any()),
                    ("kaniop.rs/operation".to_string(), "probe".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    automount_service_account_token: Some(false),
                    restart_policy: Some("Never".to_string()),
                    security_context: Some(hardened_pod_security_context()),
                    containers: vec![Container {
                        name: "probe".to_string(),
                        image: Some(data_mover_image()),
                        command: Some(vec!["/bin/sh".to_string()]),
                        args: Some(vec![
                            "-c".to_string(),
                            build_data_mover_wrapper("probe"),
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

async fn find_probe_job(
    client: &Client,
    namespace: &str,
    repository_name: &str,
) -> Result<Option<Job>> {
    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!(
        "kaniop.rs/repository={repository_name},kaniop.rs/operation=probe"
    ));
    let jobs = job_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list probe jobs for repository {namespace}/{repository_name}"),
            Box::new(e),
        )
    })?;
    Ok(jobs.items.into_iter().next())
}

async fn read_probe_result(
    client: &Client,
    namespace: &str,
    job: &Job,
) -> Result<Option<ResultDocument>> {
    let job_name = job.name_any();
    let pod_api: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!("job-name={job_name}"));
    let pods = pod_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list pods for probe job {namespace}/{job_name}"),
            Box::new(e),
        )
    })?;

    let pod = match select_succeeded_pod(&pods.items) {
        Some(p) => p,
        None => return Ok(None),
    };

    let raw = match extract_termination_message(pod, "probe") {
        Some(msg) => msg,
        None => return Ok(None),
    };

    let doc = kaniop_backup_core::result::parse_result_document(&raw).map_err(|e| {
        Error::MissingData(format!(
            "probe result document for {namespace}/{job_name} is invalid: {e}"
        ))
    })?;
    Ok(Some(doc))
}

fn probe_capabilities(result: &ResultDocument) -> Option<RepositoryCapabilities> {
    let probe = result.probe.as_ref()?;
    Some(RepositoryCapabilities {
        multipart_upload: probe.multipart_upload,
        conditional_put: probe.conditional_put,
        object_lock: false,
    })
}

async fn reconcile_repository(
    obj: Arc<KanidmBackupRepository>,
    ctx: Arc<kaniop_operator::controller::context::Context<KanidmBackupRepository>>,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    debug!(msg = "reconciling KanidmBackupRepository", %namespace, %name);

    let spec = &obj.spec;

    if spec.s3.bucket.is_empty() {
        return Err(Error::MissingData("bucket is required".to_string()));
    }

    if spec.s3.prefix.contains("..") {
        return Err(Error::MissingData(
            "prefix contains path traversal".to_string(),
        ));
    }

    if let Some(endpoint) = &spec.s3.endpoint {
        if !endpoint.starts_with("https://") {
            return Err(Error::MissingData("endpoint must use HTTPS".to_string()));
        }
    }

    RepositoryPath::new(&spec.s3.bucket, &spec.s3.prefix)
        .map_err(|e| Error::MissingData(format!("invalid repository path: {e}")))?;

    let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), &namespace);
    let existing_job = find_probe_job(&ctx.client, &namespace, &name).await?;

    let mut status = obj.status.clone().unwrap_or_default();
    status.observed_generation = obj.metadata.generation;

    if existing_job.is_none()
        && let Some(last_probe_time) = status.last_probe_time.as_deref()
        && status
            .conditions
            .iter()
            .any(|condition| condition.type_ == "Ready")
        && last_probe_time.parse::<Timestamp>().is_ok_and(|timestamp| {
            Timestamp::now().duration_since(timestamp).as_secs_f64()
                < REQUEUE_PROBE_COMPLETE.as_secs_f64()
        })
    {
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_PROBE_COMPLETE),
            status
                .conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True"),
        ));
    }

    status.last_probe_time = Some(Timestamp::now().to_string());

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
            let failure_message = match read_probe_result(&ctx.client, &namespace, &job).await {
                Ok(Some(result)) => result
                    .error
                    .as_ref()
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| {
                        "Repository probe Job failed; check endpoint, credentials and bucket access"
                            .to_string()
                    }),
                _ => "Repository probe Job failed; check endpoint, credentials and bucket access"
                    .to_string(),
            };

            job_api
                .delete(&job.name_any(), &Default::default())
                .await
                .ok();

            let ready_condition = Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                observed_generation: obj.metadata.generation,
                last_transition_time: Time(Timestamp::now()),
                reason: "ProbeFailed".to_string(),
                message: format!("Repository probe failed: {failure_message}"),
            };
            status.conditions.retain(|c| c.type_ != "Ready");
            status.conditions.push(ready_condition);
            status.capabilities = None;

            patch_repository_status(&ctx, &namespace, &name, &status).await?;
            return Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_PROBE_COMPLETE),
                false,
            ));
        }

        if job_complete {
            match read_probe_result(&ctx.client, &namespace, &job).await? {
                Some(result) if result.success && result.exit_code == ExitCode::Success => {
                    let capabilities = probe_capabilities(&result);
                    status.capabilities = capabilities;

                    let ready_condition = Condition {
                        type_: "Ready".to_string(),
                        status: "True".to_string(),
                        observed_generation: obj.metadata.generation,
                        last_transition_time: Time(Timestamp::now()),
                        reason: "Probed".to_string(),
                        message: "Repository probe completed successfully".to_string(),
                    };
                    status.conditions.retain(|c| c.type_ != "Ready");
                    status.conditions.push(ready_condition);

                    job_api
                        .delete(&job.name_any(), &Default::default())
                        .await
                        .ok();

                    patch_repository_status(&ctx, &namespace, &name, &status).await?;
                    return Ok((
                        kube::runtime::controller::Action::requeue(REQUEUE_PROBE_COMPLETE),
                        true,
                    ));
                }
                Some(result) => {
                    let error_msg = result
                        .error
                        .as_ref()
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "probe returned non-success exit code".to_string());

                    let ready_condition = Condition {
                        type_: "Ready".to_string(),
                        status: "False".to_string(),
                        observed_generation: obj.metadata.generation,
                        last_transition_time: Time(Timestamp::now()),
                        reason: "ProbeFailed".to_string(),
                        message: format!("Repository probe failed: {error_msg}"),
                    };
                    status.conditions.retain(|c| c.type_ != "Ready");
                    status.conditions.push(ready_condition);
                    status.capabilities = None;

                    job_api
                        .delete(&job.name_any(), &Default::default())
                        .await
                        .ok();

                    patch_repository_status(&ctx, &namespace, &name, &status).await?;
                    return Ok((
                        kube::runtime::controller::Action::requeue(REQUEUE_PROBE_COMPLETE),
                        false,
                    ));
                }
                None => {
                    debug!(
                        msg =
                            "probe Job completed but result document not yet readable; requeueing",
                        namespace, name,
                    );
                    patch_repository_status(&ctx, &namespace, &name, &status).await?;
                    return Ok((
                        kube::runtime::controller::Action::requeue(REQUEUE_PROBE_PENDING),
                        false,
                    ));
                }
            }
        }

        debug!(msg = "probe Job still running", namespace, name,);
        if !status
            .conditions
            .iter()
            .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        {
            let ready_condition = Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                observed_generation: obj.metadata.generation,
                last_transition_time: Time(Timestamp::now()),
                reason: "Probing".to_string(),
                message: "Repository probe Job is running".to_string(),
            };
            status.conditions.retain(|c| c.type_ != "Ready");
            status.conditions.push(ready_condition);
            patch_repository_status(&ctx, &namespace, &name, &status).await?;
        }
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_PROBE_PENDING),
            false,
        ));
    }

    let probe_job = build_probe_job(&obj, &namespace);
    match job_api.create(&Default::default(), &probe_job).await {
        Ok(_) => {
            info!(msg = "created repository probe Job", namespace, name,);
        }
        Err(kube::Error::Api(ae)) if ae.code == 409 => {
            debug!(msg = "probe Job already exists", namespace, name,);
        }
        Err(e) => {
            return Err(Error::KubeError(
                format!("failed to create probe Job for repository {namespace}/{name}"),
                Box::new(e),
            ));
        }
    }

    let ready_condition = Condition {
        type_: "Ready".to_string(),
        status: "False".to_string(),
        observed_generation: obj.metadata.generation,
        last_transition_time: Time(Timestamp::now()),
        reason: "Probing".to_string(),
        message: "Repository probe Job created".to_string(),
    };
    status.conditions.retain(|c| c.type_ != "Ready");
    status.conditions.push(ready_condition);
    patch_repository_status(&ctx, &namespace, &name, &status).await?;

    Ok((
        kube::runtime::controller::Action::requeue(REQUEUE_PROBE_PENDING),
        false,
    ))
}

async fn patch_repository_status(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackupRepository>,
    namespace: &str,
    name: &str,
    status: &crate::crd::KanidmBackupRepositoryStatus,
) -> Result<()> {
    let api: Api<KanidmBackupRepository> = Api::namespaced(ctx.client.clone(), namespace);
    let patch = serde_json::json!({
        "apiVersion": "kaniop.rs/v1alpha1",
        "kind": "KanidmBackupRepository",
        "status": status
    });
    api.patch_status(
        name,
        &PatchParams::apply(CONTROLLER_ID),
        &Patch::Apply(patch),
    )
    .await
    .map_err(|e| {
        Error::KubeError(
            format!("failed to patch status for {namespace}/{name}"),
            Box::new(e),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_backup_core::auth::CA_BUNDLE_VOLUME_NAME;
    use kaniop_backup_core::result::{ProbeResult, ResultDocument};

    #[test]
    fn probe_job_name_uses_prefix() {
        let name = probe_job_name("my-repo");
        assert!(name.starts_with(PROBE_JOB_PREFIX));
        assert!(name.contains("my-repo"));
    }

    #[test]
    fn probe_job_name_truncates_long_names() {
        let long_name = "x".repeat(50);
        let name = probe_job_name(&long_name);
        let suffix = &name[PROBE_JOB_PREFIX.len() + 1..];
        assert!(suffix.len() <= 20);
    }

    #[test]
    fn probe_job_name_does_not_end_with_hyphen() {
        let name = probe_job_name("test-remote-restore-rt-repo");
        assert!(!name.ends_with('-'));
    }

    #[test]
    fn probe_capabilities_extracts_from_result() {
        let mut result = ResultDocument::success("probe");
        result.probe = Some(ProbeResult {
            multipart_upload: true,
            conditional_put: true,
            head_object: true,
        });
        let caps = probe_capabilities(&result).unwrap();
        assert!(caps.multipart_upload);
        assert!(caps.conditional_put);
        assert!(!caps.object_lock);
    }

    #[test]
    fn probe_capabilities_returns_none_without_probe() {
        let result = ResultDocument::success("probe");
        assert!(probe_capabilities(&result).is_none());
    }

    #[test]
    fn probe_capabilities_handles_disabled_features() {
        let mut result = ResultDocument::success("probe");
        result.probe = Some(ProbeResult {
            multipart_upload: false,
            conditional_put: false,
            head_object: false,
        });
        let caps = probe_capabilities(&result).unwrap();
        assert!(!caps.multipart_upload);
        assert!(!caps.conditional_put);
        assert!(!caps.object_lock);
    }

    #[test]
    fn probe_job_injects_writer_auth_env_vars() {
        use kaniop_backup_core::crd::{
            AuthMethod, KanidmBackupRepositorySpec, RepositoryAuthentication, S3Config, SecretRef,
        };

        let repository = KanidmBackupRepository {
            metadata: kube::api::ObjectMeta {
                name: Some("test-repo".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "b".to_string(),
                    prefix: "p".to_string(),
                    region: Some("r".to_string()),
                    endpoint: Some("https://s3.example.com".to_string()),
                    force_path_style: false,
                    ca_bundle_ref: None,
                },
                authentication: RepositoryAuthentication {
                    writer: AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(SecretRef {
                            name: "writer-secret".to_string(),
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
            status: None,
        };

        let job = build_probe_job(&repository, "default");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let container = &pod_spec.containers[0];
        let env_vars = container.env.as_ref().unwrap();

        let aws_key = env_vars.iter().find(|e| e.name == "AWS_ACCESS_KEY_ID");
        assert!(
            aws_key.is_some(),
            "probe job should inject AWS_ACCESS_KEY_ID"
        );
        let key_ref = aws_key
            .unwrap()
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.name, "writer-secret");
        assert_eq!(key_ref.key, "AWS_ACCESS_KEY_ID");
    }

    #[test]
    fn probe_job_injects_ca_bundle_when_configured() {
        use kaniop_backup_core::crd::{
            AuthMethod, KanidmBackupRepositorySpec, RepositoryAuthentication, S3Config,
        };

        let repository = KanidmBackupRepository {
            metadata: kube::api::ObjectMeta {
                name: Some("test-repo".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "b".to_string(),
                    prefix: "p".to_string(),
                    region: Some("r".to_string()),
                    endpoint: Some("https://s3.example.com".to_string()),
                    force_path_style: false,
                    ca_bundle_ref: Some("my-ca-cm".to_string()),
                },
                authentication: RepositoryAuthentication {
                    writer: AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(kaniop_backup_core::crd::SecretRef {
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
            status: None,
        };

        let job = build_probe_job(&repository, "default");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();

        let has_ca_volume = pod_spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .any(|v| v.name == CA_BUNDLE_VOLUME_NAME);
        assert!(has_ca_volume, "probe job should mount CA bundle volume");

        let has_ca_mount = pod_spec.containers[0]
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .any(|m| m.name == CA_BUNDLE_VOLUME_NAME);
        assert!(
            has_ca_mount,
            "probe job should mount CA bundle in container"
        );
    }

    #[test]
    fn probe_job_workload_identity_omits_static_credentials() {
        use kaniop_backup_core::crd::{
            AuthMethod, KanidmBackupRepositorySpec, RepositoryAuthentication, S3Config,
            WorkloadIdentity,
        };

        let repository = KanidmBackupRepository {
            metadata: kube::api::ObjectMeta {
                name: Some("test-repo".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "b".to_string(),
                    prefix: "p".to_string(),
                    region: Some("r".to_string()),
                    endpoint: Some("https://s3.example.com".to_string()),
                    force_path_style: false,
                    ca_bundle_ref: None,
                },
                authentication: RepositoryAuthentication {
                    writer: AuthMethod {
                        workload_identity: Some(WorkloadIdentity { audience: None }),
                        secret_ref: None,
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
            status: None,
        };

        let job = build_probe_job(&repository, "default");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        assert_eq!(pod_spec.automount_service_account_token, Some(false));

        let container = &pod_spec.containers[0];
        let env_vars = container.env.as_ref().unwrap();
        let has_aws_key = env_vars.iter().any(|e| e.name == "AWS_ACCESS_KEY_ID");
        assert!(
            !has_aws_key,
            "workload identity should not inject static AWS credentials"
        );
    }
}
