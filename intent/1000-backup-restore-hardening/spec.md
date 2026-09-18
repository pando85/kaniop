# Specification: Backup/restore production hardening

Derived from: `intent.md` (draft)

## Required behavior

1. **A1 -- Local restore path**: local-source restores resolve backup files under
   `/data/backups/<fileName>`. The `BACKUP_PATH` constant is `/data/backups`.
   `safe_basename` continues to reject path separators.

2. **A2 -- Safety backup validation**: `validate_safety_backup_config` requires
   `safetyBackup.repositoryRef` whenever the safety backup is not explicitly
   skipped, for any source type. A local restore without safety config fails at
   `Validating` (pre-quiesce).

3. **A3 -- Post-mutation deletion safety**: deletion of a `KanidmRestore` with
   `databaseMutationStarted == true` and a non-`Completed` phase must NOT clear
   the target lock annotation unless the annotation
   `restore.kaniop.rs/force-release` is present on the CR. Without it: emit a
   Warning event, set a condition, keep the lock, and requeue.

4. **A4 -- Safety-backup ID from UID**: `compute_safety_backup_id` derives from
   the restore **UID**, not name, so name reuse cannot overwrite a prior safety
   backup payload.

5. **A5 -- Deletion deferral observability**: when the deletion Job fails with
   Object-Lock or access-denied errors, the controller classifies the failure,
   sets a `DeletionDeferred` condition, emits a Warning event, increments the
   `kaniop_backup_gc_deferred_total` counter, and requeues with backoff.

6. **KEK preflight**: the repository controller sets `EncryptionKeyReady` based
   on Secret/key metadata presence (never reading the KEK value). Transport
   sidecar injection is gated on `EncryptionKeyReady == True`; a missing KEK
   does not brick Kanidm pod startup.

7. **Alerts**: `KaniopBackupGCDeferred` and `KaniopBackupRepositoryNotReady`
   alerts are defined in `prometheusrules.yaml`, referencing shipped metrics.
   `KaniopBackupStale` and `KaniopBackupFailures` are documented as planned, not
   shipped.

8. **Retention property tests**: `libs/backup-core/src/retention.rs` includes
   property-based tests covering protected-entry preservation, partition
   totality/disjointness, keep-last, daily/weekly/monthly bucketing, and
   determinism.

## Acceptance criteria

- All five correctness bugs (A1-A5) have failing-then-passing unit or e2e tests.
- `kaniop_backup_gc_deferred_total` increments for `object_lock`, `access_denied`,
  and `active_restore` reasons.
- `kaniop_backup_repository_not_ready` is 1 when repository is not
  Accepted/Ready or `EncryptionKeyReady=False`; 0 otherwise.
- Transport sidecar is not injected when `EncryptionKeyReady` is False.
- `restore.kaniop.rs/force-release` annotation is the only path to clear the
  target lock after post-mutation failure.
- Helm unittest validates alert expressions reference existing metrics.
- E2e shards cover: corrupt remote payload, operator restart during mutation,
  post-mutation failure with force-release, Object-Lock deferral, active-restore
  deferral, remote HA round-trip.

## Design

### Deletion deferral flow (A5)

```
deletion Job completes with failures
  -> read termination message from FAILED pods
  -> classify via FailedKey::is_object_lock() / is_access_denied()
  -> set DeletionDeferred condition with classified reason
  -> emit Warning event
  -> increment kaniop_backup_gc_deferred_total{namespace, reason}
  -> requeue with exponential backoff
```

### KEK preflight flow

```
repository reconcile
  -> if spec.encryption.clientSide configured:
       -> check Secret exists and key present (metadata only)
       -> set EncryptionKeyReady condition (KeyPresent / MissingSecret / MissingKey)
  -> else:
       -> EncryptionKeyReady = True (not applicable)

transport reconcile
  -> if !is_encryption_key_ready(repository):
       -> skip sidecar injection
       -> surface via condition on Kanidm CR
```

### Post-mutation deletion safety (A3)

```
KanidmRestore deleted
  -> if databaseMutationStarted && phase != Completed:
       -> if force-release annotation present:
            -> clear target lock, proceed with cleanup
       -> else:
            -> emit Warning event
            -> set condition
            -> requeue (do NOT clear lock)
```

## Interfaces and data

### Conditions

| Resource | Type | Reasons | Status |
|---|---|---|---|
| `KanidmBackup` | `DeletionDeferred` | `ActiveRestoreReference`, `ObjectLockRetention`, `AccessDenied` | True when deletion is deferred |
| `KanidmBackupRepository` | `EncryptionKeyReady` | `KeyPresent`, `MissingSecret`, `MissingKey` | True when KEK is available or not required |

### Metrics

| Registered name | Exported as | Type | Labels |
|---|---|---|---|
| `backup_gc_deferred` | `kaniop_backup_gc_deferred_total` | Counter | `namespace`, `reason` |
| `backup_repository_not_ready` | `kaniop_backup_repository_not_ready` | Gauge | `namespace`, `name` |

Reason values for `backup_gc_deferred`: `active_restore`, `object_lock`, `access_denied`.

### Annotations

| Annotation | Resource | Purpose |
|---|---|---|
| `restore.kaniop.rs/force-release` | `KanidmRestore` | Allow deletion of post-mutation Failed restore to clear target lock |

### Alerts

| Alert | Expression | Severity |
|---|---|---|
| `KaniopBackupGCDeferred` | `increase(kaniop_backup_gc_deferred_total[30m]) > 0` for 5m | warning |
| `KaniopBackupRepositoryNotReady` | `kaniop_backup_repository_not_ready == 1` for 5m | warning |

## Security and privacy

- KEK value is **never** read by controllers. Only metadata (Secret existence,
  key presence) is checked. Data-mover pods remain the sole KEK consumers.
- `safe_basename` rejects path separators to prevent path traversal in restore
  source resolution.
- Force-release annotation is an explicit opt-in; without it, the target lock is
  preserved to prevent silent resume on unverified data.

## Kubernetes, upgrade, and compatibility considerations

- All changes are additive on existing v1alpha1/v1beta1 CRDs.
- No storage-version migration required.
- `KanidmBackup.spec` remains immutable; new state is in status conditions.
- New conditions and metrics are backward-compatible (additive only).
- No changes to webhook validation schema that would reject existing CRs.

## Operational behavior and rollback

- Operators can monitor `kaniop_backup_gc_deferred_total` for Object-Lock or
  access-denied deferrals and alert on them.
- Operators can monitor `kaniop_backup_repository_not_ready` for KEK or
  repository readiness issues.
- To force-clear a post-mutation restore lock: annotate the `KanidmRestore` CR
  with `restore.kaniop.rs/force-release` and delete it.
- Rollback: revert the branch. All changes are additive and behind existing
  experimental gating. No data migration to undo.

## Concerns and unresolved questions

- Object-Lock e2e depends on MinIO version supporting `--with-lock` and
  GOVERNANCE retention. COMPLIANCE mode is not used because it would block test
  cleanup.
- Post-mutation failure e2e uses deterministic Job-name pre-creation to inject
  actual restore-Job and verify-Job failures after `databaseMutationStarted`.
  The controller records `restore_job_name` and `verify_job_name` into status
  on the failure path (not only on Complete), enabling e2e assertions on the
  recorded job names.
