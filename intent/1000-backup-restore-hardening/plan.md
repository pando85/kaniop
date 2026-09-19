# Implementation plan: Backup/restore production hardening

Derived from: `spec.md` (draft)

## Repository findings

- The Prometheus exporter in `libs/operator/src/prometheus_exporter.rs` adds the
  `kaniop_` prefix and `_total` suffix. Instruments must be registered WITHOUT
  these (e.g., `backup_gc_deferred`, not `kaniop_backup_gc_deferred_total`).
- `libs/backup/src/controller/backup.rs` owns backup GC and deletion logic.
- `libs/backup/src/controller/repository.rs` owns repository reconciliation and
  is the correct location for `EncryptionKeyReady`.
- `libs/operator/src/kanidm/reconcile/transport.rs` owns sidecar injection.
- `libs/operator/src/kanidm/restore/controller.rs` owns restore lifecycle
  including deletion hooks.
- `libs/operator/src/kanidm/restore/legacy.rs` owns local restore path
  resolution and safety-backup validation.
- `libs/backup-core/src/result.rs` owns `DeletionResult` and `FailedKey` types.
- `libs/backup-core/src/retention.rs` owns retention policy logic.
- E2e tests live in `tests/e2e/test/kanidm/{backup,restore,mod}.rs`.
- Alerts live in `charts/kaniop/files/prometheusrules.yaml` with tests in
  `charts/kaniop/tests/prometheusrules_test.yaml`.

## Files and components

**Core correctness (Wave 1)**:
- `libs/operator/src/kanidm/restore/legacy.rs` -- A1 (BACKUP_PATH), A2 (safety validation)
- `libs/operator/src/kanidm/restore/controller.rs` -- A3 (force-release), A4 (UID-based safety ID)
- `libs/backup-core/src/result.rs` -- A5 (FailedKey classification, GcDeferReason)
- `libs/backup/src/controller/backup.rs` -- A5 (DeletionDeferred condition, metrics, backoff)
- `libs/backup/src/controller/repository.rs` -- KEK preflight (EncryptionKeyReady)
- `libs/backup/src/controller/mod.rs` -- shared metric helpers
- `libs/operator/src/kanidm/reconcile/transport.rs` -- KEK gate on sidecar injection

**Property tests (Wave 1)**:
- `libs/backup-core/src/retention.rs` -- property-based retention tests
- `libs/backup-core/Cargo.toml` -- proptest dependency

**Charts and alerts (Wave 2)**:
- `charts/kaniop/files/prometheusrules.yaml` -- KaniopBackupGCDeferred, KaniopBackupRepositoryNotReady
- `charts/kaniop/tests/prometheusrules_test.yaml` -- alert tests

**E2e tests (Wave 2)**:
- `tests/e2e/test/kanidm/restore.rs` -- corrupt payload, restart-at-mutation, post-mutation failure
- `tests/e2e/test/kanidm/backup.rs` -- Object-Lock deferral, active-restore deferral
- `tests/e2e/test/kanidm/mod.rs` -- remote HA drill, MinIO helpers (lock config, scoped deleter, governance retention)
- `tests/e2e/scripts/setup-minio.sh` -- MinIO lock-enabled setup

**Data-mover tests**:
- `cmd/data-mover/src/commands/delete_plan.rs` -- classification unit tests

**Documentation**:
- `Documentation/src/usage/backup-restore.md` -- A1 path fix, force-release annotation
- `Documentation/src/troubleshooting.md` -- DeletionDeferred, EncryptionKeyReady
- `docs/adr/0001-production-kanidm-backup-and-restore.md` -- minor cross-reference
- `docs/plans/backup-hardening.md` -- status update
- `docs/plans/backup-transport.md` -- status update

**Build**:
- `Cargo.toml`, `Cargo.lock` -- workspace dependency additions
- `Makefile` -- e2e shard filter update for `restore_remote_ha_round_trip`

**Unit tests**:
- `tests/tests/restore_hardening.rs` -- restore correctness unit tests

## Ordered implementation steps

### Wave 1: Restore correctness, deletion observability, KEK preflight, retention tests

1. **A1 -- Local restore path**: change `BACKUP_PATH` from `/data` to
   `/data/backups` in `libs/operator/src/kanidm/restore/legacy.rs`. Update
   `safe_basename` usage. Add unit test.

2. **A2 -- Safety backup validation**: require `safetyBackup.repositoryRef` in
   `validate_safety_backup_config` when skip is not set, for any source. Add
   unit test verifying pre-quiesce failure.

3. **A4 -- Safety-backup ID from UID**: change `compute_safety_backup_id` to
   derive from restore UID. Update `safety_backup_id_survives_restart` test.

4. **A3 -- Post-mutation deletion safety**: add `FORCE_RELEASE_ANNOTATION`
   constant. In deletion handler, check `databaseMutationStarted` and phase
   before clearing target lock. Emit event and condition when annotation is
   absent. Add unit tests.

5. **A5 -- Deletion deferral**: add `FailedKey::is_object_lock()`,
   `is_access_denied()`, `GcDeferReason` enum, `DeletionResult::classify_deferral()`
   in `libs/backup-core/src/result.rs`. In backup controller `handle_deletion`,
   read termination messages from FAILED pods, classify, set `DeletionDeferred`
   condition, emit event, increment `backup_gc_deferred` counter, requeue with
   backoff. Add unit tests for each reason.

6. **KEK preflight**: add `KekCheckResult` enum and `encryption_key_condition()`
   in repository controller. Set `EncryptionKeyReady` condition based on Secret
   metadata. Add `is_encryption_key_ready()` helper in transport reconcile. Gate
   sidecar injection on it. Add unit tests for all branches.

7. **Retention property tests**: add proptest-based tests in
   `libs/backup-core/src/retention.rs` covering protected-entry preservation,
   partition totality/disjointness, keep-last, daily/weekly/monthly bucketing,
   and determinism.

### Wave 2: Charts, alerts, docs, e2e

8. **Alerts**: add `KaniopBackupGCDeferred` and `KaniopBackupRepositoryNotReady`
   to `charts/kaniop/files/prometheusrules.yaml`. Add helm unittest assertions.

9. **Documentation**: update backup-restore usage guide (A1 path, force-release
   annotation), troubleshooting guide (DeletionDeferred, EncryptionKeyReady),
   ADR cross-reference, plan status files.

10. **Restore e2e**: add tests for corrupt remote payload, operator restart
    during mutation, post-mutation failure with force-release.

11. **Backup e2e + Object-Lock fixture**: add tests for Object-Lock deferral and
    active-restore deferral. Add MinIO helpers for lock-enabled S3 config, scoped
    deleter credentials, governance retention setup. Update setup-minio.sh.

12. **Remote HA drill**: add `restore_remote_ha_round_trip` e2e test. Update
    Makefile shard filter.

### Post-implementation

13. **Metric name fix**: catch and fix exporter double-prefix issue (instruments
    registered with `kaniop_` prefix would be exported as `kaniop_kaniop_...`).

14. **Clippy fixes**: address any clippy warnings without `#[allow]`.

## Dependencies and coordination

- **CONTRACT.md** pins all shared names (metrics, conditions, alerts, annotations).
  All implementation agents must read it before making naming decisions.
- **Exporter double-prefix gotcha**: the OpenTelemetry-to-Prometheus exporter adds
  `kaniop_` prefix and `_total` suffix. Register instruments as `backup_gc_deferred`
  and `backup_repository_not_ready`, not with the full exported names.
- **CRD/example regeneration**: after any CRD status field changes, run
  `make crdgen` and `make examples`. Never hand-edit generated files.

## Risks and blast radius

- **E2e cannot be run locally**: requires a Kind cluster with MinIO. CI validates
  e2e; local verification is compile-only (`cargo check -p kaniop-e2e-tests --features e2e-test`).
- **Object-Lock fixture**: depends on MinIO version supporting `--with-lock` flag
  and GOVERNANCE retention mode. COMPLIANCE mode would block test cleanup and is
  not used.
- **Restart-injection e2e timing**: the operator-restart-during-mutation test
  depends on timing the operator restart while the restore is in the mutation
  phase. The PVC-blocker approach provides deterministic blocking.
- **Post-mutation failure e2e**: uses PVC-blocker to deterministically reach a
  post-mutation Failed state rather than forcing a mid-restore-job crash.
- **Additive changes only**: all new conditions, metrics, and annotations are
  additive. No breaking changes to existing CRDs or APIs.

## Alternatives rejected

- **Silencing clippy with `#[allow]`**: rejected per repo rule. All clippy
  warnings must be fixed properly.
- **Faking Object-Lock in e2e**: rejected. The test uses real MinIO Object-Lock
  with GOVERNANCE retention and a scoped deleter credential to exercise the
  actual AccessDenied path.
- **Deriving safety-backup ID from name**: rejected (A4). Name reuse would
  overwrite prior safety payloads. UID is stable and unique.
- **Removing TransportExperimental gate**: rejected. Upstream-blocked; not in
  scope for this change.

## Verification

| Command | Expected result |
|---|---|
| `make lint` | Zero warnings |
| `cargo test --workspace` | All unit/integration tests pass (883 passed, 0 failed) |
| `helm unittest charts/kaniop` | All tests pass (204 passed) |
| `make check-e2e-shards` | 208 tests across 6 shards, all OK |
| `make crdgen` | No drift (generated files match) |
| `make examples` | No drift (generated examples match) |

## Rollback

Revert the branch `feat/backup-restore-production-hardening`. All changes are
additive and behind existing experimental gating. No data migration, no CRD
storage-version change, no webhook schema change. Safe to revert at any point.

## Deviations

(a) **Post-mutation restore/verify job-failure e2e**: uses deterministic Job-name
pre-creation to inject actual restore-Job and verify-Job failures after
`databaseMutationStarted`. The restore-Job failure test pre-creates a failing
Job with the controller's deterministic name (`{restore}-restore`); the
verify-Job failure test pre-creates a failing `{restore}-verify` Job. Both
assert `databaseMutationStarted == true`, phase `Failed`, lock retained, and
force-release required for cleanup. The PVC-blocker timeout is no longer the
sole post-mutation failure path. The controller now records `restore_job_name`
and `verify_job_name` into status on the failure path (not only on Complete),
closing an observability gap and enabling e2e assertions on the recorded job
names.

(b) **Object-Lock e2e**: exercises the AccessDenied path via a scoped deleter
credential + GOVERNANCE retention on the S3 prefix. COMPLIANCE mode is not used
because it would block test cleanup (immutable retention). ObjectLockRetention
classification is covered by unit tests in `libs/backup-core/src/result.rs` and
`libs/backup/src/controller/backup.rs`.

(c) **Phase-specific operator-restart e2e**: uses deterministic Job-name
pre-creation (blocking Jobs with `sleep 300`) to hold `RestoringPrimary` and
`Verifying` phases, and PVC-holder pods for `RebuildingReplicas`. This avoids
timing races and provides deterministic observation of each phase before
operator restart.

(d) **Versioned-delete rewrite in data-mover**: `delete_plan.rs` now resolves
version IDs via `head_object` and issues batch `delete_objects` with version
identifiers instead of per-key `delete_object`. This is a material behavior
change: on versioned buckets, versionless deletes create delete markers rather
than removing objects, so explicit version IDs are required for correct GC.
`get_version_id`, `list_all_versions_under_prefix`, and `delete_versioned_keys`
replace the former `list_all_keys_under_prefix` and `delete_key` functions.

(e) **Coverage gap for versioned S3 functions**: `get_version_id`,
`list_all_versions_under_prefix`, and `delete_versioned_keys` are exercised
only by e2e tests. No S3 mock harness exists for unit tests; the unit test
suite covers classification and fallback logic only (e.g., FailedKey reason
strings, VersionedKey presence/absence of version_id). Full integration
coverage depends on the e2e MinIO fixture.

(f) **A1 path revert to /data**: commit eced48ac reverted `BACKUP_PATH` from
`/data/backups` to `/data` because the kanidm minimal container lacks `mkdir`.
The operator, e2e tests, and `Documentation/src/usage/backup-restore.md` now
use `/data` consistently. spec.md A1 and the usage documentation have been
updated to match. The original A1 design (subdirectory isolation) is deferred
until the kanidm image provides directory creation or the operator pre-creates
the path.
