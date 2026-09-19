# Coordination contract (single source of truth for shared names)

This file is read by every implementation subagent. Do NOT invent different
names. If you believe a name here is wrong, stop and report back instead of
diverging. Delete this file is NOT allowed; it is intentionally kept out of the
final tree only if the maintainer prefers (orchestrator decides before commit).

## Scope

Implements item 1 (correctness bugs A1-A5 + cheap e2e + KEK preflight + alert
truth) and item 2 (Object-Lock fixture + DeletionDeferred e2e, operator-restart
injection at mutation-sensitive phases, restore/verify-job failure e2e, remote
2-replica HA drill, retention property tests) from the backup/restore audit.
Tracks issue #1000. Does NOT remove `TransportExperimental` (upstream-blocked).

## Shared names — metrics

Instrument names are registered WITHOUT the `kaniop_` prefix and WITHOUT `_total`
(the OpenTelemetry→Prometheus exporter in `libs/operator/src/prometheus_exporter.rs`
adds both). Registered names: `backup_gc_deferred` (counter), `backup_repository_not_ready` (gauge).

- `kaniop_backup_gc_deferred_total` (counter, labels: `namespace`, `reason`)
  - reason values: `active_restore`, `object_lock`, `access_denied`
  - Registered in `libs/backup/src/controller/backup.rs`.
- `kaniop_backup_repository_not_ready` (gauge, labels: `namespace`, `name`, value 1/0)
  - Registered in `libs/backup/src/controller/repository.rs`.
  - 1 when repository is not `Accepted`/`Ready` OR `EncryptionKeyReady=False`.

## Shared names — conditions

- `KanidmBackup` status condition type `DeletionDeferred`
  - reasons: `ActiveRestoreReference` (existing), `ObjectLockRetention`, `AccessDenied`
  - Set in `handle_deletion` when the deletion Job fails with a classified key error.
- `KanidmBackupRepository` status condition type `EncryptionKeyReady`
  - reasons: `KeyPresent` (True), `MissingSecret`, `MissingKey` (False)
  - Only checked when `spec.encryption.clientSide` is configured.
  - MUST NOT read the secret value; metadata/key-presence only.

## Shared names — alerts (charts/kaniop/files/prometheusrules.yaml)

- ADD `KaniopBackupGCDeferred` (warning): `increase(kaniop_backup_gc_deferred_total[30m]) > 0`
- ADD `KaniopBackupRepositoryNotReady` (warning): `kaniop_backup_repository_not_ready == 1` for 5m
- `KaniopBackupStale` and `KaniopBackupFailures` require RPO metrics that are
  out of scope (item 5). DO NOT add them. Docs must mark them "planned".

## Shared names — annotations

- Restore force-release (A3): `restore.kaniop.rs/force-release` on the
  `KanidmRestore` CR. When present, deletion of a post-mutation `Failed`
  restore may clear the target lock annotation; otherwise it must NOT.

## Behavior decisions (fixed)

- A1: local restore resolves files under `/data/backups/<fileName>` (where
  Kanidm writes online backups), NOT `/data/<fileName>`. Update `BACKUP_PATH`
  usage for the local-source check/restore command. Keep `safe_basename`
  (reject `/`). Update CRD doc comment + user docs + example.
- A2: `validate_safety_backup_config` must require `safetyBackup.repositoryRef`
  whenever a safety backup is required (skip != true) for ANY source, so a
  local restore without safety config fails at `Validating` (pre-quiesce), not
  as a post-quiesce deadlock.
- A3: deletion of a post-mutation (`databaseMutationStarted == true`)
  non-`Completed` restore must NOT clear the target `RESTORE_ANNOTATION` unless
  the force-release annotation is present. Without it: emit Warning event, set a
  condition, keep the lock, and requeue (do not silently resume unverified DB).
- A4: `compute_safety_backup_id` derives from the restore **UID** (not name) so
  reusing a restore name cannot overwrite a prior safety backup payload. Update
  the `safety_backup_id_survives_restart` unit test accordingly.
- A5: `handle_deletion` must read the deletion Job result from FAILED pods too
  (termination message), classify `object_lock`/`access_denied`, set
  `DeletionDeferred`, emit event, increment `kaniop_backup_gc_deferred_total`,
  and requeue with backoff instead of an unconditional 30s loop.
- KEK preflight: repository controller sets `EncryptionKeyReady`; transport
  sidecar injection in `libs/operator/src/kanidm/reconcile/transport.rs` is
  gated so a missing KEK Secret does NOT brick Kanidm pod startup (do not inject
  the sidecar; surface via schedule/repository condition + alert).

## Verification commands each agent must run for its own scope

- `make lint` (zero warnings) — or at minimum `cargo clippy -p <crate> --all-targets`
- `cargo test -p <crate>` for unit tests touched
- e2e agents: `cargo check -p kaniop-e2e-tests --features e2e-test` (compile only;
  do NOT run e2e — no cluster). Then `make check-e2e-shards`.

## Hard rules

- Never hand-edit `charts/kaniop/crds/crds.yaml` or `examples/`; run `make crdgen` / `make examples`.
- Never enable `integration-test` and `e2e-test` together.
- Imports at module scope, grouped std / external / internal / local.
- Behavioral changes require tests. No `#[allow]` to silence clippy.
- Keep the smallest correct diff.
