---
title: Troubleshooting
weight: 50000
---

# Troubleshooting

When something goes wrong, consult the following steps to diagnose and resolve issues with Kaniop:

1. Operator Status

   Check that the operator pod is running and healthy:

   ```bash
   kubectl get pods -n kaniop
   kubectl logs deployment/kaniop -n kaniop
   ```

2. Resource Conditions

   Inspect the status of your Custom Resources (CRs):

   ```bash
   kubectl describe <kind> <name>
   ```

   Look for `Conditions` and `Events` sections to find reconciliation errors.

3. Events and Logs

   Check cluster events for errors:

   ```bash
   kubectl get events --sort-by '.metadata.creationTimestamp'
   ```

   Tail operator logs for insights:

   ```bash
   kubectl logs -f deployment/kaniop -n kaniop
   ```

4. Common Issues

   - **CRD not found**: Ensure CRDs are installed or upgrade chart with CRDs.
   - **RBAC errors**: Verify service account and ClusterRoleBindings.
   - **Missing permissions**: Grant the operator access to necessary resources.

5. Community Support

   If you need help, join the Kaniop discussion or open an issue on GitHub:

   - [GitHub Discussions](https://github.com/pando85/kaniop/discussions)
   - [GitHub Issues](https://github.com/pando85/kaniop/issues)

## Backup and Restore Troubleshooting

### Restore stuck in validation

**Symptom**: `KanidmRestore` remains in `Pending` or `Validating` phase.

**Diagnosis**:
```bash
kubectl describe kanidmrestore <name>
kubectl get events -n <namespace> --field-selector involvedObject.name=<name>
```

**Common causes**:
- **Target UID mismatch**: The `spec.targetRef.uid` does not match the current Kanidm CR UID. Re-read the UID and recreate the restore.
- **Another active restore**: Only one restore can target a Kanidm at a time. Delete or wait for the existing restore to complete.
- **Image mismatch**: The `restoreImage` does not match the backup's Kanidm version. Use the exact pinned image from the backup.
- **Non-persistent storage**: The target Kanidm uses `emptyDir` instead of a PVC. Restore requires persistent storage.

### Restore fails after database mutation

**Symptom**: `KanidmRestore` shows `Failed` and `databaseMutationStarted: true`. The Kanidm target remains offline.

**Diagnosis**:
```bash
kubectl describe kanidmrestore <name>
kubectl logs -n <namespace> job/<restore-job-name>
```

**Recovery**:
1. The target is fail-closed by design. Do not manually start Kanidm pods.
2. If the database is corrupted, create a new restore from a different backup.
3. Deleting the `KanidmRestore` CR does **not** silently release the target lock. To release the lock, annotate the CR with `restore.kaniop.rs/force-release`. The controller emits a Warning event and clears the target maintenance annotation. Without this annotation, the lock persists even after CR deletion.
   ```bash
   kubectl annotate kanidmrestore <name> -n <namespace> \
     restore.kaniop.rs/force-release=yes
   ```
4. As a last resort, delete the `KanidmRestore` (the finalizer blocks deletion until the database is verified) and recreate the Kanidm CR from scratch.

### Backup not running

**Symptom**: No new backup files appear in `/data/backups/` despite a configured schedule.

**Diagnosis**:
```bash
# Check Kanidm primary pod logs
kubectl logs -n <namespace> <kanidm-primary-pod> | grep -i backup

# Verify the backup schedule is configured
kubectl get kanidm <name> -o jsonpath='{.spec.backup}'
```

**Common causes**:
- **No primary node**: The Kanidm has no `replicaGroup` with `primaryNode: true`. Online backup only runs on the primary.
- **PVC full**: The PVC has insufficient space for new backups. Check available space and increase PVC size if needed.
- **Schedule misconfiguration**: The cron expression is invalid or the schedule is suspended.

### VolumeAttachment stuck after restore

**Symptom**: After a restore, pods cannot start because VolumeAttachments remain attached to old nodes.

**Diagnosis**:
```bash
kubectl get volumeattachments
kubectl get pods -n <namespace> -o wide
```

**Recovery**:
The controller waits for VolumeAttachments to detach before proceeding. If they remain stuck:
1. Verify the old pods are fully terminated: `kubectl get pods -n <namespace> --field-selector=status.phase!=Running`
2. If a node is down, the VolumeAttachment may need manual cleanup after the node recovers.

### Operator restart during restore

**Symptom**: The operator pod restarted while a restore was in progress.

**Recovery**:
The controller persists phase state in the `KanidmRestore` status. After restart, reconciliation resumes from the last persisted phase. Check that any transient Jobs (backup, restore, verify) complete successfully:
```bash
kubectl get jobs -n <namespace>
kubectl logs -n <namespace> job/<job-name>
```

### Backup deletion deferred (Object Lock or access denied)

**Symptom**: `KanidmBackup` remains in `Deleting` or `Ready` phase with condition `DeletionDeferred` and reason `ObjectLockRetention` or `AccessDenied`. The `kaniop_backup_gc_deferred_total` metric increments.

**Diagnosis**:
```bash
kubectl describe kanidmbackup <name>
kubectl get events -n <namespace> --field-selector involvedObject.name=<name>
```

**Common causes**:
- **Object Lock retention**: The S3 bucket has Object Lock enabled with a retention period that has not yet expired. The backup cannot be deleted until the retention date passes.
- **Access denied**: The deleter identity lacks `s3:DeleteObject` permission on the backup prefix. Check the IAM policy for the deleter role.

**Recovery**:
1. For Object Lock: wait for the retention period to expire. The controller backs off and retries. The alert `KaniopBackupGCDeferred` fires when deferrals accumulate.
2. For access denied: fix the IAM policy for the deleter role and ensure it grants `s3:DeleteObject` on the repository prefix.
3. The `DeletionDeferred` condition clears automatically once the deletion Job succeeds.

### Phantom discovered backups and validation alerts

**Symptom**: `KaniopBackupValidationFailures` fires shortly after each retention pass, `kubectl get kanidmbackup` shows records in `Invalid` phase labeled `kaniop.rs/discovered=true`, and the records reappear after you delete them.

**Diagnosis**:
```bash
kubectl get kanidmbackup -n <namespace> -l kaniop.rs/discovered=true \
  -o custom-columns=NAME:.metadata.name,PHASE:.status.phase,KEY:.spec.manifestKey
kubectl describe kanidmbackup <name> -n <namespace>
kubectl get events -n <namespace> --field-selector reason=ManifestAbsent
```

**Cause**: retention deleted the artifact while a discovery scan that had already started listing the repository was still running. When that scan result was consumed, the `KanidmBackup` was already gone, so the deleted manifest key was recreated as a phantom catalogue entry and its validation Job then failed with HTTP 404 `NoSuchKey`. This is a stale-scan race, not a failing backup: the artifact was already outside the retention window, so no recovery point was lost.

**Recovery**:
1. Discovered records need no manual cleanup. The controller deletes one whose manifest is demonstrably absent, emits a Warning event (`reason: ManifestAbsent`, `action: PhantomBackupDiscarded`), and does not count it in `kaniop_backup_validation_failures_total`. Absence is only concluded when the fetch fails with HTTP 404 **and** an explicit `NoSuchKey` error code; a bare 404, `NoSuchBucket`, an endpoint or path-style mismatch, 403, 5xx, or a transport error stays `MANIFEST_FETCH_ERROR`, which is retryable and parks the record in `Invalid` until a complete scan prunes it.
2. Records already parked in `Invalid` by an older version are pruned by the next complete discovery scan. A scan whose listing is truncated prunes nothing, and truncation covers the `maxResults` result cap, the listing's page cap, and a provider that reports a truncated listing without a usable continuation token, so a leftover record survives until a scan that runs the listing to the end of the prefix. Confirm a prune with `kaniop_backup_discovery_dangling_pruned_total{repository}` or `kubectl logs deployment/kaniop -n kaniop | grep "pruned dangling discovered backup"`; the `, pruned N dangling` suffix in the `Discovered` condition message is best-effort and may not persist while [issue #1082](https://github.com/pando85/kaniop/issues/1082) is open.
3. If the alert still fires, the affected record is one you authored yourself. `Ready=False` with reason `ManifestAbsent` means the provider answered `NoSuchKey` for `spec.manifestKey`; reason `ValidationFailed` means the manifest could not be fetched, was ambiguous about being absent, or could not be parsed. Check repository connectivity and credentials, then re-read the manifest key from the repository listing.
4. Confirm the catalogue still matches reality: `kubectl get kanidmbackup -n <namespace> -l kaniop.rs/discovered=true` should leave only backups whose manifests are present, and `kaniop_backup_last_success_timestamp` should still reflect your newest retained backup.

### KanidmBackup stays Terminating after deletion

**Symptom**: A `KanidmBackup` stays `Terminating` for minutes, never longer than 30, even though its deletion Job completed and the S3 objects are gone. No `DeletionDeferred` condition is set.

**Diagnosis**:
```bash
kubectl describe kanidmbackup <name> -n <namespace>
kubectl get jobs -n <namespace> -l kaniop.rs/repository=<repo-name>,kaniop.rs/operation=discover
kubectl logs deployment/kaniop -n kaniop | grep "stale discover Job"
```

**Common causes**:
- **Waiting for discovery to observe the deletion**: a discover Job for the repository started before the deletion and could still report the deleted manifest key. The finalizer is held so the record stays in the list discovery uses to suppress recreating it.
- **Stuck discover Job**: an unfinished discover Job, or one whose result cannot be read, is treated as possibly referencing the key until the bound expires.

**Recovery**:
1. Wait. The hold is bounded at 1800 seconds after the deletion, after which the operator logs `discovery confirmation grace exceeded; removing finalizer` and the record disappears. The deletion time is now taken from the deletion Job's cluster-side `status.completionTime`, falling back to the operator clock only when the Job carries none, so both sides of the comparison against the discover Jobs' start times are cluster-stamped and operator/cluster clock skew cannot open the gate early; the grace itself is measured against the operator clock, where a sub-minute skew is negligible at this magnitude. A record resurrected in a residual window is still discarded by the phantom path or pruned by a later complete scan rather than left `Invalid`.
2. If the hold repeats, resolve why the discover Job does not finish. Repository connectivity problems also surface as `KaniopBackupDiscoveryStale` and `kaniop_backup_discovery_scan_failures_total`.

### Repository not ready: KEK Secret missing

**Symptom**: `KanidmBackupRepository` has condition `EncryptionKeyReady=False` with reason `MissingSecret` or `MissingKey`. The transport sidecar is not injected into Kanidm pods. Alert `KaniopBackupRepositoryNotReady` fires.

**Diagnosis**:
```bash
kubectl describe kanidmbackuprepository <name>
kubectl get secret <keyRef-name> -n <namespace>
```

**Common causes**:
- **MissingSecret**: The Secret referenced by `spec.encryption.keyRef` does not exist in the namespace.
- **MissingKey**: The Secret exists but does not contain the expected key (default: `encryption-key`).

**Recovery**:
1. Create or restore the KEK Secret with the correct key name and a 32-byte value.
2. The repository controller re-checks automatically and sets `EncryptionKeyReady=True` (reason `KeyPresent`). The transport sidecar is injected on the next reconcile.
3. Kanidm pods are not restarted or affected by the missing KEK; backups simply pause until the Secret is restored.

### Break-glass restore

If the current PVC is unreadable and no safety backup is possible, a break-glass restore bypasses the safety backup requirement:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmRestore
metadata:
  name: emergency-restore
  annotations:
    backup.kaniop.rs/break-glass-reason: "PVC is unreadable after node failure"
    backup.kaniop.rs/break-glass-approved-by: "incident-commander@example.com"
spec:
  safetyBackup:
    skip: true
  # ... rest of spec
```

This emits a `SafetyBackupSkipped` Warning Event and increments the `kaniop_restore_break_glass_total` metric. The Kubernetes audit log records the actor identity.
