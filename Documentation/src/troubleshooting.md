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
3. As a last resort, delete the `KanidmRestore` (the finalizer blocks deletion until the database is verified) and recreate the Kanidm CR from scratch.

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
