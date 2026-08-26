# Backup and Restore

Kaniop uses Kanidm's native logical backup format. The operator configures the online backup scheduler on exactly one primary node and stores local artifacts under `/data/backups` on the Kanidm PVC.

```yaml
spec:
  backup:
    schedule: "0 2 * * *"
    versions: 7
```

Local backups require PVC-backed storage and one `replicaGroup` with `primaryNode: true`. Kaniop intentionally does not claim PITR semantics or a globally atomic point-in-time cut across replicated writable nodes.

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
