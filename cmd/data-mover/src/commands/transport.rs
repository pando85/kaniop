use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::ExitCode;
use s3::bucket::Bucket;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::checksum;
use crate::crypto;
use crate::s3::{S3Config, S3Error, SseHeaders, create_bucket};

use super::listing::{extract_backup_id_from_manifest_key, list_manifest_keys};
use super::load_operation;
use super::upload_shared::{
    ManifestParams, UploadEncryptionConfig, build_manifest, upload_manifest_conditional,
    upload_payload_streaming, verify_commit,
};

pub async fn run(operation_doc_path: &str) -> Result<(), i32> {
    let doc = load_operation(operation_doc_path).await?;
    let op = match &doc.spec {
        OperationSpec::Transport(op) => op,
        _ => {
            error!("expected transport operation");
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    setup_signal_handler(shutdown_tx);

    if !is_primary_pod() {
        info!("not the designated primary; transport idling");
        loop {
            if *shutdown_rx.borrow() {
                info!("transport shutdown complete");
                return Ok(());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = shutdown_rx.changed() => {
                    info!("transport shutdown complete");
                    return Ok(());
                }
            }
        }
    }

    let watch_dir = Path::new(&op.watch_dir);
    let poll_interval = Duration::from_secs(op.poll_interval_secs.min(60));
    wait_for_watch_dir(watch_dir, &op.watch_dir, poll_interval, &mut shutdown_rx).await?;

    let endpoint = &op.endpoint;
    let region = &op.region;

    if endpoint.is_empty() || region.is_empty() {
        error!("endpoint and region are required for transport");
        return Err(ExitCode::InvalidInput as i32);
    }

    let s3_config = S3Config {
        bucket: op.bucket.clone(),
        endpoint: endpoint.clone(),
        region: region.clone(),
        force_path_style: op.force_path_style,
        ca_bundle_path: op.ca_bundle_path.clone(),
        insecure: op.insecure,
    };

    let bucket = create_bucket(&s3_config).await.map_err(|e| {
        match e {
            S3Error::MissingCredentials => {
                error!("credentials not configured");
            }
            _ => {
                error!(error = %e, "failed to create S3 client");
            }
        }
        ExitCode::Retryable as i32
    })?;

    let repo_path = RepositoryPath::new(&op.bucket, &op.prefix).map_err(|e| {
        error!(error = %e, "invalid repository path");
        ExitCode::InvalidInput as i32
    })?;

    let manifests_prefix = repo_path
        .manifests_prefix(&op.namespace_uid, &op.kanidm_uid)
        .map_err(|e| {
            error!(error = %e, "failed to construct manifests prefix");
            ExitCode::InvalidInput as i32
        })?;

    let (known_backup_ids, legacy_v7_timestamps_ms) =
        backfill_existing_backups(&bucket, &manifests_prefix, op.max_retries).await?;

    info!(
        known_backups = known_backup_ids.len(),
        legacy_v7_count = legacy_v7_timestamps_ms.len(),
        "startup backfill complete, entering poll loop"
    );

    run_poll_loop(
        &bucket,
        &repo_path,
        op,
        watch_dir,
        known_backup_ids,
        legacy_v7_timestamps_ms,
        shutdown_rx,
    )
    .await?;

    info!("transport shutdown complete");
    Ok(())
}

fn setup_signal_handler(shutdown_tx: watch::Sender<bool>) {
    tokio::spawn(async move {
        let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
        let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT");
        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM, initiating graceful shutdown");
            }
            _ = sigint.recv() => {
                info!("received SIGINT, initiating graceful shutdown");
            }
        }
        let _ = shutdown_tx.send(true);
    });
}

async fn backfill_existing_backups(
    bucket: &Bucket,
    manifests_prefix: &str,
    max_retries: u32,
) -> Result<(HashSet<String>, HashSet<u64>), i32> {
    let manifest_keys =
        list_manifest_keys(bucket, manifests_prefix, usize::MAX, max_retries).await?;

    let mut known_ids = HashSet::new();
    let mut legacy_v7_timestamps_ms = HashSet::new();

    for key in &manifest_keys {
        if let Some(backup_id) = extract_backup_id_from_manifest_key(key) {
            known_ids.insert(backup_id.to_string());
            if let Some(ts_ms) = extract_v7_timestamp_ms(backup_id) {
                legacy_v7_timestamps_ms.insert(ts_ms);
            }
        }
    }

    Ok((known_ids, legacy_v7_timestamps_ms))
}

async fn run_poll_loop(
    bucket: &Bucket,
    repo_path: &RepositoryPath,
    op: &kaniop_backup_core::operation::TransportOperation,
    watch_dir: &Path,
    mut known_backup_ids: HashSet<String>,
    legacy_v7_timestamps_ms: HashSet<u64>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), i32> {
    let poll_interval = Duration::from_secs(op.poll_interval_secs);
    let mut previous_scan: HashMap<PathBuf, (u64, SystemTime)> = HashMap::new();

    loop {
        if *shutdown_rx.borrow() {
            return Ok(());
        }

        if !watch_dir.is_dir() {
            warn!(watch_dir = %op.watch_dir, "watch directory temporarily unavailable, will retry");
            tokio::select! {
                _ = tokio::time::sleep(poll_interval) => continue,
                _ = shutdown_rx.changed() => return Ok(()),
            }
        }

        let candidates = scan_for_candidates(
            watch_dir,
            &op.file_prefix,
            &op.file_suffix,
            op.min_file_age_secs,
            &mut previous_scan,
        )
        .await;

        for candidate in candidates {
            if *shutdown_rx.borrow() {
                return Ok(());
            }

            let backup_id = match derive_backup_id(
                &candidate.path,
                &op.file_prefix,
                &op.namespace_uid,
                &op.kanidm_uid,
            ) {
                Some(id) => id,
                None => {
                    warn!(path = %candidate.path.display(), "could not derive backup ID, skipping");
                    continue;
                }
            };

            if known_backup_ids.contains(&backup_id) {
                info!(backup_id = %backup_id, "backup already known, skipping");
                continue;
            }

            if candidate_matches_legacy_v7(
                &candidate.path,
                &op.file_prefix,
                &legacy_v7_timestamps_ms,
            ) {
                info!(
                    backup_id = %backup_id,
                    "backup matches legacy v7 manifest, skipping"
                );
                known_backup_ids.insert(backup_id.clone());
                continue;
            }

            match upload_candidate(bucket, repo_path, op, &candidate, &backup_id).await {
                Ok(()) => {
                    known_backup_ids.insert(backup_id.clone());
                    info!(backup_id = %backup_id, "backup uploaded successfully");
                }
                Err(UploadError::AlreadyExists) => {
                    known_backup_ids.insert(backup_id.clone());
                    info!(backup_id = %backup_id, "backup already committed in repository");
                }
                Err(UploadError::Transient(e)) => {
                    warn!(backup_id = %backup_id, error = %e, "transient upload error, will retry next tick");
                }
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = shutdown_rx.changed() => return Ok(()),
        }
    }
}

fn is_primary_pod() -> bool {
    let pod_name = std::env::var("POD_NAME").ok();
    let primary_node = std::env::var("KANIDM_PRIMARY_NODE").ok();

    match (pod_name, primary_node) {
        (Some(pod), Some(primary)) => pod == primary,
        _ => {
            warn!("POD_NAME or KANIDM_PRIMARY_NODE not set; defaulting to non-primary");
            false
        }
    }
}

async fn wait_for_watch_dir(
    watch_dir: &Path,
    watch_dir_str: &str,
    poll_interval: Duration,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<(), i32> {
    if watch_dir.is_dir() {
        return Ok(());
    }
    info!(watch_dir = %watch_dir_str, "watch directory not yet available, waiting for it to appear");
    loop {
        if *shutdown_rx.borrow() {
            return Err(ExitCode::Retryable as i32);
        }
        if watch_dir.is_dir() {
            return Ok(());
        }
        debug!(watch_dir = %watch_dir_str, "watch directory still missing, retrying");
        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = shutdown_rx.changed() => return Err(ExitCode::Retryable as i32),
        }
    }
}

struct CandidateFile {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

async fn scan_for_candidates(
    watch_dir: &Path,
    file_prefix: &str,
    file_suffix: &str,
    min_file_age_secs: u64,
    previous_scan: &mut HashMap<PathBuf, (u64, SystemTime)>,
) -> Vec<CandidateFile> {
    let mut candidates = Vec::new();
    let mut current_scan: HashMap<PathBuf, (u64, SystemTime)> = HashMap::new();

    let entries = match tokio::fs::read_dir(watch_dir).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, dir = %watch_dir.display(), "failed to read watch directory");
            return candidates;
        }
    };

    let mut entries_stream = entries;
    while let Ok(Some(entry)) = entries_stream.next_entry().await {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name_os = entry.file_name();
        let file_name = match file_name_os.to_str() {
            Some(name) => name,
            None => continue,
        };

        if !file_name.starts_with(file_prefix) || !file_name.ends_with(file_suffix) {
            continue;
        }

        let metadata = match entry.metadata().await {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "failed to read file metadata");
                continue;
            }
        };

        let size = metadata.len();
        if size == 0 {
            continue;
        }

        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "failed to read file mtime");
                continue;
            }
        };

        let age = match SystemTime::now().duration_since(mtime) {
            Ok(d) => d.as_secs(),
            Err(_) => continue,
        };

        if age < min_file_age_secs {
            current_scan.insert(path, (size, mtime));
            continue;
        }

        if let Some((prev_size, prev_mtime)) = previous_scan.get(&path) {
            if *prev_size == size && *prev_mtime == mtime {
                candidates.push(CandidateFile {
                    path: path.clone(),
                    size,
                    mtime,
                });
            }
        }

        current_scan.insert(path, (size, mtime));
    }

    *previous_scan = current_scan;

    candidates.sort_by_key(|a| a.mtime);
    candidates
}

fn derive_backup_id(
    path: &Path,
    file_prefix: &str,
    namespace_uid: &str,
    kanidm_uid: &str,
) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;

    let stem = file_name
        .strip_prefix(file_prefix)
        .and_then(|s| s.rsplit_once('.'))
        .map(|(ts, _)| ts)
        .unwrap_or(file_name);

    let mut name = String::with_capacity(namespace_uid.len() + kanidm_uid.len() + stem.len() + 2);
    name.push_str(namespace_uid);
    name.push(':');
    name.push_str(kanidm_uid);
    name.push(':');
    name.push_str(stem);

    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_DNS, name.as_bytes());
    Some(uuid.to_string())
}

fn extract_v7_timestamp_ms(uuid_str: &str) -> Option<u64> {
    let uuid = Uuid::parse_str(uuid_str).ok()?;
    if uuid.get_version_num() != 7 {
        return None;
    }
    let bytes = uuid.as_bytes();
    let ts_ms = ((bytes[0] as u64) << 40)
        | ((bytes[1] as u64) << 32)
        | ((bytes[2] as u64) << 24)
        | ((bytes[3] as u64) << 16)
        | ((bytes[4] as u64) << 8)
        | (bytes[5] as u64);
    Some(ts_ms)
}

fn extract_filename_timestamp_ms(path: &Path, file_prefix: &str) -> Option<u64> {
    let file_name = path.file_name()?.to_str()?;
    let after_prefix = file_name.strip_prefix(file_prefix)?;

    let mut try_str = after_prefix;
    loop {
        if let Ok(dt) = DateTime::parse_from_rfc3339(try_str) {
            return Some(dt.timestamp_millis() as u64);
        }
        match try_str.rsplit_once('.') {
            Some((before, _)) if !before.is_empty() => try_str = before,
            _ => break,
        }
    }
    None
}

fn candidate_matches_legacy_v7(
    path: &Path,
    file_prefix: &str,
    legacy_timestamps_ms: &HashSet<u64>,
) -> bool {
    extract_filename_timestamp_ms(path, file_prefix)
        .is_some_and(|ts_ms| legacy_timestamps_ms.contains(&ts_ms))
}

enum UploadError {
    AlreadyExists,
    Transient(String),
}

async fn upload_candidate(
    bucket: &Bucket,
    repo_path: &RepositoryPath,
    op: &kaniop_backup_core::operation::TransportOperation,
    candidate: &CandidateFile,
    backup_id: &str,
) -> Result<(), UploadError> {
    let filename = candidate
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "backup.json.gz".to_string());

    let payload_key = repo_path
        .payload_key(&op.namespace_uid, &op.kanidm_uid, backup_id, &filename)
        .map_err(|e| UploadError::Transient(format!("failed to construct payload key: {e}")))?;

    let manifest_key = repo_path
        .manifest_key(&op.namespace_uid, &op.kanidm_uid, backup_id)
        .map_err(|e| UploadError::Transient(format!("failed to construct manifest key: {e}")))?;

    info!(
        backup_id = %backup_id,
        path = %candidate.path.display(),
        size = candidate.size,
        "starting backup upload"
    );

    let local_checksum = checksum::compute_sha256(&candidate.path)
        .await
        .map_err(|e| UploadError::Transient(format!("failed to compute checksum: {e}")))?;

    let sse = SseHeaders::from_operation_fields(
        op.encryption_mode.as_deref(),
        op.encryption_key_id.as_deref(),
    );

    let envelope = if op.encryption_mode.as_deref() == Some("clientSide") {
        crypto::load_envelope_for_upload(crate::s3::DEFAULT_PART_SIZE as u64)
            .map_err(|e| {
                UploadError::Transient(format!("client-side encryption setup failed: {e}"))
            })?
            .into()
    } else {
        None
    };

    let enc = UploadEncryptionConfig {
        sse: sse.clone(),
        envelope,
    };

    upload_payload_streaming(
        bucket,
        &candidate.path,
        &payload_key,
        op.max_retries,
        op.max_concurrent_parts,
        &enc,
    )
    .await
    .map_err(|e| UploadError::Transient(format!("payload upload failed: {e}")))?;

    let client_side_meta = enc.envelope.as_ref().map(|(_, meta)| meta.clone());

    let params = ManifestParams {
        backup_id,
        namespace_uid: &op.namespace_uid,
        kanidm_uid: &op.kanidm_uid,
        kanidm_name: &op.kanidm_name,
        domain: &op.domain,
        kanidm_version: &op.kanidm_version,
        image_digest: op.image_digest.as_deref(),
        consistency: &op.consistency,
        reason: &op.reason,
        encryption_mode: op.encryption_mode.as_deref(),
        encryption_key_id: op.encryption_key_id.as_deref(),
        client_side_meta,
    };

    let manifest = build_manifest(&params, &payload_key, &local_checksum);
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| UploadError::Transient(format!("failed to serialize manifest: {e}")))?;

    match upload_manifest_conditional(
        bucket,
        &manifest_key,
        &manifest_json,
        op.max_retries,
        sse.as_ref(),
    )
    .await
    {
        Ok(()) => {}
        Err(e) if e == ExitCode::Integrity as i32 => {
            return Err(UploadError::AlreadyExists);
        }
        Err(e) => {
            return Err(UploadError::Transient(format!(
                "manifest upload failed: {e}"
            )));
        }
    }

    verify_commit(bucket, &manifest_key, &manifest_json)
        .await
        .map_err(|e| UploadError::Transient(format!("commit verification failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::tempdir;

    use filetime::{FileTime, set_file_mtime};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn derive_backup_id_from_rfc3339_filename() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41.123456789+00:00.json.gz");
        let id = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1");
        assert!(id.is_some());
        let uuid_str = id.unwrap();
        assert!(Uuid::parse_str(&uuid_str).is_ok());
    }

    #[test]
    fn derive_backup_id_with_custom_prefix() {
        let path = Path::new("/data/backups/mybackup-2026-08-18T02:03:41+00:00.json.gz");
        let id = derive_backup_id(path, "mybackup-", "ns-uid-1", "kanidm-uid-1");
        assert!(id.is_some());
        assert!(Uuid::parse_str(&id.unwrap()).is_ok());
    }

    #[test]
    fn derive_backup_id_mismatched_prefix_still_deterministic() {
        let path = Path::new("/data/backups/other-2026-08-18T02:03:41+00:00.json.gz");
        let id1 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1");
        let id2 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1");
        assert!(id1.is_some());
        assert_eq!(id1, id2);
    }

    #[test]
    fn derive_backup_id_fallback_to_full_filename() {
        let path = Path::new("/data/backups/backup-unknown-format.json.gz");
        let id = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1");
        assert!(id.is_some());
        let uuid_str = id.unwrap();
        assert!(Uuid::parse_str(&uuid_str).is_ok());
    }

    #[test]
    fn derive_backup_id_is_deterministic_across_calls() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41+00:00.json.gz");
        let id1 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1");
        let id2 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1");
        assert_eq!(id1, id2);
    }

    #[test]
    fn derive_backup_id_distinct_timestamps_produce_distinct_ids() {
        let path1 = Path::new("/data/backups/backup-2026-08-18T02:03:41+00:00.json.gz");
        let path2 = Path::new("/data/backups/backup-2026-08-18T03:00:00+00:00.json.gz");
        let id1 = derive_backup_id(path1, "backup-", "ns-uid-1", "kanidm-uid-1").unwrap();
        let id2 = derive_backup_id(path2, "backup-", "ns-uid-1", "kanidm-uid-1").unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn derive_backup_id_distinct_kanidm_uids_produce_distinct_ids() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41+00:00.json.gz");
        let id1 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-A").unwrap();
        let id2 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-B").unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn derive_backup_id_distinct_namespaces_produce_distinct_ids() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41+00:00.json.gz");
        let id1 = derive_backup_id(path, "backup-", "ns-uid-X", "kanidm-uid-1").unwrap();
        let id2 = derive_backup_id(path, "backup-", "ns-uid-Y", "kanidm-uid-1").unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn scan_for_candidates_filters_by_prefix_and_suffix() {
        let dir = tempdir().unwrap();
        let watch_dir = dir.path();

        File::create(watch_dir.join("backup-2026-08-18T00:00:00Z.json.gz"))
            .unwrap()
            .write_all(b"content")
            .unwrap();
        File::create(watch_dir.join("other-file.txt"))
            .unwrap()
            .write_all(b"content")
            .unwrap();
        File::create(watch_dir.join("backup-no-suffix.bin"))
            .unwrap()
            .write_all(b"content")
            .unwrap();

        let old_time = SystemTime::now() - Duration::from_secs(300);
        set_file_mtime(
            watch_dir.join("backup-2026-08-18T00:00:00Z.json.gz"),
            FileTime::from_system_time(old_time),
        )
        .unwrap();

        let mut previous_scan = HashMap::new();
        let candidates =
            scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;

        assert_eq!(candidates.len(), 0);

        let candidates =
            scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;

        assert_eq!(candidates.len(), 1);
        assert!(
            candidates[0]
                .path
                .to_str()
                .unwrap()
                .contains("backup-2026-08-18")
        );
    }

    #[tokio::test]
    async fn scan_for_candidates_skips_zero_size_files() {
        let dir = tempdir().unwrap();
        let watch_dir = dir.path();

        File::create(watch_dir.join("backup-empty.json.gz")).unwrap();

        let old_time = SystemTime::now() - Duration::from_secs(300);
        set_file_mtime(
            watch_dir.join("backup-empty.json.gz"),
            FileTime::from_system_time(old_time),
        )
        .unwrap();

        let mut previous_scan = HashMap::new();
        let _ = scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;
        let candidates =
            scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;

        assert_eq!(candidates.len(), 0);
    }

    #[tokio::test]
    async fn scan_for_candidates_requires_stability() {
        let dir = tempdir().unwrap();
        let watch_dir = dir.path();

        let file_path = watch_dir.join("backup-stable.json.gz");
        File::create(&file_path)
            .unwrap()
            .write_all(b"content")
            .unwrap();

        let old_time = SystemTime::now() - Duration::from_secs(300);
        set_file_mtime(&file_path, FileTime::from_system_time(old_time)).unwrap();

        let mut previous_scan = HashMap::new();

        let candidates =
            scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;
        assert_eq!(candidates.len(), 0, "first scan should not find candidates");

        let candidates =
            scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;
        assert_eq!(
            candidates.len(),
            1,
            "second scan should find stable candidate"
        );
    }

    #[tokio::test]
    async fn scan_for_candidates_orders_by_mtime() {
        let dir = tempdir().unwrap();
        let watch_dir = dir.path();

        let file1 = watch_dir.join("backup-older.json.gz");
        let file2 = watch_dir.join("backup-newer.json.gz");

        File::create(&file1)
            .unwrap()
            .write_all(b"content1")
            .unwrap();
        File::create(&file2)
            .unwrap()
            .write_all(b"content2")
            .unwrap();

        let older_time = SystemTime::now() - Duration::from_secs(600);
        let newer_time = SystemTime::now() - Duration::from_secs(300);

        set_file_mtime(&file1, FileTime::from_system_time(older_time)).unwrap();
        set_file_mtime(&file2, FileTime::from_system_time(newer_time)).unwrap();

        let mut previous_scan = HashMap::new();
        let _ = scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;
        let candidates =
            scan_for_candidates(watch_dir, "backup-", ".json.gz", 60, &mut previous_scan).await;

        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].path.to_str().unwrap().contains("older"));
        assert!(candidates[1].path.to_str().unwrap().contains("newer"));
    }

    #[test]
    fn is_primary_pod_returns_false_when_not_primary() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("POD_NAME", "kanidm-default-1");
            std::env::set_var("KANIDM_PRIMARY_NODE", "kanidm-default-0");
        }
        assert!(!is_primary_pod());
        unsafe {
            std::env::remove_var("POD_NAME");
            std::env::remove_var("KANIDM_PRIMARY_NODE");
        }
    }

    #[test]
    fn is_primary_pod_returns_true_when_primary() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("POD_NAME", "kanidm-default-0");
            std::env::set_var("KANIDM_PRIMARY_NODE", "kanidm-default-0");
        }
        assert!(is_primary_pod());
        unsafe {
            std::env::remove_var("POD_NAME");
            std::env::remove_var("KANIDM_PRIMARY_NODE");
        }
    }

    #[test]
    fn is_primary_pod_returns_false_when_env_unset() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("POD_NAME");
            std::env::remove_var("KANIDM_PRIMARY_NODE");
        }
        assert!(!is_primary_pod());
    }

    #[tokio::test]
    async fn wait_for_watch_dir_proceeds_once_dir_appears() {
        let parent = tempdir().unwrap();
        let watch_dir = parent.path().join("backups");
        let watch_dir_str = watch_dir.to_string_lossy().to_string();
        let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let watch_dir_clone = watch_dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            tokio::fs::create_dir_all(&watch_dir_clone).await.unwrap();
        });

        let result = wait_for_watch_dir(
            &watch_dir,
            &watch_dir_str,
            Duration::from_millis(50),
            &mut shutdown_rx,
        )
        .await;
        assert!(result.is_ok());
        assert!(watch_dir.is_dir());
    }

    #[tokio::test]
    async fn wait_for_watch_dir_returns_immediately_when_dir_exists() {
        let dir = tempdir().unwrap();
        let watch_dir = dir.path();
        let watch_dir_str = watch_dir.to_string_lossy().to_string();
        let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let result = wait_for_watch_dir(
            watch_dir,
            &watch_dir_str,
            Duration::from_secs(1),
            &mut shutdown_rx,
        )
        .await;
        assert!(result.is_ok());
    }

    fn make_v7_uuid(ts_ms: u64) -> Uuid {
        let ts_bytes = ts_ms.to_be_bytes();
        let mut bytes = [0u8; 16];
        bytes[0] = ts_bytes[2];
        bytes[1] = ts_bytes[3];
        bytes[2] = ts_bytes[4];
        bytes[3] = ts_bytes[5];
        bytes[4] = ts_bytes[6];
        bytes[5] = ts_bytes[7];
        bytes[6] = 0x70;
        bytes[8] = 0x80;
        Uuid::from_bytes(bytes)
    }

    #[test]
    fn extract_v7_timestamp_ms_from_valid_v7_uuid() {
        let ts_ms = 1_723_943_021_123u64;
        let uuid = make_v7_uuid(ts_ms);
        let extracted = extract_v7_timestamp_ms(&uuid.to_string());
        assert_eq!(extracted, Some(ts_ms));
    }

    #[test]
    fn extract_v7_timestamp_ms_returns_none_for_v5_uuid() {
        let uuid = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test");
        assert_eq!(extract_v7_timestamp_ms(&uuid.to_string()), None);
    }

    #[test]
    fn extract_v7_timestamp_ms_returns_none_for_invalid_string() {
        assert_eq!(extract_v7_timestamp_ms("not-a-uuid"), None);
        assert_eq!(extract_v7_timestamp_ms(""), None);
    }

    #[test]
    fn extract_filename_timestamp_ms_nanosecond_precision() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41.123456789+00:00.json.gz");
        let ts = extract_filename_timestamp_ms(path, "backup-");
        let expected = DateTime::parse_from_rfc3339("2026-08-18T02:03:41.123456789+00:00")
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(ts, Some(expected));
    }

    #[test]
    fn extract_filename_timestamp_ms_second_precision() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41+00:00.json.gz");
        let ts = extract_filename_timestamp_ms(path, "backup-");
        let expected = DateTime::parse_from_rfc3339("2026-08-18T02:03:41+00:00")
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(ts, Some(expected));
    }

    #[test]
    fn extract_filename_timestamp_ms_millisecond_precision() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41.123+00:00.json.gz");
        let ts = extract_filename_timestamp_ms(path, "backup-");
        let expected = DateTime::parse_from_rfc3339("2026-08-18T02:03:41.123+00:00")
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(ts, Some(expected));
    }

    #[test]
    fn extract_filename_timestamp_ms_z_timezone() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41Z.json.gz");
        let ts = extract_filename_timestamp_ms(path, "backup-");
        let expected = DateTime::parse_from_rfc3339("2026-08-18T02:03:41Z")
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(ts, Some(expected));
    }

    #[test]
    fn extract_filename_timestamp_ms_returns_none_for_non_timestamp() {
        let path = Path::new("/data/backups/backup-unknown-format.json.gz");
        assert_eq!(extract_filename_timestamp_ms(path, "backup-"), None);
    }

    #[test]
    fn extract_filename_timestamp_ms_returns_none_for_wrong_prefix() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41Z.json.gz");
        assert_eq!(extract_filename_timestamp_ms(path, "other-"), None);
    }

    #[test]
    fn candidate_matches_legacy_v7_with_matching_timestamp() {
        let ts_ms = 1_723_943_021_000u64;
        let mut legacy_timestamps = HashSet::new();
        legacy_timestamps.insert(ts_ms);

        let dt = DateTime::from_timestamp_millis(ts_ms as i64).unwrap();
        let filename = format!(
            "backup-{}.json.gz",
            dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        let full_path = format!("/data/backups/{filename}");
        let path = Path::new(&full_path);

        assert!(candidate_matches_legacy_v7(
            path,
            "backup-",
            &legacy_timestamps
        ));
    }

    #[test]
    fn candidate_does_not_match_legacy_v7_with_different_timestamp() {
        let mut legacy_timestamps = HashSet::new();
        legacy_timestamps.insert(1_723_943_021_000u64);

        let path = Path::new("/data/backups/backup-2026-08-18T03:00:00+00:00.json.gz");
        assert!(!candidate_matches_legacy_v7(
            path,
            "backup-",
            &legacy_timestamps
        ));
    }

    #[test]
    fn candidate_does_not_match_empty_legacy_set() {
        let legacy_timestamps = HashSet::new();
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41+00:00.json.gz");
        assert!(!candidate_matches_legacy_v7(
            path,
            "backup-",
            &legacy_timestamps
        ));
    }

    #[test]
    fn candidate_does_not_match_legacy_when_filename_has_no_timestamp() {
        let mut legacy_timestamps = HashSet::new();
        legacy_timestamps.insert(1_723_943_021_000u64);

        let path = Path::new("/data/backups/backup-unknown-format.json.gz");
        assert!(!candidate_matches_legacy_v7(
            path,
            "backup-",
            &legacy_timestamps
        ));
    }

    #[test]
    fn v5_backup_id_is_version_5_and_deterministic() {
        let path = Path::new("/data/backups/backup-2026-08-18T02:03:41+00:00.json.gz");
        let id1 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1").unwrap();
        let id2 = derive_backup_id(path, "backup-", "ns-uid-1", "kanidm-uid-1").unwrap();
        assert_eq!(id1, id2);

        let uuid = Uuid::parse_str(&id1).unwrap();
        assert_eq!(uuid.get_version_num(), 5);
    }

    #[test]
    fn legacy_v7_match_promotes_to_known_ids() {
        let ts_ms = 1_723_943_021_000u64;
        let v7_uuid = make_v7_uuid(ts_ms);

        let mut known_ids = HashSet::new();
        known_ids.insert(v7_uuid.to_string());

        let mut legacy_timestamps = HashSet::new();
        legacy_timestamps.insert(ts_ms);

        let dt = DateTime::from_timestamp_millis(ts_ms as i64).unwrap();
        let filename = format!(
            "backup-{}.json.gz",
            dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        let full_path = format!("/data/backups/{filename}");
        let path = Path::new(&full_path);

        let v5_id = derive_backup_id(path, "backup-", "ns-uid", "k-uid").unwrap();
        assert!(!known_ids.contains(&v5_id));

        assert!(candidate_matches_legacy_v7(
            path,
            "backup-",
            &legacy_timestamps
        ));

        known_ids.insert(v5_id.clone());
        assert!(known_ids.contains(&v5_id));
    }

    #[test]
    fn nanosecond_and_millisecond_filenames_match_same_v7_timestamp() {
        let ts_ms = 1_723_943_021_123u64;
        let mut legacy_timestamps = HashSet::new();
        legacy_timestamps.insert(ts_ms);

        let dt = DateTime::from_timestamp_millis(ts_ms as i64).unwrap();
        let rfc_nano = dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        let rfc_milli = dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let nano_path_str = format!("/data/backups/backup-{rfc_nano}.json.gz");
        let milli_path_str = format!("/data/backups/backup-{rfc_milli}.json.gz");
        let path_nano = Path::new(&nano_path_str);
        let path_milli = Path::new(&milli_path_str);

        let ts_nano = extract_filename_timestamp_ms(path_nano, "backup-").unwrap();
        let ts_milli = extract_filename_timestamp_ms(path_milli, "backup-").unwrap();
        assert_eq!(ts_nano, ts_milli);
        assert_eq!(ts_nano, ts_ms);

        assert!(candidate_matches_legacy_v7(
            path_nano,
            "backup-",
            &legacy_timestamps
        ));
        assert!(candidate_matches_legacy_v7(
            path_milli,
            "backup-",
            &legacy_timestamps
        ));
    }
}
