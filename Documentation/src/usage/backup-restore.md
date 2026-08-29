# Backup and Restore

> [!WARNING]
> **Experimental and incomplete.** Kaniop's backup and restore subsystem is still under active development. APIs and behavior may change, some workflows are not yet fully implemented or hardened, and the feature is **not yet production-supported**. Do not rely on it as the sole backup or disaster-recovery mechanism. See the [production backup and restore implementation plan](https://github.com/pando85/kaniop/blob/master/docs/plans/production-kanidm-backup-and-restore.md) for the remaining production gates.

Kaniop uses Kanidm's native logical backup format. The operator configures the online backup scheduler on exactly one primary node and stores local artifacts under `/data/backups` on the Kanidm PVC.

```yaml
spec:
  backup:
    schedule: "0 2 * * *"
    versions: 7
```

Local backups require PVC-backed storage and one `replicaGroup` with `primaryNode: true`. Kaniop intentionally does not claim PITR semantics or a globally atomic point-in-time cut across replicated writable nodes.

### Native Backup Behavior

**Schedule Timing and Time Zone:**

- The `schedule` field uses cron syntax and is interpreted in the `timeZone` field (defaults to UTC).
- Kaniop renders the schedule into Kanidm's `[online_backup]` configuration block on the primary node only.
- Backup files are written to `/data/backups` on the Kanidm PVC with filenames like `backup-<timestamp>.json`.
- Local retention is controlled by `localVersions` (default 7), which keeps the N most recent backup files on the PVC.

**StatefulSet Rolling Behavior:**

- When you change the backup schedule, time zone, or local versions, Kaniop updates the Kanidm StatefulSet's pod template environment variables.
- The StatefulSet controller performs a rolling update of the pods to apply the new configuration.
- This means backup configuration changes trigger a pod restart, which may cause a brief interruption to the Kanidm service.
- The rolling update follows the StatefulSet's update strategy (typically `RollingUpdate` with `partition: 0`).

**Primary Node Selection:**

- Kaniop configures the online backup scheduler only on the node with `primaryNode: true` in the `replicaGroup`.
- If no node is marked as primary, the backup scheduler is not configured.
- The primary node is responsible for writing backup files to the shared PVC.

**Troubleshooting Backup Schedule Issues:**

If backups are not being created, verify the following:

1. **Check Kanidm logs for online_backup scheduler messages:**
   ```bash
   kubectl logs -n <namespace> <kanidm-primary-pod> | grep -i "online_backup\|backup"
   ```
   Expected: log lines showing the backup scheduler started with the configured schedule. Absence of these messages indicates the scheduler did not initialize.

2. **Verify the rendered configuration:**
   Kaniop renders the schedule into Kanidm's `[online_backup]` configuration block via environment variables on the primary pod:
   ```bash
   kubectl exec -n <namespace> <kanidm-primary-pod> -- env | grep KANIDM_BACKUP
   ```

3. **Verify the schedule is correct for the time zone:**
   The `schedule` field uses cron syntax interpreted in the `timeZone` field (defaults to UTC). A schedule of `"0 2 * * *"` fires at 02:00 UTC daily.

4. **Verify the backup directory is writable:**
   The Kanidm process writes backup files to `/data/backups` on the PVC. Verify the directory exists and is writable by the container's configured runtime UID:
   ```bash
   kubectl exec -n <namespace> <kanidm-primary-pod> -- test -w /data/backups && echo "writable" || echo "not writable"
   ```

5. **Check pod uptime:**
   If the Kanidm pod recently restarted, the backup scheduler may not have triggered yet. Verify the pod has been running long enough for at least one schedule interval:
   ```bash
   kubectl get pod -n <namespace> <kanidm-primary-pod> -o jsonpath='{.status.startTime}'
   ```

## Remote Backups (S3-Compatible Repositories)

Kaniop supports remote backup repositories for disaster recovery. This uses three additional resources:

- **KanidmBackupRepository**: Defines an S3-compatible destination with authentication, encryption, and transport limits.
- **KanidmBackupSchedule**: The single source of the Kanidm online backup cron, local retention, and remote retention policy. Only one Schedule may target a given Kanidm at a time.
- **KanidmBackup**: Immutable catalog metadata for a committed remote backup. These are discovered automatically from remote manifests.

### Online Backup Transport: Experimental Status

The `TransportExperimental` condition on `KanidmBackupSchedule` indicates that the online backup transport (moving backups from the Kanidm PVC to the remote repository) is **experimental and not production-supported**.

**What this means:**

- Kanidm has no documented completion contract for online backups. It writes directly to the final filename with no atomic rename, completion marker, or event that an external data mover can use as an unambiguous completion signal.
- Kaniop's data mover uses file stability heuristics (size, mtime, checksums) to detect when a backup is complete, but these are **not a production completion contract**.
- Kaniop **does not report production backup success** based on these heuristics alone. A `Ready` condition on the Schedule means the schedule is configured, not that backups are successfully committed to the remote repository.

**Current support status:**

- The entire Kaniop-managed backup and restore subsystem remains experimental while the production implementation and hardening plan is incomplete.
- Restore from a committed backup, the safety backup created before restore, and local backup workflows are implemented to varying degrees, but they have **not yet passed all production-support gates**.
- Until those gates are complete, use an independent, tested backup and disaster-recovery mechanism and regularly verify that it can restore your Kanidm deployment.

**What blocks production support for online transport?**

Online transport cannot become production-supported until Kanidm documents and Kaniop tests a minimum-version completion contract such as:

- Atomic rename from a temporary filename after successful close
- A completion marker written after close
- An authenticated event/API returning a completed path
- Native upstream object-storage shipping with equivalent commit semantics

Until then, use CSI/Velero snapshots as an independent disaster-recovery layer if you require production-grade remote backups.

### Schedule and Retention Immutability

The following fields on `KanidmBackupSchedule` are **immutable after the first backup has been discovered**:

- `kanidmRef`: The target Kanidm instance
- `repositoryRef`: The target repository
- `schedule`: The cron schedule
- `retention`: The remote retention policy

**Why are these fields immutable?**

Once a backup has been discovered, the Schedule is bound to a specific Kanidm instance, repository, and retention policy. Changing these fields would create ambiguity about which backups belong to which Schedule and could lead to orphaned or incorrectly retained backups.

**How to change these fields:**

To change any of these fields, you must **delete the existing KanidmBackupSchedule and create a new one**:

```bash
# 1. Delete the old schedule
kubectl delete kanidmbackupschedule <name> -n <namespace>

# 2. Create a new schedule with the desired configuration
kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1alpha1
kind: KanidmBackupSchedule
metadata:
  name: <new-name>
  namespace: <namespace>
spec:
  kanidmRef:
    name: <kanidm-name>
  repositoryRef:
    name: <repository-name>
  schedule: "0 3 * * *"  # New schedule
  retention:
    keepLast: 10
    daily: 14
    weekly: 8
    monthly: 12
    minAge: "24h"
EOF
```

**What happens to existing Backup CRs and S3 data?**

- **Existing KanidmBackup CRs are not deleted** when you delete a Schedule. They remain in the cluster as immutable catalog entries.
- **Remote S3 data (payloads and manifests) is not deleted** when you delete a Schedule. The data remains in the repository.
- **Retention policy changes take effect immediately** for the new Schedule. The new Schedule's retention policy is applied to all discovered backups matching the repository and Kanidm UID.
- **The new Schedule will discover existing backups** in the repository if they match the new Schedule's `kanidmRef` and `repositoryRef`.

**Safety considerations:**

- Before deleting a Schedule, verify that the new Schedule's retention policy will not immediately delete backups you need to keep.
- If you are changing repositories, ensure the new repository is accessible and properly configured before deleting the old Schedule.
- The `suspend` field is **mutable** and can be changed at any time to pause or resume the backup schedule without deleting the Schedule.

### Repository Immutability

The following fields on `KanidmBackupRepository` are **immutable after the repository has been used** (after `observedGeneration` is set in status):

- `s3.bucket`: The S3 bucket name
- `s3.prefix`: The prefix within the bucket
- `s3.endpoint`: The S3 endpoint URL

**Why are these fields immutable?**

Once a repository has been used, Backup CRs reference it by name. Changing the bucket, prefix, or endpoint would orphan existing backups and create ambiguity about where backups are stored.

**How to change these fields:**

To change any of these fields, you must **delete the existing KanidmBackupRepository and create a new one**:

```bash
# 1. Delete the old repository
kubectl delete kanidmbackuprepository <name> -n <namespace>

# 2. Create a new repository with the desired configuration
kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1alpha1
kind: KanidmBackupRepository
metadata:
  name: <new-name>
  namespace: <namespace>
spec:
  s3:
    bucket: <new-bucket>
    prefix: <new-prefix>
    endpoint: <new-endpoint>
    region: <region>
  authentication:
    writer:
      workloadIdentity: {}
    reader:
      workloadIdentity: {}
    deleter:
      workloadIdentity: {}
EOF

# 3. Update the KanidmBackupSchedule to reference the new repository
# (This requires deleting and recreating the Schedule; see above)
```

**What happens to existing Backup CRs and S3 data?**

- **Existing KanidmBackup CRs are not deleted** when you delete a Repository. However, they become orphaned and cannot be used for restore because their `repositoryRef` no longer exists.
- **Remote S3 data remains in the old bucket/prefix** and is accessible only through the old Repository configuration. If you delete the Repository, you lose the ability to restore from those backups unless you recreate a Repository with the same name and configuration.
- **To migrate to a new repository**, you must:
  1. Create the new Repository
  2. Delete and recreate the Schedule to reference the new Repository
  3. Optionally, manually copy S3 data from the old repository to the new one if you need to retain access to old backups

**Safety considerations:**

- Before deleting a Repository, ensure you have a Schedule that references a valid Repository, or delete the Schedule first.
- If you need to retain access to old backups, keep the old Repository or document its configuration for potential recreation.
- The `authentication`, `encryption`, and `limits` fields are **mutable** and can be changed at any time to update credentials or transport limits.

## Restore

Restore is an explicit destructive operation represented by `KanidmRestore`. Obtain the target UID with `kubectl get kanidm <name> -o jsonpath='{.metadata.uid}'`, select an existing backup basename from `/data/backups`, and use the same pinned Kanidm image as the target. `latest` and untagged images are rejected.

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmRestore
metadata:
  name: my-idm-restore
spec:
  targetRef:
    name: my-idm
    uid: <kanidm-uid>
  source:
    local:
      fileName: backup.json.gz
  restoreImage: kanidm/server:1.10.0
```

The controller validates the request, marks the target in maintenance, scales all Kanidm pods down, runs `kanidmd database restore`, verifies the database offline, discards stale secondary PVC data, starts the restored primary, rebuilds replicas through normal Kanidm replication, and only then resumes ordinary Kaniop reconciliation. A failure after database mutation is fail-closed: the restore remains `Failed` and the target remains marked as restoring.

Restoring a historical database is followed by GitOps reconciliation. Declaratively managed Kaniop resources can therefore be recreated or changed after recovery.

## S3 and infrastructure snapshots

Kaniop does not implement a separate S3 uploader. Native S3-compatible shipping remains deferred until Kanidm exposes its supported upstream interface. CSI/Velero snapshots can be used as an independent disaster-recovery layer, but they are not represented as equivalent to a Kanidm-native logical backup.

## Operations Runbook

### Pre-restore checklist

Before creating a `KanidmRestore`:

1. **Verify the target UID** matches the running Kanidm CR:
   ```bash
   kubectl get kanidm <name> -o jsonpath='{.metadata.uid}'
   ```

2. **List available local backups** from the primary pod:
   ```bash
   kubectl exec -n <namespace> <kanidm-primary-pod> -- ls -la /data/backups/
   ```

3. **Confirm the pinned image** matches the backup's Kanidm version. The restore image must be an exact digest or tag (no `latest`).

4. **Ensure no other active restore** targets the same Kanidm:
   ```bash
   kubectl get kanidmrestore -n <namespace>
   ```

5. **Verify PVC storage** is persistent (not `emptyDir`) and the Kanidm has exactly one primary replica group.

### Performing a local restore

```bash
# 1. Get target UID
KANIDM_UID=$(kubectl get kanidm my-idm -o jsonpath='{.metadata.uid}')

# 2. Apply the restore
kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: KanidmRestore
metadata:
  name: my-idm-restore
  namespace: default
spec:
  targetRef:
    name: my-idm
    uid: ${KANIDM_UID}
  source:
    local:
      fileName: backup.json.gz
  restoreImage: kanidm/server:1.10.0
EOF

# 3. Monitor progress
kubectl get kanidmrestore my-idm-restore -w
kubectl describe kanidmrestore my-idm-restore
```

### Post-restore verification

After the restore reaches `Completed`:

1. Confirm all Kanidm pods are running and ready:
   ```bash
   kubectl get pods -l app.kubernetes.io/instance=<kanidm-name>
   ```

2. Verify Kanidm is serving authentication:
   ```bash
   kubectl exec -n <namespace> <kanidm-primary-pod> -- kanidmd verify
   ```

3. Check that Kaniop reconciliation has resumed:
   ```bash
   kubectl get kanidmpersonaccount,kanidmgroup,kanidmoauth2client,kanidmserviceaccount -n <namespace>
   ```

4. Verify the maintenance annotation has been removed:
   ```bash
   kubectl get kanidm <name> -o jsonpath='{.metadata.annotations}'
   ```

### Failure recovery

#### Restore fails before database mutation

If the restore fails during validation, quiesce, or preflight checks, the controller automatically restores the original replica counts and removes the maintenance annotation. No manual intervention is required.

#### Restore fails after database mutation

If the restore fails after `databaseMutationStarted` becomes true, the Kanidm target remains offline and marked as restoring. This is a fail-closed state by design.

Recovery steps:

1. Diagnose the failure from the restore status and events:
   ```bash
   kubectl describe kanidmrestore <name>
   kubectl get events -n <namespace> --field-selector involvedObject.name=<name>
   ```

2. If the database is corrupted beyond automatic recovery, create a new restore from a known-good backup.

3. As a last resort, delete the `KanidmRestore` and recreate the Kanidm CR from scratch. The finalizer prevents deletion until the database is verified and service recovery is resolved.

#### Operator restart during restore

The controller persists phase state. After an operator restart, reconciliation resumes from the last persisted phase. No manual intervention is required unless the restore was in a transient Job phase—check that the Jobs complete successfully.

### Prometheus alerts

When `metrics.prometheusRules.enabled` is set to `true`, the following backup/restore alerts are available:

| Alert | Severity | Condition | Action |
|---|---|---|---|
| `KaniopBackupStale` | critical | Backup age exceeds 24h | Verify backup schedule and Kanidm primary health |
| `KaniopBackupFailures` | warning | Multiple backup failures in 15m | Check Kanidm logs and PVC space |
| `KaniopRestoreFailures` | critical | Restore operation failed | Check restore status and events |
| `KaniopRestoreStuck` | critical | Restore running > 1 hour | Check Jobs and operator logs |
| `KaniopBackupRepositoryNotReady` | warning | Repository not ready for 5m | Verify credentials and endpoint |
| `KaniopBackupDiscoveryStale` | warning | Discovery not succeeded recently | Check repository connectivity |
| `KaniopRestoreBreakGlassUsed` | critical | Break-glass override used | Review audit log for authorization |
| `KaniopBackupGCDeferred` | warning | GC deferred for 30m | Check Object Lock or retention policy |

Individual alerts can be disabled or overridden via `metrics.prometheusRules.overrides`:

```yaml
metrics:
  prometheusRules:
    enabled: true
    overrides:
      KaniopBackupStale:
        disabled: true
```

## Orphan Cleanup

When you delete a `KanidmBackupSchedule` or `KanidmBackupRepository`, the associated `KanidmBackup` CRs are not automatically deleted. These become orphaned resources that reference non-existent schedules or repositories.

**Important:** Kaniop does not automatically delete orphaned `KanidmBackup` CRs or their associated S3 data. This is a safety measure to prevent accidental data loss.

### Manual Orphan Cleanup

To safely clean up orphaned `KanidmBackup` CRs, use a selector-based approach to ensure you only delete the intended resources:

```bash
# List orphaned backups using jsonpath (no jq required)
kubectl get kanidmbackup -n <namespace> \
  -o jsonpath='{range .items[?(@.spec.repositoryRef.name=="<deleted-repo-name>")]}{.metadata.name}{"\n"}{end}'

# Delete specific orphaned backups by name
kubectl delete kanidmbackup <backup-name> -n <namespace>

# Or delete all orphaned backups for a specific repository (USE WITH CAUTION)
kubectl delete kanidmbackup -n <namespace> -l "kaniop.rs/repository=<deleted-repo-name>"
```

### Cleaning Up Orphan Pods

Succeeded pods from backup-discover Jobs may accumulate if Job TTL has not yet expired. To clean up only Succeeded orphan pods safely:

```bash
# List Succeeded orphan pods from backup-discover Jobs
kubectl get pods -n <namespace> \
  -l kaniop.rs/operation=discover \
  --field-selector=status.phase=Succeeded

# Delete Succeeded orphan pods from backup-discover Jobs
kubectl delete pods -n <namespace> \
  -l kaniop.rs/operation=discover \
  --field-selector=status.phase=Succeeded
```

**Safety considerations:**

- Always list orphaned backups before deleting to verify you are targeting the correct resources.
- Orphaned backups cannot be used for restore operations because their `repositoryRef` no longer exists.
- Remote S3 data (payloads and manifests) is not deleted when you delete `KanidmBackup` CRs. To clean up S3 data, you must manually delete the objects from the S3 bucket.
- If you need to retain access to old backups, keep the `KanidmBackupRepository` or document its configuration for potential recreation.
- Do not use blanket deletion commands like `kubectl delete kanidmbackup --all` without first verifying that all backups are truly orphaned and no longer needed.
