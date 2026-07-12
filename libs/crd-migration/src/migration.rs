use std::collections::HashMap;
use std::env;
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{DynamicObject, Patch, PatchParams};
use kube::{Api, ResourceExt};
use serde_json::Value;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    API_GROUP, API_VERSION, CORRECTED_CRD_NAME, DEFAULT_MARKER_NAME, KIND, LEGACY_CRD_NAME,
    LEGACY_FINALIZER, LEGACY_PLURAL, MigrationError, Result,
    backup::{create_or_validate_backup, list_backup_entries, mark_backup_restored},
    checksum::{object_checksum, source_set_checksum},
    corrected_person_api_resource,
    crd::{
        delete_crd, extract_corrected_person_crd, get_crd, is_crd_ready,
        validate_corrected_crd_schema,
    },
    sanitize::{
        compute_finalizer_patch, extract_metadata_spec, sanitize_for_backup, sanitize_for_restore,
    },
    state::{
        Action, MigrationMarker, Phase, create_or_update_marker, detect_crd_state,
        determine_action, get_marker,
    },
    verify::{
        AdoptionVerificationResult, adoption_verification_passed, structural_verification_passed,
        verify_adoption, verify_structural,
    },
};

const FAIL_AFTER_ENV: &str = "KANIOP_MIGRATION_FAIL_AFTER";
const DEFAULT_OPERATOR_DEPLOYMENT: &str = "kaniop";
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const POLL_INTERVAL_SECS: u64 = 2;
const FINALIZER_PATCH_RETRIES: usize = 5;

pub struct MigrationConfig {
    pub namespace: String,
    pub operator_namespace: String,
    pub operator_deployment: String,
    pub marker_name: String,
    pub timeout: Duration,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            namespace: "kaniop".to_string(),
            operator_namespace: "kaniop".to_string(),
            operator_deployment: DEFAULT_OPERATOR_DEPLOYMENT.to_string(),
            marker_name: DEFAULT_MARKER_NAME.to_string(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }
}

fn check_fail_injection(phase: Phase) -> Result<()> {
    let fail_after = env::var(FAIL_AFTER_ENV).ok();
    check_fail_injection_value(phase, fail_after.as_deref())
}

fn check_fail_injection_value(phase: Phase, fail_after: Option<&str>) -> Result<()> {
    if fail_after.is_some_and(|value| value == phase.to_string()) {
        return Err(MigrationError::InjectedFailure(phase.to_string()));
    }
    Ok(())
}

fn person_api_resource() -> kube::api::ApiResource {
    kube::api::ApiResource {
        group: API_GROUP.to_string(),
        version: API_VERSION.to_string(),
        api_version: format!("{API_GROUP}/{API_VERSION}"),
        kind: KIND.to_string(),
        plural: LEGACY_PLURAL.to_string(),
    }
}

fn legacy_namespaced_api(client: &kube::Client, namespace: &str) -> Api<DynamicObject> {
    Api::namespaced_with(client.clone(), namespace, &person_api_resource())
}

fn corrected_namespaced_api(client: &kube::Client, namespace: &str) -> Api<DynamicObject> {
    Api::namespaced_with(client.clone(), namespace, &corrected_person_api_resource())
}

fn legacy_all_api(client: &kube::Client) -> Api<DynamicObject> {
    Api::all_with(client.clone(), &person_api_resource())
}

pub async fn run_presync(client: &kube::Client, config: &MigrationConfig) -> Result<()> {
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    let marker_api: Api<ConfigMap> = Api::namespaced(client.clone(), &config.namespace);

    let legacy_crd = get_crd(&crd_api, LEGACY_CRD_NAME).await?;
    let corrected_crd = get_crd(&crd_api, CORRECTED_CRD_NAME).await?;

    let crd_state = detect_crd_state(legacy_crd.is_some(), corrected_crd.is_some());
    let marker = get_marker(&marker_api, &config.marker_name).await?;
    let action = determine_action(crd_state, &marker)?;

    info!(?action, ?crd_state, "PreSync: migration state detected");

    match action {
        Action::FreshInstall => {
            info!("fresh install, creating corrected CRD");
            let corrected = extract_corrected_person_crd()?;
            create_or_validate_crd(&crd_api, &corrected).await?;
            wait_for_crd_established(&crd_api, CORRECTED_CRD_NAME, config.timeout).await?;
            info!("corrected CRD created and established");

            let now = utc_now();
            let mut new_marker = MigrationMarker::new(&now);
            new_marker.phase = Phase::Verified;
            new_marker.source_count = 0;
            new_marker.restored_count = 0;
            new_marker.source_set_checksum = crate::checksum::empty_source_set_checksum();
            create_or_update_marker(
                &marker_api,
                &new_marker,
                &config.marker_name,
                &config.namespace,
            )
            .await?;
            Ok(())
        }
        Action::StartNew => {
            let now = utc_now();
            let mut new_marker = MigrationMarker::new(&now);
            create_or_update_marker(
                &marker_api,
                &new_marker,
                &config.marker_name,
                &config.namespace,
            )
            .await?;
            run_presync_from_phase(client, config, Phase::Discovered, &mut new_marker).await
        }
        Action::Resume(phase) => {
            let mut marker = marker.unwrap();
            info!(?phase, "resuming PreSync migration");
            run_presync_from_phase(client, config, phase, &mut marker).await
        }
        Action::PostSyncVerify => {
            info!("PreSync: marker already at Verified or later, validating corrected CRD");
            let corrected_crd = corrected_crd.ok_or_else(|| {
                MigrationError::State(
                    "marker indicates migration completed but corrected CRD is missing".to_string(),
                )
            })?;

            validate_corrected_crd_schema(&corrected_crd)?;

            if !is_crd_ready(&corrected_crd) {
                return Err(MigrationError::State(
                    "corrected CRD exists but is not ready (Established=True and NamesAccepted=True)".to_string(),
                ));
            }

            info!("PreSync: corrected CRD validated and ready");
            Ok(())
        }
    }
}

async fn run_presync_from_phase(
    client: &kube::Client,
    config: &MigrationConfig,
    start_phase: Phase,
    marker: &mut MigrationMarker,
) -> Result<()> {
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    let marker_api: Api<ConfigMap> = Api::namespaced(client.clone(), &config.namespace);
    let backup_api: Api<Secret> = Api::namespaced(client.clone(), &config.namespace);
    let deployment_api: Api<Deployment> =
        Api::namespaced(client.clone(), &config.operator_namespace);

    if start_phase <= Phase::Discovered {
        info!(phase = "Discovered", "backing up legacy resources");
        let legacy_all = legacy_all_api(client);
        backup_legacy_resources(&legacy_all, &backup_api, &config.namespace, marker).await?;
        marker.phase = Phase::BackedUp;
        marker.updated_at = utc_now();
        create_or_update_marker(&marker_api, marker, &config.marker_name, &config.namespace)
            .await?;
        check_fail_injection(Phase::BackedUp)?;
    }

    if start_phase <= Phase::BackedUp {
        info!(phase = "OperatorStopped", "stopping operator");
        ensure_operator_stopped(
            &deployment_api,
            &config.operator_deployment,
            config.timeout,
            marker,
        )
        .await?;
        marker.phase = Phase::OperatorStopped;
        marker.updated_at = utc_now();
        create_or_update_marker(&marker_api, marker, &config.marker_name, &config.namespace)
            .await?;
        check_fail_injection(Phase::OperatorStopped)?;
    }

    if start_phase <= Phase::OperatorStopped {
        ensure_operator_still_stopped(&deployment_api, &config.operator_deployment).await?;
        info!(phase = "FinalizersRemoved", "removing finalizers");
        let backup_entries = list_backup_entries(&backup_api).await?;
        remove_finalizers(client, &backup_entries).await?;
        marker.phase = Phase::FinalizersRemoved;
        marker.updated_at = utc_now();
        create_or_update_marker(&marker_api, marker, &config.marker_name, &config.namespace)
            .await?;
        check_fail_injection(Phase::FinalizersRemoved)?;
    }

    if start_phase <= Phase::FinalizersRemoved {
        ensure_operator_still_stopped(&deployment_api, &config.operator_deployment).await?;
        info!(phase = "LegacyCRDDeleted", "deleting legacy CRD");
        delete_crd(&crd_api, LEGACY_CRD_NAME).await?;
        wait_for_crd_gone(&crd_api, LEGACY_CRD_NAME, config.timeout).await?;
        marker.phase = Phase::LegacyCRDDeleted;
        marker.updated_at = utc_now();
        create_or_update_marker(&marker_api, marker, &config.marker_name, &config.namespace)
            .await?;
        check_fail_injection(Phase::LegacyCRDDeleted)?;
    }

    if start_phase <= Phase::LegacyCRDDeleted {
        info!(phase = "CorrectedCRDCreated", "creating corrected CRD");
        let corrected = extract_corrected_person_crd()?;
        create_or_validate_crd(&crd_api, &corrected).await?;
        wait_for_crd_established(&crd_api, CORRECTED_CRD_NAME, config.timeout).await?;
        marker.phase = Phase::CorrectedCRDCreated;
        marker.updated_at = utc_now();
        create_or_update_marker(&marker_api, marker, &config.marker_name, &config.namespace)
            .await?;
        check_fail_injection(Phase::CorrectedCRDCreated)?;
    }

    if start_phase <= Phase::CorrectedCRDCreated {
        info!(phase = "ObjectsRestored", "restoring objects");
        let backup_entries = list_backup_entries(&backup_api).await?;
        restore_objects(client, &backup_api, &backup_entries, marker).await?;
        marker.phase = Phase::ObjectsRestored;
        marker.updated_at = utc_now();
        create_or_update_marker(&marker_api, marker, &config.marker_name, &config.namespace)
            .await?;
        check_fail_injection(Phase::ObjectsRestored)?;
    }

    if start_phase <= Phase::ObjectsRestored {
        info!(phase = "Verified", "running structural verification");
        let result = verify_structural(client, &backup_api, marker).await?;

        if !structural_verification_passed(&result) {
            return Err(MigrationError::Verify(format!(
                "structural verification failed: {} missing restorations, {} checksum mismatches",
                result.missing_restorations.len(),
                result.checksum_mismatches.len(),
            )));
        }

        marker.phase = Phase::Verified;
        marker.updated_at = utc_now();
        create_or_update_marker(&marker_api, marker, &config.marker_name, &config.namespace)
            .await?;
    }

    info!("PreSync completed successfully; marker at Verified; backups retained for PostSync");
    Ok(())
}

pub async fn run_postsync(
    client: &kube::Client,
    config: &MigrationConfig,
) -> Result<AdoptionVerificationResult> {
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    let marker_api: Api<ConfigMap> = Api::namespaced(client.clone(), &config.namespace);
    let backup_api: Api<Secret> = Api::namespaced(client.clone(), &config.namespace);

    let marker = get_marker(&marker_api, &config.marker_name).await?;

    let legacy_crd = get_crd(&crd_api, LEGACY_CRD_NAME).await?;
    let corrected_crd = get_crd(&crd_api, CORRECTED_CRD_NAME).await?;
    let crd_state = detect_crd_state(legacy_crd.is_some(), corrected_crd.is_some());

    info!(?crd_state, "PostSync: migration state detected");

    if crd_state == crate::state::CrdState::BothAbsent && marker.is_none() {
        info!("PostSync: no marker and both CRDs absent; fresh install with no migration needed");
        return Ok(AdoptionVerificationResult {
            corrected_crd_established: corrected_crd.as_ref().is_some_and(is_crd_ready),
            legacy_crd_absent: true,
            source_count: 0,
            restored_count: 0,
            checksum_matches: 0,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
            missing_finalizers: vec![],
            missing_exists: vec![],
        });
    }

    if marker.is_none() && legacy_crd.is_none() {
        if let Some(ref crd) = corrected_crd {
            validate_corrected_crd_schema(crd)?;
        }
        info!("PostSync: corrected CRD is already present and no migration marker is needed");
        return Ok(AdoptionVerificationResult {
            corrected_crd_established: corrected_crd.as_ref().is_some_and(is_crd_ready),
            legacy_crd_absent: true,
            source_count: 0,
            restored_count: 0,
            checksum_matches: 0,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
            missing_finalizers: vec![],
            missing_exists: vec![],
        });
    }

    if let Some(ref m) = marker {
        if m.phase >= Phase::Completed {
            info!("PostSync: marker already at Completed; verifying and cleaning up backups");
            if list_backup_entries(&backup_api).await?.is_empty() {
                if let Some(ref crd) = corrected_crd {
                    validate_corrected_crd_schema(crd)?;
                }
                return Ok(AdoptionVerificationResult {
                    corrected_crd_established: corrected_crd.as_ref().is_some_and(is_crd_ready),
                    legacy_crd_absent: legacy_crd.is_none(),
                    source_count: m.source_count,
                    restored_count: m.source_count,
                    checksum_matches: m.source_count,
                    checksum_mismatches: vec![],
                    missing_restorations: vec![],
                    missing_finalizers: vec![],
                    missing_exists: vec![],
                });
            }
            let result = verify_adoption(client, &backup_api, m, Duration::from_secs(1)).await?;
            if adoption_verification_passed(&result) {
                info!("PostSync: adoption verified; deleting any remaining backups");
                crate::backup::delete_all_backups(&backup_api).await?;
            }
            return Ok(result);
        }
    }

    let marker_ref = marker.as_ref().ok_or_else(|| {
        MigrationError::State(
            "PostSync requires a marker but none found; cannot proceed with adoption verification"
                .to_string(),
        )
    })?;

    info!("PostSync: waiting for operator adoption (finalizer + Exists condition)");
    let result = verify_adoption(client, &backup_api, marker_ref, config.timeout).await?;

    if !adoption_verification_passed(&result) {
        return Err(MigrationError::Verify(format!(
            "adoption verification failed: {} missing restorations, {} checksum mismatches, \
             {} missing finalizers, {} missing exists conditions",
            result.missing_restorations.len(),
            result.checksum_mismatches.len(),
            result.missing_finalizers.len(),
            result.missing_exists.len(),
        )));
    }

    if let Some(mut m) = marker {
        m.phase = Phase::Completed;
        m.updated_at = utc_now();
        create_or_update_marker(&marker_api, &m, &config.marker_name, &config.namespace).await?;
    }

    info!("PostSync: adoption verified; deleting backups");
    crate::backup::delete_all_backups(&backup_api).await?;

    info!("PostSync completed successfully");
    Ok(result)
}

async fn ensure_operator_stopped(
    deployment_api: &Api<Deployment>,
    name: &str,
    timeout: Duration,
    marker: &mut MigrationMarker,
) -> Result<()> {
    let replicas = deployment_api
        .get(name)
        .await
        .map_err(|e| MigrationError::Kube(format!("get deployment {name}"), Box::new(e)))?;

    let status = replicas.status.as_ref();
    let current_replicas = status.and_then(|s| s.replicas).unwrap_or(0);
    let current_ready = status.and_then(|s| s.ready_replicas).unwrap_or(0);
    let current_available = status.and_then(|s| s.available_replicas).unwrap_or(0);

    if current_replicas == 0 && current_ready == 0 && current_available == 0 {
        info!("operator already scaled to zero");
        return Ok(());
    }

    let original_replicas = replicas.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
    marker.original_replicas = Some(original_replicas);

    scale_deployment_to_zero(deployment_api, name, timeout).await
}

async fn ensure_operator_still_stopped(deployment_api: &Api<Deployment>, name: &str) -> Result<()> {
    let deployment = deployment_api
        .get(name)
        .await
        .map_err(|e| MigrationError::Kube(format!("get deployment {name}"), Box::new(e)))?;

    let status = deployment.status.as_ref();
    let replicas = status.and_then(|s| s.replicas).unwrap_or(0);
    let ready = status.and_then(|s| s.ready_replicas).unwrap_or(0);
    let available = status.and_then(|s| s.available_replicas).unwrap_or(0);

    if replicas != 0 || ready != 0 || available != 0 {
        return Err(MigrationError::State(format!(
            "operator deployment {name} has active replicas ({replicas} replicas, \
             {ready} ready, {available} available); refusing to proceed with destructive \
             operations while operator is running"
        )));
    }

    Ok(())
}

async fn backup_legacy_resources(
    legacy_api: &Api<DynamicObject>,
    backup_api: &Api<Secret>,
    backup_namespace: &str,
    marker: &mut MigrationMarker,
) -> Result<()> {
    let list = legacy_api
        .list(&Default::default())
        .await
        .map_err(|e| MigrationError::Kube("list legacy persons".to_string(), Box::new(e)))?;

    let mut checksum_entries = Vec::new();
    let mut resource_versions: HashMap<String, String> = HashMap::new();
    let mut seen = std::collections::HashSet::new();

    for obj in &list.items {
        let ns = obj.namespace().unwrap_or_default();
        let name = obj.name_unchecked();
        let key = format!("{ns}/{name}");

        if !seen.insert(key.clone()) {
            return Err(MigrationError::DuplicateSource(key));
        }

        if let Some(rv) = obj.metadata.resource_version.as_ref() {
            resource_versions.insert(key, rv.clone());
        }

        let value: Value = serde_json::to_value(obj)
            .map_err(|e| MigrationError::Serialization(format!("serialize {ns}/{name}"), e))?;

        let sanitized = sanitize_for_backup(&value)?;
        let checksum = object_checksum(&sanitized);

        create_or_validate_backup(backup_api, &sanitized, &ns, &name, backup_namespace).await?;

        checksum_entries.push((ns, name, checksum));
    }

    marker.source_count = checksum_entries.len();
    marker.source_set_checksum = source_set_checksum(
        &checksum_entries
            .iter()
            .map(|(ns, name, cs)| (ns.clone(), name.clone(), cs.clone()))
            .collect::<Vec<_>>(),
    );

    let recheck = legacy_api
        .list(&Default::default())
        .await
        .map_err(|e| MigrationError::Kube("re-list legacy persons".to_string(), Box::new(e)))?;

    let mut recheck_keys = std::collections::HashSet::new();
    for obj in &recheck.items {
        let ns = obj.namespace().unwrap_or_default();
        let name = obj.name_unchecked();
        let key = format!("{ns}/{name}");

        if !recheck_keys.insert(key.clone()) {
            return Err(MigrationError::DuplicateSource(key));
        }

        let expected_rv = resource_versions.get(&key).ok_or_else(|| {
            MigrationError::Backup(format!(
                "source set changed during backup: {key} present in re-list but not in initial list"
            ))
        })?;

        let actual_rv = obj.metadata.resource_version.as_ref().ok_or_else(|| {
            MigrationError::Backup(format!(
                "source object {key} missing resourceVersion in re-list"
            ))
        })?;

        if expected_rv != actual_rv {
            return Err(MigrationError::Backup(format!(
                "resourceVersion changed for {key} during backup: \
                 was {expected_rv}, now {actual_rv}"
            )));
        }
    }

    let initial_keys: std::collections::HashSet<_> = resource_versions.keys().cloned().collect();
    if initial_keys != recheck_keys {
        let added: Vec<_> = recheck_keys.difference(&initial_keys).collect();
        let removed: Vec<_> = initial_keys.difference(&recheck_keys).collect();
        return Err(MigrationError::Backup(format!(
            "source set changed during backup: added {:?}, removed {:?}",
            added, removed
        )));
    }

    Ok(())
}

async fn scale_deployment_to_zero(
    api: &Api<Deployment>,
    name: &str,
    timeout: Duration,
) -> Result<()> {
    let patch = serde_json::json!({
        "spec": {
            "replicas": 0
        }
    });

    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(|e| MigrationError::Kube(format!("scale deployment {name} to 0"), Box::new(e)))?;

    let start = std::time::Instant::now();
    loop {
        let deployment = api
            .get(name)
            .await
            .map_err(|e| MigrationError::Kube(format!("get deployment {name}"), Box::new(e)))?;

        let status = deployment.status.as_ref();
        let replicas = status.and_then(|s| s.replicas).unwrap_or(0);
        let ready = status.and_then(|s| s.ready_replicas).unwrap_or(0);
        let available = status.and_then(|s| s.available_replicas).unwrap_or(0);

        if replicas == 0 && ready == 0 && available == 0 {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(MigrationError::Timeout(format!(
                "deployment {name} did not scale to zero within timeout"
            )));
        }

        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

async fn remove_finalizers(
    client: &kube::Client,
    backups: &[crate::backup::BackupEntry],
) -> Result<()> {
    for entry in backups {
        remove_finalizer(client, entry).await?;
    }

    Ok(())
}

async fn remove_finalizer(client: &kube::Client, entry: &crate::backup::BackupEntry) -> Result<()> {
    let ns_api = legacy_namespaced_api(client, &entry.namespace);

    for attempt in 0..FINALIZER_PATCH_RETRIES {
        let obj = ns_api.get(&entry.name).await.map_err(|e| {
            MigrationError::Kube(
                format!("get legacy object {}/{}", entry.namespace, entry.name),
                Box::new(e),
            )
        })?;

        let current_finalizers = obj.metadata.finalizers.clone().unwrap_or_default();
        let resource_version = obj.metadata.resource_version.as_deref().unwrap_or("");
        let Some(new_finalizers) =
            compute_finalizer_patch(&current_finalizers, &entry.namespace, &entry.name)?
        else {
            info!(
                ns = %entry.namespace,
                name = %entry.name,
                "no legacy finalizer to remove"
            );
            return Ok(());
        };

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": new_finalizers,
                "resourceVersion": resource_version
            }
        });

        match ns_api
            .patch(&entry.name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            Ok(_) => {
                let verify = ns_api.get(&entry.name).await.map_err(|e| {
                    MigrationError::Kube(
                        format!(
                            "verify finalizer removal {}/{}",
                            entry.namespace, entry.name
                        ),
                        Box::new(e),
                    )
                })?;
                let remaining = verify.metadata.finalizers.unwrap_or_default();
                if remaining.iter().any(|f| f == LEGACY_FINALIZER) {
                    return Err(MigrationError::State(format!(
                        "legacy finalizer still present on {}/{} after patch",
                        entry.namespace, entry.name
                    )));
                }
                return Ok(());
            }
            Err(kube::Error::Api(ae))
                if ae.code == 409 && attempt + 1 < FINALIZER_PATCH_RETRIES =>
            {
                warn!(
                    ns = %entry.namespace,
                    name = %entry.name,
                    attempt = attempt + 1,
                    "conflict removing finalizer; retrying"
                );
                sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
            Err(kube::Error::Api(ae)) if ae.code == 409 => {
                return Err(MigrationError::Conflict(format!(
                    "exhausted {FINALIZER_PATCH_RETRIES} retries removing finalizer from {}/{}",
                    entry.namespace, entry.name
                )));
            }
            Err(error) => {
                return Err(MigrationError::Kube(
                    format!("patch finalizers {}/{}", entry.namespace, entry.name),
                    Box::new(error),
                ));
            }
        }
    }

    unreachable!("finalizer retry loop always returns on its last attempt")
}

async fn restore_objects(
    client: &kube::Client,
    backup_api: &Api<Secret>,
    backups: &[crate::backup::BackupEntry],
    marker: &mut MigrationMarker,
) -> Result<()> {
    let mut restored = 0;

    for entry in backups {
        let backup_obj = get_backup_object_value(backup_api, &entry.secret_name).await?;
        let restore_obj = sanitize_for_restore(&backup_obj)?;

        let ns = entry.namespace.clone();
        let name = entry.name.clone();
        let ns_api = corrected_namespaced_api(client, &ns);

        let dynamic_obj: DynamicObject =
            serde_json::from_value(restore_obj.clone()).map_err(|e| {
                MigrationError::Serialization(format!("deserialize restore object {ns}/{name}"), e)
            })?;

        if entry.restored {
            info!(
                ns = %ns,
                name = %name,
                "backup marked as restored; verifying object still exists and matches"
            );

            match ns_api.get(&name).await {
                Ok(existing) => {
                    let existing_value: Value = serde_json::to_value(&existing).map_err(|e| {
                        MigrationError::Serialization(
                            format!("serialize existing object {ns}/{name}"),
                            e,
                        )
                    })?;

                    if !metadata_spec_matches(&restore_obj, &existing_value) {
                        return Err(MigrationError::Conflict(format!(
                            "existing object {ns}/{name} does not match backup; \
                             manual intervention required"
                        )));
                    }

                    restored += 1;
                }
                Err(kube::Error::Api(ae)) if ae.code == 404 => {
                    warn!(
                        ns = %ns,
                        name = %name,
                        "object marked as restored but not found; recreating"
                    );

                    match ns_api.create(&Default::default(), &dynamic_obj).await {
                        Ok(_) => {
                            restored += 1;
                        }
                        Err(kube::Error::Api(ae)) if ae.code == 409 => {
                            let existing = ns_api.get(&name).await.map_err(|e| {
                                MigrationError::Kube(
                                    format!("get existing object {ns}/{name} after 409"),
                                    Box::new(e),
                                )
                            })?;

                            let existing_value: Value =
                                serde_json::to_value(&existing).map_err(|e| {
                                    MigrationError::Serialization(
                                        format!("serialize existing object {ns}/{name}"),
                                        e,
                                    )
                                })?;

                            if !metadata_spec_matches(&restore_obj, &existing_value) {
                                return Err(MigrationError::Conflict(format!(
                                    "existing object {ns}/{name} does not match backup; \
                                     manual intervention required"
                                )));
                            }

                            restored += 1;
                        }
                        Err(e) => {
                            return Err(MigrationError::Kube(
                                format!("recreate restored object {ns}/{name}"),
                                Box::new(e),
                            ));
                        }
                    }
                }
                Err(e) => {
                    return Err(MigrationError::Kube(
                        format!("get existing object {ns}/{name}"),
                        Box::new(e),
                    ));
                }
            }
        } else {
            match ns_api.create(&Default::default(), &dynamic_obj).await {
                Ok(_) => {
                    mark_backup_restored(backup_api, &entry.secret_name).await?;
                    restored += 1;
                }
                Err(kube::Error::Api(ae)) if ae.code == 409 => {
                    warn!(
                        ns = %ns,
                        name = %name,
                        "object already exists on corrected endpoint; comparing with backup"
                    );

                    let existing = ns_api.get(&name).await.map_err(|e| {
                        MigrationError::Kube(
                            format!("get existing object {ns}/{name} after 409"),
                            Box::new(e),
                        )
                    })?;

                    let existing_value: Value = serde_json::to_value(&existing).map_err(|e| {
                        MigrationError::Serialization(
                            format!("serialize existing object {ns}/{name}"),
                            e,
                        )
                    })?;

                    if !metadata_spec_matches(&restore_obj, &existing_value) {
                        return Err(MigrationError::Conflict(format!(
                            "existing object {ns}/{name} does not match backup; \
                             manual intervention required"
                        )));
                    }

                    info!(
                        ns = %ns,
                        name = %name,
                        "existing object matches backup; marking as restored"
                    );
                    mark_backup_restored(backup_api, &entry.secret_name).await?;
                    restored += 1;
                }
                Err(e) => {
                    return Err(MigrationError::Kube(
                        format!("create restored object {ns}/{name}"),
                        Box::new(e),
                    ));
                }
            }
        }
    }

    marker.restored_count = restored;
    Ok(())
}

fn metadata_spec_matches(desired: &Value, existing: &Value) -> bool {
    let desired_meta_spec = extract_metadata_spec(desired);
    let existing_meta_spec = extract_metadata_spec(existing);

    match (desired_meta_spec, existing_meta_spec) {
        (Some(d), Some(e)) => d == e,
        _ => false,
    }
}

async fn get_backup_object_value(api: &Api<Secret>, secret_name: &str) -> Result<Value> {
    let secret = api
        .get(secret_name)
        .await
        .map_err(|e| MigrationError::Kube(format!("get backup {secret_name}"), Box::new(e)))?;

    let obj_json = secret
        .string_data
        .as_ref()
        .and_then(|d| d.get("object.json"))
        .cloned()
        .or_else(|| {
            secret
                .data
                .as_ref()
                .and_then(|d| d.get("object.json"))
                .map(|v| String::from_utf8_lossy(&v.0).to_string())
        })
        .ok_or_else(|| {
            MigrationError::Backup(format!("backup {secret_name} missing object.json"))
        })?;

    serde_json::from_str(&obj_json)
        .map_err(|e| MigrationError::Serialization(format!("parse backup {secret_name}"), e))
}

async fn create_or_validate_crd(
    api: &Api<CustomResourceDefinition>,
    crd: &CustomResourceDefinition,
) -> Result<()> {
    let crd_name = crd.metadata.name.as_deref().unwrap_or("unknown");

    match api.create(&Default::default(), crd).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ae)) if ae.code == 409 => {
            info!(
                crd = crd_name,
                "CRD already exists; validating existing CRD matches expected"
            );
            let existing = get_crd(api, crd_name).await?.ok_or_else(|| {
                MigrationError::Crd(format!(
                    "CRD {crd_name} returned 409 on create but not found on get"
                ))
            })?;
            validate_corrected_crd_schema(&existing)?;
            Ok(())
        }
        Err(e) => Err(MigrationError::Kube(
            format!("create CRD {crd_name}"),
            Box::new(e),
        )),
    }
}

async fn wait_for_crd_established(
    api: &Api<CustomResourceDefinition>,
    name: &str,
    timeout: Duration,
) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        if let Some(crd) = get_crd(api, name).await? {
            if is_crd_ready(&crd) {
                return Ok(());
            }
        }

        if start.elapsed() > timeout {
            return Err(MigrationError::Timeout(format!(
                "CRD {name} did not become established within timeout"
            )));
        }

        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

async fn wait_for_crd_gone(
    api: &Api<CustomResourceDefinition>,
    name: &str,
    timeout: Duration,
) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        if get_crd(api, name).await?.is_none() {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(MigrationError::Timeout(format!(
                "CRD {name} still present after timeout"
            )));
        }

        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

fn utc_now() -> String {
    jiff::Zoned::now()
        .with_time_zone(jiff::tz::TimeZone::UTC)
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_fail_injection_no_env() {
        assert!(check_fail_injection_value(Phase::BackedUp, None).is_ok());
    }

    #[test]
    fn test_check_fail_injection_matching() {
        let result = check_fail_injection_value(Phase::BackedUp, Some("BackedUp"));
        assert!(result.is_err());
        match result.unwrap_err() {
            MigrationError::InjectedFailure(phase) => assert_eq!(phase, "BackedUp"),
            e => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn test_check_fail_injection_non_matching() {
        assert!(check_fail_injection_value(Phase::BackedUp, Some("OperatorStopped")).is_ok());
    }

    #[test]
    fn test_person_api_resource() {
        let ar = person_api_resource();
        assert_eq!(ar.group, API_GROUP);
        assert_eq!(ar.version, API_VERSION);
        assert_eq!(ar.kind, KIND);
        assert_eq!(ar.plural, LEGACY_PLURAL);
    }

    #[test]
    fn test_corrected_person_api_resource() {
        let ar = corrected_person_api_resource();
        assert_eq!(ar.group, API_GROUP);
        assert_eq!(ar.version, API_VERSION);
        assert_eq!(ar.kind, KIND);
        assert_eq!(ar.plural, crate::CORRECTED_PLURAL);
    }

    #[test]
    fn test_migration_config_default() {
        let config = MigrationConfig::default();
        assert_eq!(config.namespace, "kaniop");
        assert_eq!(config.operator_deployment, "kaniop");
        assert_eq!(config.marker_name, DEFAULT_MARKER_NAME);
    }

    #[test]
    fn test_utc_now_format() {
        let ts = utc_now();
        assert!(ts.ends_with('Z'), "timestamp should end with Z: {ts}");
        assert!(
            ts.contains('T'),
            "timestamp should contain T separator: {ts}"
        );
        assert_eq!(ts.len(), 20, "unexpected timestamp length: {ts}");
    }

    #[test]
    fn test_restore_object_deserialization() {
        let json = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {
                "name": "alice",
                "namespace": "default",
                "labels": {"app": "test"},
                "annotations": {"note": "value"}
            },
            "spec": {
                "kanidmRef": {"name": "kanidm"},
                "displayName": "Alice"
            }
        });

        let obj: DynamicObject = serde_json::from_value(json).unwrap();
        assert_eq!(obj.name_any(), "alice");
        assert_eq!(obj.namespace().as_deref(), Some("default"));
        assert!(obj.data.get("spec").is_some());
        assert_eq!(obj.data["spec"]["displayName"].as_str().unwrap(), "Alice");
        assert_eq!(
            obj.labels().get("app").map(|s| s.as_str()).unwrap_or(""),
            "test"
        );
    }

    #[test]
    fn test_restore_object_preserves_owner_references() {
        let json = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {
                "name": "alice",
                "namespace": "default",
                "ownerReferences": [{
                    "apiVersion": "kaniop.rs/v1beta1",
                    "kind": "Kanidm",
                    "name": "kanidm",
                    "uid": "owner-uid"
                }]
            },
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        let obj: DynamicObject = serde_json::from_value(json).unwrap();
        let owners = obj.metadata.owner_references.as_ref().unwrap();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].name, "kanidm");
    }

    #[test]
    fn test_metadata_spec_matches_identical() {
        let desired = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {
                "name": "alice",
                "namespace": "default",
                "labels": {"app": "test"}
            },
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        let existing = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {
                "name": "alice",
                "namespace": "default",
                "uid": "some-uid",
                "resourceVersion": "123",
                "labels": {"app": "test"}
            },
            "spec": {"kanidmRef": {"name": "kanidm"}},
            "status": {"ready": true}
        });

        assert!(metadata_spec_matches(&desired, &existing));
    }

    #[test]
    fn test_metadata_spec_matches_different() {
        let desired = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}, "displayName": "Alice"}
        });

        let existing = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}, "displayName": "Bob"}
        });

        assert!(!metadata_spec_matches(&desired, &existing));
    }
}
