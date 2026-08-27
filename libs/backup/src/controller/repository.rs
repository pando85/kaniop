use crate::crd::{AuthMethod, KanidmBackupRepository, KanidmBackupRepositorySpec};

use kaniop_backup_core::paths::RepositoryPath;
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{ControllerId, ResourceReflector, State, error_policy};

use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kaniop_k8s_util::error::{Error, Result};
use kube::api::{Patch, PatchParams};
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::reflector::{ObjectRef, Store};
use kube::runtime::watcher::Config;
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Api, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, info};

pub const CONTROLLER_ID: ControllerId = "backup-repository";

pub async fn run(
    state: State,
    client: Client,
    repository_api: Api<KanidmBackupRepository>,
    repository_r: ResourceReflector<KanidmBackupRepository>,
) {
    let ctx = Arc::new(state.to_context(client, CONTROLLER_ID));

    let secret_api: Api<Secret> = Api::all(ctx.client.clone());
    let configmap_api: Api<ConfigMap> = Api::all(ctx.client.clone());

    let store_for_secret = repository_r.store.clone();
    let store_for_cm = repository_r.store.clone();

    info!(msg = format!("starting {CONTROLLER_ID} controller"));

    let repository_watcher = watcher(repository_api, Config::default().any_semantic())
        .default_backoff()
        .reflect(repository_r.writer)
        .touched_objects();

    let repository_controller = Controller::for_stream(repository_watcher, repository_r.store)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .watches(
            secret_api,
            Config::default().any_semantic(),
            move |secret: Secret| {
                secret_name_to_repository_refs(
                    &store_for_secret,
                    &secret.name_any(),
                    secret.namespace().as_deref(),
                )
            },
        )
        .watches(
            configmap_api,
            Config::default().any_semantic(),
            move |cm: ConfigMap| {
                configmap_name_to_repository_refs(
                    &store_for_cm,
                    &cm.name_any(),
                    cm.namespace().as_deref(),
                )
            },
        )
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

fn secret_name_to_repository_refs(
    store: &Store<KanidmBackupRepository>,
    secret_name: &str,
    secret_namespace: Option<&str>,
) -> Vec<ObjectRef<KanidmBackupRepository>> {
    store
        .state()
        .iter()
        .filter(|repo| {
            let ns = repo.namespace();
            let repo_ns = ns.as_deref();
            repo_ns == secret_namespace && repository_references_secret(repo, secret_name)
        })
        .map(|repo| ObjectRef::from_obj(repo.as_ref()))
        .collect()
}

fn configmap_name_to_repository_refs(
    store: &Store<KanidmBackupRepository>,
    cm_name: &str,
    cm_namespace: Option<&str>,
) -> Vec<ObjectRef<KanidmBackupRepository>> {
    store
        .state()
        .iter()
        .filter(|repo| {
            let ns = repo.namespace();
            let repo_ns = ns.as_deref();
            repo_ns == cm_namespace
                && repo
                    .spec
                    .s3
                    .ca_bundle_ref
                    .as_deref()
                    .is_some_and(|r| r == cm_name)
        })
        .map(|repo| ObjectRef::from_obj(repo.as_ref()))
        .collect()
}

fn repository_references_secret(repo: &KanidmBackupRepository, secret_name: &str) -> bool {
    auth_method_references_secret(&repo.spec.authentication.writer, secret_name)
        || auth_method_references_secret(&repo.spec.authentication.reader, secret_name)
        || auth_method_references_secret(&repo.spec.authentication.deleter, secret_name)
}

fn auth_method_references_secret(method: &AuthMethod, secret_name: &str) -> bool {
    method
        .secret_ref
        .as_ref()
        .is_some_and(|sr| sr.name == secret_name)
}

fn validate_spec(spec: &KanidmBackupRepositorySpec) -> Option<String> {
    if spec.s3.bucket.is_empty() {
        return Some("bucket is required".to_string());
    }
    if spec.s3.prefix.contains("..") {
        return Some("prefix contains path traversal".to_string());
    }
    if let Some(endpoint) = &spec.s3.endpoint {
        if !endpoint.starts_with("https://") && !spec.s3.insecure {
            return Some("endpoint must use HTTPS".to_string());
        }
    }
    if let Err(e) = RepositoryPath::new(&spec.s3.bucket, &spec.s3.prefix) {
        return Some(format!("invalid repository path: {e}"));
    }
    None
}

async fn reconcile_repository(
    obj: Arc<KanidmBackupRepository>,
    ctx: Arc<kaniop_operator::controller::context::Context<KanidmBackupRepository>>,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    debug!(msg = "reconciling KanidmBackupRepository", %namespace, %name);

    let validation_error = validate_spec(&obj.spec);

    let (ready_status, reason, message) = match &validation_error {
        Some(err_msg) => ("False", "InvalidSpec", err_msg.clone()),
        None => (
            "True",
            "Accepted",
            "Repository configuration accepted".to_string(),
        ),
    };

    let mut status = obj.status.clone().unwrap_or_default();
    let existing_ready = status
        .conditions
        .iter()
        .find(|c| c.type_ == "Ready")
        .cloned();

    let condition_changed = match &existing_ready {
        Some(c) => c.status != ready_status || c.reason != reason || c.message != message,
        None => true,
    };
    let generation_changed = status.observed_generation != obj.metadata.generation;

    if condition_changed || generation_changed {
        let last_transition_time = if condition_changed {
            Time(Timestamp::now())
        } else {
            existing_ready
                .as_ref()
                .unwrap()
                .last_transition_time
                .clone()
        };

        let ready_condition = Condition {
            type_: "Ready".to_string(),
            status: ready_status.to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time,
            reason: reason.to_string(),
            message,
        };
        status.conditions.retain(|c| c.type_ != "Ready");
        status.conditions.push(ready_condition);
        patch_repository_status(&ctx, &namespace, &name, &status).await?;
    }

    Ok((
        kube::runtime::controller::Action::await_change(),
        validation_error.is_none(),
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
    use kaniop_backup_core::crd::{
        AuthMethod, KanidmBackupRepositorySpec, RepositoryAuthentication, S3Config, SecretRef,
    };
    use kube::runtime::reflector::store_shared;

    fn make_repo(
        name: &str,
        writer_secret: &str,
        ca_bundle: Option<&str>,
    ) -> KanidmBackupRepository {
        KanidmBackupRepository {
            metadata: kube::api::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "b".to_string(),
                    prefix: "p".to_string(),
                    region: None,
                    endpoint: None,
                    force_path_style: false,
                    insecure: false,
                    ca_bundle_ref: ca_bundle.map(String::from),
                },
                authentication: RepositoryAuthentication {
                    writer: AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(SecretRef {
                            name: writer_secret.to_string(),
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
        }
    }

    #[test]
    fn repository_references_secret_via_writer() {
        let repo = make_repo("r", "my-secret", None);
        assert!(repository_references_secret(&repo, "my-secret"));
        assert!(!repository_references_secret(&repo, "other"));
    }

    #[test]
    fn repository_references_secret_via_reader_and_deleter() {
        let mut repo = make_repo("r", "w", None);
        repo.spec.authentication.reader = AuthMethod {
            workload_identity: None,
            secret_ref: Some(SecretRef {
                name: "reader-s".to_string(),
            }),
        };
        repo.spec.authentication.deleter = AuthMethod {
            workload_identity: None,
            secret_ref: Some(SecretRef {
                name: "deleter-s".to_string(),
            }),
        };
        assert!(repository_references_secret(&repo, "reader-s"));
        assert!(repository_references_secret(&repo, "deleter-s"));
        assert!(!repository_references_secret(&repo, "nonexistent"));
    }

    #[test]
    fn repository_does_not_reference_secret_with_workload_identity() {
        let mut repo = make_repo("r", "w", None);
        repo.spec.authentication.writer = AuthMethod {
            workload_identity: Some(kaniop_backup_core::crd::WorkloadIdentity { audience: None }),
            secret_ref: None,
        };
        assert!(!repository_references_secret(&repo, "w"));
    }

    #[test]
    fn secret_mapper_returns_matching_repos() {
        let (store, mut writer) = store_shared::<KanidmBackupRepository>(16);
        let repo1 = make_repo("repo-1", "shared-secret", None);
        let repo2 = make_repo("repo-2", "other-secret", None);
        let repo3 = make_repo("repo-3", "shared-secret", Some("ca-cm"));
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo1.clone()));
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo2.clone()));
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo3.clone()));

        let refs = secret_name_to_repository_refs(&store, "shared-secret", Some("default"));
        let names: Vec<String> = refs.iter().map(|r| r.name.clone()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"repo-1".to_string()));
        assert!(names.contains(&"repo-3".to_string()));
    }

    #[test]
    fn secret_mapper_returns_empty_for_unknown() {
        let (store, mut writer) = store_shared::<KanidmBackupRepository>(16);
        let repo = make_repo("repo-1", "my-secret", None);
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo));

        let refs = secret_name_to_repository_refs(&store, "unknown-secret", Some("default"));
        assert!(refs.is_empty());
    }

    #[test]
    fn secret_mapper_filters_by_namespace() {
        let (store, mut writer) = store_shared::<KanidmBackupRepository>(16);
        let mut repo = make_repo("repo-1", "my-secret", None);
        repo.metadata.namespace = Some("other-ns".to_string());
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo));

        let refs = secret_name_to_repository_refs(&store, "my-secret", Some("default"));
        assert!(refs.is_empty());

        let refs = secret_name_to_repository_refs(&store, "my-secret", Some("other-ns"));
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn configmap_mapper_returns_matching_repos() {
        let (store, mut writer) = store_shared::<KanidmBackupRepository>(16);
        let repo1 = make_repo("repo-1", "w", Some("my-ca"));
        let repo2 = make_repo("repo-2", "w", None);
        let repo3 = make_repo("repo-3", "w", Some("my-ca"));
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo1));
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo2));
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(repo3));

        let refs = configmap_name_to_repository_refs(&store, "my-ca", Some("default"));
        assert_eq!(refs.len(), 2);

        let refs = configmap_name_to_repository_refs(&store, "other-cm", Some("default"));
        assert!(refs.is_empty());
    }

    #[test]
    fn validate_spec_accepts_valid_spec() {
        let spec = KanidmBackupRepositorySpec {
            s3: S3Config {
                bucket: "my-bucket".to_string(),
                prefix: "prod".to_string(),
                region: None,
                endpoint: Some("https://s3.example.com".to_string()),
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
        };
        assert!(validate_spec(&spec).is_none());
    }

    #[test]
    fn validate_spec_rejects_empty_bucket() {
        let spec = KanidmBackupRepositorySpec {
            s3: S3Config {
                bucket: "".to_string(),
                prefix: "p".to_string(),
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
        };
        assert!(validate_spec(&spec).is_some());
        assert!(validate_spec(&spec).unwrap().contains("bucket"));
    }

    #[test]
    fn validate_spec_rejects_path_traversal() {
        let spec = KanidmBackupRepositorySpec {
            s3: S3Config {
                bucket: "b".to_string(),
                prefix: "foo/../bar".to_string(),
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
        };
        assert!(validate_spec(&spec).is_some());
        assert!(validate_spec(&spec).unwrap().contains("path traversal"));
    }

    #[test]
    fn validate_spec_rejects_http_endpoint_without_insecure() {
        let spec = KanidmBackupRepositorySpec {
            s3: S3Config {
                bucket: "b".to_string(),
                prefix: "p".to_string(),
                region: None,
                endpoint: Some("http://s3.example.com".to_string()),
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
        };
        assert!(validate_spec(&spec).is_some());
        assert!(validate_spec(&spec).unwrap().contains("HTTPS"));
    }

    #[test]
    fn validate_spec_accepts_http_endpoint_with_insecure() {
        let spec = KanidmBackupRepositorySpec {
            s3: S3Config {
                bucket: "b".to_string(),
                prefix: "p".to_string(),
                region: None,
                endpoint: Some("http://localhost:9000".to_string()),
                force_path_style: false,
                insecure: true,
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
        };
        assert!(validate_spec(&spec).is_none());
    }

    #[test]
    fn transition_time_preserved_when_condition_unchanged() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
        use k8s_openapi::jiff::Timestamp;

        let old_time = Time(Timestamp::new(1704067200, 0).unwrap());
        let existing_condition = Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            reason: "Accepted".to_string(),
            message: "Repository configuration accepted".to_string(),
            last_transition_time: old_time.clone(),
            observed_generation: Some(1),
        };

        let mut status = crate::crd::KanidmBackupRepositoryStatus {
            observed_generation: Some(1),
            conditions: vec![existing_condition.clone()],
        };

        let ready_status = "True";
        let reason = "Accepted";
        let message = "Repository configuration accepted".to_string();

        let existing_ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .cloned();
        let condition_changed = match &existing_ready {
            Some(c) => c.status != ready_status || c.reason != reason || c.message != message,
            None => true,
        };

        assert!(!condition_changed);

        let generation_changed = status.observed_generation != Some(2);
        assert!(generation_changed);

        if condition_changed || generation_changed {
            let last_transition_time = if condition_changed {
                Time(Timestamp::now())
            } else {
                existing_ready
                    .as_ref()
                    .unwrap()
                    .last_transition_time
                    .clone()
            };
            let ready_condition = Condition {
                type_: "Ready".to_string(),
                status: ready_status.to_string(),
                observed_generation: Some(2),
                last_transition_time: last_transition_time.clone(),
                reason: reason.to_string(),
                message,
            };
            status.conditions.retain(|c| c.type_ != "Ready");
            status.conditions.push(ready_condition);
        }

        let ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .unwrap();
        assert_eq!(ready.last_transition_time, old_time);
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Accepted");
    }

    #[test]
    fn transition_time_updates_when_status_changes() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
        use k8s_openapi::jiff::Timestamp;

        let old_time = Time(Timestamp::new(1704067200, 0).unwrap());
        let existing_condition = Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            reason: "InvalidSpec".to_string(),
            message: "bucket is required".to_string(),
            last_transition_time: old_time,
            observed_generation: Some(1),
        };

        let mut status = crate::crd::KanidmBackupRepositoryStatus {
            observed_generation: Some(1),
            conditions: vec![existing_condition],
        };

        let ready_status = "True";
        let reason = "Accepted";
        let message = "Repository configuration accepted".to_string();

        let existing_ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .cloned();
        let condition_changed = match &existing_ready {
            Some(c) => c.status != ready_status || c.reason != reason || c.message != message,
            None => true,
        };

        assert!(condition_changed);

        let last_transition_time = if condition_changed {
            Time(Timestamp::now())
        } else {
            existing_ready
                .as_ref()
                .unwrap()
                .last_transition_time
                .clone()
        };
        let ready_condition = Condition {
            type_: "Ready".to_string(),
            status: ready_status.to_string(),
            observed_generation: Some(2),
            last_transition_time,
            reason: reason.to_string(),
            message,
        };
        status.conditions.retain(|c| c.type_ != "Ready");
        status.conditions.push(ready_condition);

        let ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .unwrap();
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Accepted");
        assert_ne!(
            ready.last_transition_time,
            Time(Timestamp::new(1704067200, 0).unwrap())
        );
    }

    #[test]
    fn invalid_spec_sets_ready_false() {
        let spec = KanidmBackupRepositorySpec {
            s3: S3Config {
                bucket: "".to_string(),
                prefix: "p".to_string(),
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
        };

        let validation_error = validate_spec(&spec);
        assert!(validation_error.is_some());

        let (ready_status, reason, _message) = match &validation_error {
            Some(err_msg) => ("False", "InvalidSpec", err_msg.clone()),
            None => (
                "True",
                "Accepted",
                "Repository configuration accepted".to_string(),
            ),
        };

        assert_eq!(ready_status, "False");
        assert_eq!(reason, "InvalidSpec");
    }

    #[test]
    fn no_patch_when_condition_and_generation_unchanged() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
        use k8s_openapi::jiff::Timestamp;

        let existing_condition = Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            reason: "Accepted".to_string(),
            message: "Repository configuration accepted".to_string(),
            last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                Timestamp::new(1704067200, 0).unwrap(),
            ),
            observed_generation: Some(5),
        };

        let status = crate::crd::KanidmBackupRepositoryStatus {
            observed_generation: Some(5),
            conditions: vec![existing_condition],
        };

        let ready_status = "True";
        let reason = "Accepted";
        let message = "Repository configuration accepted".to_string();

        let existing_ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .cloned();
        let condition_changed = match &existing_ready {
            Some(c) => c.status != ready_status || c.reason != reason || c.message != message,
            None => true,
        };
        let generation_changed = status.observed_generation != Some(5);

        assert!(!condition_changed);
        assert!(!generation_changed);
    }
}
