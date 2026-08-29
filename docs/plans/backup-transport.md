# Backup Transport Sidecar

Status: **Implemented**

Related:

- [Kaniop issue #979](https://github.com/pando85/kaniop/issues/979) — local backups never discovered/uploaded
- [Backup and Restore ADR](../adr/0001-production-kanidm-backup-and-restore.md) — transport deferred as "experimental sidecar"
- [Original backup plan](./production-kanidm-backup-and-restore.md)

## Summary

Kanidm (via its native `[online_backup]` stanza) already writes local backups to `/data/backups` on the primary pod, and the discovery controller already reconciles S3 manifest keys into `KanidmBackup` CRs. The missing middle step is the **transport**: nothing moves completed local backup files from the PVC to S3. Issue #979 is the direct consequence: files accumulate locally, discovery finds zero manifests, no `KanidmBackup` CRs exist, retention never runs.

This plan implements the transport as the sidecar the ADR already anticipated, plus discovery-loop observability fixes so the system no longer appears "stuck" (the 900s staleness gate combined with debug-only logging is what made the issue look like a dead controller).

## Goals

- Upload every completed Kanidm online backup from the primary pod to the `KanidmBackupRepository` S3 bucket, exactly once, with the same manifest format the discovery controller already consumes.
- Idempotent and restart-safe: a sidecar restart uploads only what is missing; duplicate attempts degrade to a no-op.
- Never upload a partially-written file (two-scan stability + age threshold + immutable conditional manifest commit).
- Never delete local files: Kanidm's `versions = N` owns local retention.
- Run only on the designated primary pod; other pods in the primary replica group run an idle sidecar.
- Surface scan activity clearly (info logs, `lastScanTime` updated even when the staleness gate skips Job creation).
- Full e2e coverage against the existing MinIO fixture.

## Non-goals

- Dynamic primary failover detection (primary remains statically designated as today).
- Pruning local backups (Kanidm `versions` owns that).
- Native S3 shipping inside Kanidm (upstream feature; when it lands, this sidecar becomes superfluous).
- CRD schema changes (none are needed).

## Architecture

```
Kanidm primary pod                          S3 bucket
┌───────────────────────────────┐
│ kanidm ──writes──> /data/backups/backup-*.json.gz
│ data-mover transport ──reads──> completed files
│      │ conditional PUT manifest.json + PUT payload
└──────┼────────────────────────┘
       ▼
   discover Job (existing) lists manifests
       ▼
   discovery controller creates KanidmBackup CRs (existing)
       ▼
   schedule controller applies retention (existing)
```

The sidecar is injected by the Kanidm controller into the primary replica group's StatefulSet when a non-suspended `KanidmBackupSchedule` targets that Kanidm and its `KanidmBackupRepository` is Ready/Accepted. StatefulSets cannot vary containers per ordinal, so the container is present in every pod of that group and the **transport binary self-gates**: it compares `POD_NAME` (fieldRef `metadata.name`) with `KANIDM_PRIMARY_NODE` (`{sts}-{ordinal-0}`) — the same pair the init container already uses for the `[online_backup]` stanza — and idles (`sleep` loop, one info log) when they differ.

### Transport operation document (new, agreed schema)

`libs/backup-core/src/operation.rs` gains `OperationSpec::Transport(TransportOperation)` (serde tag `"transport"`):

```json
{
  "apiVersion": "backup.kaniop.rs/v1alpha1",
  "kind": "OperationDocument",
  "operation": "transport",
  "watchDir": "/data/backups",
  "filePrefix": "backup-",
  "fileSuffix": ".json.gz",
  "pollIntervalSecs": 60,
  "minFileAgeSecs": 120,
  "bucket": "...", "prefix": "...", "endpoint": "...", "region": "...",
  "forcePathStyle": false, "insecure": false, "caBundlePath": null,
  "namespaceUid": "...", "kanidmUid": "...", "kanidmName": "...",
  "domain": "...", "kanidmVersion": "...", "imageDigest": null,
  "consistency": "<see open question>", "reason": "scheduled",
  "encryptionMode": null, "encryptionKeyId": null,
  "maxConcurrentParts": 4, "maxRetries": 3
}
```

No `resultPath` (long-running process, no result document). Document validation mirrors the existing `UploadOperation` rules in `libs/backup-core/src/operation.rs`.

### Deterministic backup IDs

Kanidm names files `backup-<RFC3339 with nanoseconds>.json.gz`. The transport derives `backup_id` deterministically (UUIDv7 seeded from the embedded timestamp; fallback: file mtime) so:

- re-uploads after restart converge on the same ID (manifest conditional PUT → 412 = "already uploaded", logged at info, success);
- `KanidmBackup` CR names produced by discovery stay stable;
- no local state file is needed (durable + stateless).

### Completing-file detection (completion contract heuristic)

Per poll tick (default 60s):

1. list files matching `{filePrefix}*{fileSuffix}`;
2. skip zero-size files;
3. skip files whose mtime is younger than `minFileAgeSecs` (default 120s) **or** whose size changed since the previous tick (two-scan stability);
4. upload eligible files oldest-first.

At startup a backfill pass uploads every eligible file not already present in S3 (dedupe by listing manifests once at startup, reuse discover-style listing) — this handles the accumulated backlog from #979.

### Sidecar container

Built by `libs/operator/src/kanidm/reconcile/statefulset.rs`, appended after the kanidm container in the primary replica group's StatefulSet only, and removed when the schedule is suspended/absent. Container: `data_mover_image()`, `command: ["/bin/kaniop-data-mover"]`, `args: ["transport", "--operation-doc", "<inline JSON>"]` (shell-free, `load_operation` already accepts inline JSON), mounts `kanidm-data` at `/data` (read-only), plus CA-bundle volume when `caBundleRef` is set. Env: `POD_NAME`/`KANIDM_PRIMARY_NODE`, writer auth via the existing `build_auth_env_vars` (secret-ref or workload identity), `SSL_CERT_FILE` when applicable. Uses the hardened security contexts and default resources already defined in `libs/backup/src/controller/mod.rs` (consolidate the `data_mover_image()` duplicate in `restore.rs`).

### Kanidm controller wiring

- Reconcile path looks up the unique `KanidmBackupSchedule` (`spec.kanidmRef.name == kanidm.name`) and its `KanidmBackupRepository`; sidecar config is passed into `create_statefulset()` (new parameter/struct).
- Add watchers on `KanidmBackupSchedule` and `KanidmBackupRepository` that map back to the referenced Kanidm CRs so suspension/repository changes roll the sidecar promptly.
- Namespace UID: already fetched by the restore path (`get_namespace_uid`); reuse. `kanidmUid` is `metadata.uid`.
- Verify chart RBAC already grants get/list/watch on both CRDs to the operator SA (backup controllers run in the same process/SA); extend if missing (helm chart workflow: values + templates + unit tests).

### Discovery observability (issue #979 half of the fix)

- Update `status.discovery.lastScanTime` on every tick, including staleness-skipped ticks (reuse of `transition_time()` keeps the `Discovered` condition timestamp stable, so the staleness math is unaffected).
- Emit one info-level summary per tick covering schedules scanned / jobs created / jobs completed / manifests found; raise the "discovery is fresh; skipping Job creation" decision to info with the computed effective cadence.
- Make both the scan interval and the 900s staleness threshold operator-configurable (clap arg + `OnceLock` getter/setter in `libs/operator/src/controller/mod.rs` per the repo's operator-configuration pattern; env vars e.g. `KANIOP_BACKUP_DISCOVERY_SCAN_INTERVAL_SECS`, `KANIOP_BACKUP_DISCOVERY_STALE_SECS`). E2E lowers them to keep tests fast.

## Implementation stages

1. **libs/backup-core**: `TransportOperation` + validation + tests.
2. **cmd/data-mover**: `transport` subcommand (extract shared upload helpers from `commands/upload.rs` into a shared module rather than duplicating), backfill + poll loop, deterministic backup IDs, 412-as-success, SIGTERM graceful exit, unit tests (pure logic: file selection/stability/ID derivation).
3. **libs/operator kanidm controller**: schedule/repository lookup, watchers, sidecar builder in `statefulset.rs`, consolidate `data_mover_image()`, unit tests (sidecar present/absent/env/mounts/args for primary group + absent for others).
4. **libs/backup discovery**: per-tick `lastScanTime`, info summary logs, configurable interval/staleness knobs (with `cmd/operator` args).
5. **Chart**: verify/extend RBAC; if a new values knob is added, follow values/schema/templates/unittest workflow.
6. **Examples + docs**: `cmd/examples` concrete transport-ready schedule/repository examples (`make examples`); update `Documentation/src/usage/backup-restore.md` (remove "does not implement a separate S3 uploader", document sidecar, cadence semantics, env knobs); ADR-0001 note that the experimental transport is implemented.
7. **E2E** (`tests/e2e/test/kanidm/backup_transport.rs`, `kanidm-data` shard, `#[serial(backup)]`): create Kanidm w/ short backup cron + MinIO repo + schedule → wait for sidecar upload → assert a `KanidmBackup` CR is created by discovery with a valid manifest key; assert sidecar container exists only in primary-group STS; assert `lastScanTime` advances; cleanup per conventions. Configure low discovery interval/staleness for the suite.

## Risks / open questions

- **Manifest `consistency` value**: online backups are not the offline `kanidm-offline` safety-backup case; implementation must verify `manifest.rs` validation and the restore/download path accept a distinct value (e.g. `kanidm-online`) and pick accordingly.
- **Partial upload window**: mitigated by two-scan stability + age threshold + conditional manifest; worst case (torn payload with committed manifest) is caught by the payload checksum in the manifest at restore validation. Acceptable per ADR's "no completion contract" stance; `TransportExperimental` condition stays.
- **RWO PVC**: sidecar shares the pod volume — no extra mounts needed.

## Commit plan

Branch `feat/backup-transport`, conventional commits per stage (allowed scopes include `backup`, `operator`, `kanidm`, `crd`, `e2e`, `test`, `chart`, `helm`, `cmd`, `image` — docs go under `docs(backup)`):

1. `docs(backup): design backup transport sidecar`
2. `feat(backup): add transport operation document`
3. `feat(backup): add data-mover transport command for local backup upload`
4. `feat(operator): inject backup transport sidecar into primary Kanidm StatefulSet`
5. `fix(backup): make discovery loop observable and cadence configurable`
6. `docs(backup): document backup transport` and `feat(backup): add transport examples` as needed
7. `test(e2e): cover backup transport end to end`
