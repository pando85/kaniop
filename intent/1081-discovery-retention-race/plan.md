# Implementation plan: Phantom backup prevention and absent-manifest classification

Derived from: `spec.md` (draft)

## Repository findings

- The Prometheus exporter in `libs/operator/src/prometheus_exporter.rs` adds the `kaniop_` prefix
  and the `_total` suffix; instruments must be registered as `backup_discovery_dangling_pruned`.
- `libs/backup-core/src/result.rs` owns `ResultDocument`, `ExitCode`, and the shared error-code
  strings; `DiscoverResult.manifest_keys`/`truncated` are already part of the document, so the
  deletion gate needs no new payload.
- `cmd/data-mover/src/commands/download.rs` already writes result documents through
  `write_result`, but returned `ExitCode::Retryable` without a document for every manifest fetch
  error. `s3::error::S3Error::HttpFailWithBody(status, body)` carries what is needed to detect
  `404`/`NoSuchKey`.
- `libs/backup/src/controller/backup.rs`: `reconcile_cleanup` removes the finalizer as soon as
  `phase == Deleted`; `handle_discovering` had two non-success arms (failed Job, and succeeded Job
  whose document is not a success) that each parked `Invalid` with `reason: ValidationFailed`.
  `read_failed_job_result` and `select_failed_pod` already exist for reading a failed pod's result.
- `libs/backup/src/controller/discovery.rs`: `reconcile_discovered_backups` already lists every
  `KanidmBackup` in the namespace and builds `existing_manifest_keys` from it -- the guard the race
  defeats -- and `read_discover_result` reads a completed discover Job's document.
- The `kaniop.rs/discovered` label was a string literal in `build_backup_cr` only.
- `DISCOVERED_LABEL`-style shared constants live in `libs/backup/src/controller/mod.rs`.
- E2e backup tests are declared with `e2e_test!` in `tests/e2e/test/kanidm/backup.rs` and run in
  the `kanidm-backup` shard (`.github/workflows/rust.yml`).
- The chart's ClusterRole already grants everything the new code calls (`kaniop.rs` resources
  `*/*`, `batch/jobs` `*/*`, `pods/log` `get`, `events` `create`).

## Files and components

- `libs/backup-core/src/result.rs` -- `MANIFEST_NOT_FOUND` shared constant.
- `cmd/data-mover/src/commands/download.rs` -- fetch-error classification, result documents for
  both outcomes, `MANIFEST_FETCH_ERROR` local constant.
- `libs/backup/src/controller/mod.rs` -- `DISCOVERED_LABEL`.
- `libs/backup/src/controller/backup.rs` -- `DISCOVERY_CONFIRMATION_GRACE`, Job-timestamp helpers,
  `discover_job_may_reference`, `conflicting_discover_job`, `discovery_confirmation_pending`,
  `ValidationFailure`/`classify_validation_failure`, `is_discovery_owned`, `list_discover_jobs`,
  `mark_backup_invalid`, `handle_manifest_absent`, `discard_phantom_backup`, gated
  `reconcile_cleanup`.
- `libs/backup/src/controller/discovery.rs` -- `dangling_discovered_backups`, prune loop and count
  threaded out of `reconcile_discovered_backups`, `backup_discovery_dangling_pruned` counter,
  `Discovered` condition message, `read_discover_result` widened to `pub(crate)`.
- `Documentation/src/usage/backup-restore.md` -- stale/missing-manifest policy, bounded
  `Terminating` wait, alert-table and metric wording.
- `Documentation/src/troubleshooting.md` -- symptoms for accumulating `Invalid` discovered records
  and for a deletion held `Terminating`.
- `intent/1081-discovery-retention-race/` -- this artifact chain.
- `evals/cases/discovery-retention-resurrection/` -- eval case for the escaped defect.
- `tests/e2e/test/kanidm/backup.rs` -- owned by the e2e agent, not by this slice: the retention
  delete test waits through the bounded finalizer gate and asserts the surviving catalogue.

## Ordered implementation steps

1. **Error code contract**: add `MANIFEST_NOT_FOUND` to `libs/backup-core/src/result.rs` with a
   round-trip test.
2. **Data mover classification**: in `download_and_verify_manifest`, classify
   `S3Error::HttpFailWithBody` as `InvalidInput`/`MANIFEST_NOT_FOUND` only when the status is 404
   **and** the error document's `<Code>` is exactly `NoSuchKey`; everything else, including
   `NoSuchBucket`, a bare or unrecognized 404 body, 403, 5xx, transport errors, and `NoSuchKey`
   under a non-404 status, is `Retryable`/`MANIFEST_FETCH_ERROR`. Classify a non-200 response the
   same way from its body, and always write a failure result document through one
   `report_manifest_failure` helper that still returns the exit code if the write fails.
3. **Shared label constant**: add `DISCOVERED_LABEL` and use it in `build_backup_cr`.
4. **Deletion gate**: in `reconcile_cleanup`, when `phase == Deleted`, read `deletedAt` from the
   `Ready`/`Deleted` condition, list discover Jobs for `spec.repositoryRef.name`, keep only Jobs
   started before `deletedAt` (`status.startTime` else `creationTimestamp`), read each completed
   Job's result, and hold (patch status, requeue `REQUEUE_DELETION`) while a Job may still
   reference `spec.manifestKey` and `now - deletedAt <= DISCOVERY_CONFIRMATION_GRACE`. Past the
   grace, warn and remove the finalizer. Jobs on the repository-gone and prefix-escape paths have
   no `Ready`/`Deleted` condition and are therefore never held.
5. **Classification in the operator**: add `ValidationFailure` + `classify_validation_failure`; read
   the failed validation Job's document in the failed-Job arm, and in the succeeded-but-non-success
   arm delete the Job first so a classified absent manifest cannot leave a stranded Job, then
   classify.
6. **Discard versus park**: `handle_manifest_absent` dispatches on `is_discovery_owned` ->
   `discard_phantom_backup` (Warning Event `ManifestAbsent`/`PhantomBackupDiscarded`, CR delete,
   tolerating 404, no counter) or `mark_backup_invalid` with `reason: ManifestAbsent` plus the
   counter. `mark_backup_invalid` also keeps the existing `ValidationFailed` path for `Other`, and
   the succeeded-but-non-success arm routes through it with its existing `ManifestInvalid` reason
   and message, which is a pure substitution: same phase, same `Ready=False` condition fields, same
   status patch, same `REQUEUE_NORMAL` action, and no counter either way.
7. **Pruning**: add `dangling_discovered_backups` and call it from `reconcile_discovered_backups`
   only under `snapshot_supports_pruning` (`!discovery.truncated` **and**
   `discovery.manifest_keys.len() == discovery.total_found as usize`); count successes, ignore 404,
   warn on other errors, return the count, emit `backup_discovery_dangling_pruned` when non-zero,
   and extend the `Discovered` condition message and the `discover result processed` log. The
   message extension is best-effort only: issue #1082 makes the second discovery status apply of
   the same tick revert `Discovered`, so the counter and the log line are the durable signals and
   nothing (docs or e2e) may depend on the message persisting.
8. **Unit tests** for each branch listed in `spec.md` acceptance criteria, including the grace
   boundary and the grace-above-Job-TTL invariant.
9. **Documentation**: `Documentation/src/usage/backup-restore.md` (new stale/missing-manifest
   subsection under the transport/discovery area, GC-and-deferral section, alert table, metric
   mention) and `Documentation/src/troubleshooting.md` (two new entries).
10. **Eval case**: add `evals/cases/discovery-retention-resurrection/{case.json,task.md}` from the
   template, validated by `make agent-framework-check`.

## Dependencies and coordination

- Shared names are fixed here: `ManifestAbsent` (condition reason and Event reason),
  `PhantomBackupDiscarded` (Event action), `MANIFEST_NOT_FOUND`/`MANIFEST_FETCH_ERROR` (result
  document codes), `kaniop.rs/discovered`, `backup_discovery_dangling_pruned`. The e2e agent asserts
  against these exact strings, but only where they are durable: the prune scenario observes the
  dangling `KanidmBackup` CR being deleted and never the `Discovered` condition message, which
  issue #1082 makes transient within a single scan tick.
- The e2e slice in `tests/e2e/test/kanidm/backup.rs` is owned by another agent; this plan does not
  touch `tests/**`, `charts/**`, or `libs/**` beyond the files already listed as implemented.
- No CRD field is added, so `make crdgen` and `make examples` must stay clean and no
  `cmd/crdgen`/`crd.rs` edit is needed.
- No chart change is needed: the RBAC the new code uses was already granted before this change.

## Risks and blast radius

- **Deletion liveness**: holding the finalizer is the only new blocking behavior. It is bounded by
  the grace, keyed on a `Ready`/`Deleted` condition that only a completed S3 cleanup sets, and
  covered by unit tests at both sides of the boundary.
- **Wrongly discarding a user record**: both the discard and the prune require
  `kaniop.rs/discovered=true`, which only `build_backup_cr` sets. A hand-authored record can never
  be auto-deleted by this change.
- **Pruning on a partial listing**: blocked by `snapshot_supports_pruning`, which requires
  `truncated == false` **and** that the snapshot's key list matches its reported total, plus the
  UID, phase, and `deletionTimestamp` conditions. `cmd/data-mover/src/commands/listing.rs` stops at
  `MAX_PAGES = 100` without setting `truncated`, so the self-consistency check is the guard that
  keeps a future raise of `maxResults` above that cap from turning a partial listing into deletes.
- **Provider variance in "not found"**: absence is concluded only from HTTP 404 with an explicit
  `NoSuchKey` code. A gateway that reports a missing key with another status, a `NoSuchBucket` 404,
  or a bare 404 stays `MANIFEST_FETCH_ERROR`/`Retryable`, which is the pre-change behavior: the
  record parks `Invalid` and is cleaned up by a later complete scan instead of being destroyed on
  an ambiguous signal.
- **Clock domains in the deletion gate**: `deletedAt` is the operator clock
  (`Timestamp::now()` when the deletion Job completion is observed) while a discover Job's
  `startTime` is the cluster clock. Skew larger than the delete-to-observe latency can let a
  genuinely stale Job be judged as started after the deletion, so the gate does not hold. The
  window is narrow, and the phantom-discard and prune paths still contain the outcome.
- **E2e cannot run locally** (needs Kind + MinIO): CI's `kanidm-backup` shard is the proof; local
  verification is compile-only.

## Alternatives rejected

- **Delete-then-ignore in discovery** (suppress recreation by remembering deleted keys): rejected --
  requires new durable state across controller restarts, and a tombstone TTL re-creates the same
  race class.
- **Always requeue the phantom delete without holding the finalizer**: rejected -- the recreated CR
  would still be validated and counted, and the deletion Job would run again against the same
  prefix.
- **Park the phantom `Invalid` and prune only on the next scan**: rejected as the primary path --
  it keeps the noisy alert and one full scan interval of a lying catalogue; the prune remains as the
  cleanup for records parked before this fix.
- **Shortening the deletion hold to one scan interval**: rejected -- the tick is configurable, so a
  constant is not a bound; a fixed grace plus the Job-TTL invariant is.
- **Classifying by parsing the Job termination message string**: rejected -- the result document is
  the existing typed contract and is testable without an S3 harness.

## Verification

| Command | Expected result |
|---|---|
| `make lint` | zero warnings |
| `cargo test --workspace` (via `make test`) | new download, backup, discovery, and result-document tests pass |
| `make agent-framework-check` | passes, including the new eval case |
| `make crdgen` | no drift |
| `make examples` | no drift |
| `make check-e2e-shards` | backup e2e tests counted in the `kanidm-backup` shard |
| CI `e2e` job, shard `kanidm-backup` | retention-delete scenario passes through the finalizer gate |

## Rollback

Revert the branch. No CRD, storage-version, or generated-file change exists to unwind, and no new
state is persisted. Rolling back restores the previous behavior: stale scans recreate records,
absent manifests park `Invalid` and increment the counter, and already-parked records are no longer
pruned.

## Deviations

- **E2e scope**: the e2e slice adapts the existing
  `backup_schedule_retention_keep_last_deletes_expired_cr_and_s3_prefix` test to wait through the
  bounded finalizer gate (`poll_until_with_timeout`, 180s, sized to the e2e discovery cadence) and
  keeps its surviving-catalogue assertion, rather than adding a dedicated stale-scan resurrection
  test. The gate's decision logic -- Job timestamps, completion, snapshot contents, unreadable
  results, and the grace boundary -- is covered by the unit tests listed in `spec.md`. A
  deterministic cluster-level resurrection scenario would need control over discover Job timing and
  is recorded as a follow-up proof gap rather than claimed here. The e2e slice is owned by another
  agent and can still change; this record describes the branch when this artifact was written.
