# Specification: Phantom backup prevention and absent-manifest classification

Derived from: `intent.md` (draft)

## Required behavior

1. **Absent manifest is a distinct outcome** (`cmd/data-mover/src/commands/download.rs`,
   `libs/backup-core/src/result.rs`): a `get_object` failure is reported as a failure
   `ResultDocument` with `error.code = MANIFEST_NOT_FOUND` and `ExitCode::InvalidInput` only when the
   provider answers HTTP 404 **and** the error document carries the explicit `NoSuchKey` code, which
   is what AWS S3 and MinIO return for a deleted key. Discovery-owned records are deleted on that
   code, so anything ambiguous stays retryable: a bare or empty-body 404, `NotFound` without
   `NoSuchKey`, `NoSuchBucket`, a misrouted endpoint or path-style-versus-virtual-host mismatch, 403,
   5xx, and transport/IO errors are reported as `error.code = MANIFEST_FETCH_ERROR` with
   `ExitCode::Retryable`, which parks the record `Invalid` and leaves it to the dangling-prune path.
   Both write a result document; the classified exit code is returned even when writing the document
   fails. Previously all fetch errors exited `Retryable` with no document, so the operator could only
   report a generic message.

2. **Deletion waits for discovery to observe it** (`libs/backup/src/controller/backup.rs`,
   `reconcile_cleanup`): when `status.phase == Deleted`, the finalizer is not removed while a
   discover Job for the same repository (`kaniop.rs/repository=<repo>,kaniop.rs/operation=discover`)
   could still resurrect this backup's `manifestKey`. A discover Job can resurrect the key when it
   started -- `status.startTime`, falling back to `metadata.creationTimestamp` -- before the
   deletion completed (`lastTransitionTime` of the `Ready` condition with `reason: Deleted`) and
   either has not completed, its result document is not readable, or its
   `discovery.manifestKeys` still contains the key. The CR stays `Terminating` so discovery's
   `existing_manifest_keys` guard keeps suppressing the resurrection.

3. **The wait is bounded**: the hold applies only while
   `now - deletedAt <= DISCOVERY_CONFIRMATION_GRACE` (1800s). After that the controller logs a
   warning naming the conflicting Job and removes the finalizer, so a stuck discover Job costs
   deletion latency, never deletion liveness. The grace deliberately exceeds
   `BACKUP_JOB_TTL_SECONDS` (600s) so a Job still inside its own lifetime is always covered.
   `reconcile_cleanup` resolves the conflicting Job once with `conflicting_discover_job` and passes
   it to `discovery_confirmation_pending`, so the wait/no-wait decision and the Job named in the log
   line come from a single evaluation of the same snapshots.

4. **The gate never blocks the escape paths**: it is keyed on a `Ready`/`Deleted` condition, so a
   deletion that never performed S3 cleanup is not held. That covers `RepositoryGoneDataOrphaned`
   and the backup-prefix-escapes-repository path, which reach `Deleted` with a different (or no)
   `Ready` condition reason.

5. **Validation failures are classified** (`classify_validation_failure`): both the failed-Job arm
   and the succeeded-but-non-success arm read the validation result document and map
   `error.code == MANIFEST_NOT_FOUND` to `ManifestAbsent`; everything else, including a missing or
   error-free document, stays `Other` with the existing generic message
   `Manifest validation Job failed; manifest is unreachable or invalid`.

6. **Phantom records are discarded, not parked** (`handle_manifest_absent`): a backup carrying
   `kaniop.rs/discovered=true` is discovery-owned, so an absent manifest means the catalogue entry
   is stale. The controller emits a Warning Event (`reason: ManifestAbsent`,
   `action: PhantomBackupDiscarded`), deletes the CR, and does **not** increment
   `kaniop_backup_validation_failures_total`. The normal finalizer flow then cleans the backup
   prefix, which may still hold the payload. A 404 on the delete is treated as already done; any
   other delete error fails the reconcile.

7. **User-authored records stay visible failures**: a backup without the discovered label whose
   manifest is absent becomes `phase: Invalid` with `Ready=False`, `reason: ManifestAbsent`, a
   message naming the manifest key, and does increment
   `kaniop_backup_validation_failures_total`. Non-absent failures keep `reason: ValidationFailed`.

8. **Dangling records are pruned** (`libs/backup/src/controller/discovery.rs`,
   `dangling_discovered_backups`): on every scan whose snapshot is complete (`truncated == false`)
   **and** self-consistent (`manifest_keys.len() == total_found`, checked by
   `snapshot_supports_pruning`), discovery deletes records that are simultaneously (a) labeled
   `kaniop.rs/discovered=true`, (b) `phase: Invalid`, (c) for the scanned Kanidm UID, (d) without
   `deletionTimestamp`, and (e) absent from the snapshot. This cleans up records parked by older
   versions. Pruning counts only successes; a delete error is logged and left for the next scan.
   The self-consistency check is an invariant today -- `cmd/data-mover` derives both fields from the
   same list -- but it guards the destructive path against `listing.rs`, which stops at
   `MAX_PAGES = 100` without marking the result truncated, should `maxResults` ever be raised above
   that cap.

9. **Pruning and discovery reporting are observable**: pruning increments
   `kaniop_backup_discovery_dangling_pruned_total{repository}` once per scan with the pruned count,
   and logs `pruned dangling discovered backup`. Those two are the durable signals. The
   `, pruned N dangling` suffix added to the `Discovered` condition message when `N > 0` is
   best-effort and does **not** reliably persist while issue #1082 is open: a successful scan tick
   applies the discovery status twice from the same stale in-memory schedule, and the second apply
   (`DiscoveryTruncated`) reverts `Discovered` to its pre-tick value. Nothing on this branch may
   treat that message -- or the e2e test -- as depending on it.

## Acceptance criteria

- `MANIFEST_NOT_FOUND` round-trips through a result document and survives serialization; 403/5xx/IO
  errors keep `ExitCode::Retryable` with `MANIFEST_FETCH_ERROR`.
- Manifest absence classification tests cover: 404 with `NoSuchKey` is absent; 404 with
  `NoSuchBucket` stays retryable; 404 with an empty or unrecognized body stays retryable; `NoSuchKey`
  under a non-404 status stays retryable; 403/500/503 and transport errors stay retryable.
- `discover_job_may_reference` / `discovery_confirmation_pending` table tests cover: running Job
  started before and after the deletion, completed Job whose snapshot has and lacks the key,
  unreadable result of a completed Job, `creationTimestamp` fallback, a Job with no timestamp, no
  Jobs at all, `deletedAt` absent, and the grace boundary before/after 1800s.
- `DISCOVERY_CONFIRMATION_GRACE > BACKUP_JOB_TTL_SECONDS` is asserted.
- `classify_validation_failure` tests cover `MANIFEST_NOT_FOUND`, a non-success document carrying
  `MANIFEST_NOT_FOUND`, other codes, a document without an error, and no document.
- `is_discovery_owned` tests cover label `true`, `false`, an unrelated value, and no labels.
- `dangling_discovered_backups` tests cover: prune an `Invalid` absent discovered record; keep a
  record present in the snapshot, a `Ready`/`Discovering`/`Deleting`/`Deleted` record, a record with
  no status, a user-authored record, a record of another Kanidm UID, and a record already
  terminating.
- `snapshot_supports_pruning` tests cover: a complete self-consistent snapshot (including an empty
  one) may prune; a truncated snapshot may not; a snapshot whose key list disagrees with its
  reported total may not.
- E2e in `tests/e2e/test/kanidm/backup.rs` keeps the retention-delete scenario green through the
  new gate, and CI runs it in the `kanidm-backup` shard. The prune scenario asserts the observable
  outcome (the dangling `KanidmBackup` CR is deleted, alongside its fixture-side S3 assertions) and
  never asserts on the `Discovered` condition message, which issue #1082 makes transient.

## Design

```
discover Job vs. deletion (TOCTOU window)
  LIST begins ----[manifest still present]---- snapshot result document lands
                                                    |
  CR deleted -> finalizer -> deletion Job removes objects -> Ready{Deleted}, phase Deleted
                                                    |
  discovery consumes stale snapshot later  -> existing_manifest_keys no longer holds the key
                                              -> phantom CR recreated -> validation 404

Closures:
  (a) hold the CR (Terminating) while a pre-deletion scan can still report its key
      -> existing_manifest_keys keeps suppressing; bounded by the grace
  (b) classify a 404 manifest as absent
      -> discovered: discard; user-authored: Invalid{ManifestAbsent}
  (c) prune dangling Invalid discovered records from the next complete snapshot
```

## Interfaces and data

### Conditions

| Resource | Type | Reason | Meaning |
|---|---|---|---|
| `KanidmBackup` | `Ready` | `ManifestAbsent` (new) | `False` + `phase: Invalid` for a **user-authored** backup whose manifest object is gone. Discovered backups with the same finding are deleted instead of parked. |
| `KanidmBackup` | `Ready` | `ValidationFailed` | unchanged: any other validation failure. |
| `KanidmBackup` | `Ready` | `Deleted` | unchanged; its `lastTransitionTime` is the deletion timestamp the discovery gate is measured against. |
| `KanidmBackupSchedule` | `Discovered` | `DiscoveryComplete` | message extended with `, pruned N dangling` when `N > 0`; the extension is best-effort and may not persist (issue #1082), so use `kaniop_backup_discovery_dangling_pruned_total` or the operator log line. |

### Result document error codes (`libs/backup-core/src/result.rs`)

| Code | Exit code | Meaning |
|---|---|---|
| `MANIFEST_NOT_FOUND` (new shared constant) | `InvalidInput` | the manifest object is absent (HTTP 404 **and** an explicit `NoSuchKey` code); permanent for this backup |
| `MANIFEST_FETCH_ERROR` | `Retryable` | the manifest could not be fetched or the outcome is not definitive, including any 404 that does not say `NoSuchKey` |

### Metrics

| Registered name | Exported as | Type | Labels |
|---|---|---|---|
| `backup_discovery_dangling_pruned` | `kaniop_backup_discovery_dangling_pruned_total` | Counter | `repository` |
| `backup_validation_failures` | `kaniop_backup_validation_failures_total` | Counter | `namespace` |

`backup_validation_failures` is no longer incremented for discarded phantom records.

### Events, labels and constants

| Item | Value | Purpose |
|---|---|---|
| Event | Warning, `reason: ManifestAbsent`, `action: PhantomBackupDiscarded` | records the discard of a phantom discovered backup |
| Label | `kaniop.rs/discovered=true` (`DISCOVERED_LABEL`, `libs/backup/src/controller/mod.rs`) | discovery ownership; value unchanged, now defined once and shared |
| Constant | `DISCOVERY_CONFIRMATION_GRACE = 1800s` | upper bound on waiting for discovery to observe a deletion |

## Security and privacy

- No credential handling changes; the data mover remains the only component reading S3 objects.
- Pruning and discarding only delete Kubernetes records that discovery itself created, never
  user-authored records, and only from a complete snapshot -- a truncated listing can never justify
  a deletion.
- Event and condition messages carry manifest keys and provider error text, which contain no
  secrets.

## Kubernetes, upgrade, and compatibility considerations

- No CRD schema change: existing `phase`, `status.conditions`, and the existing label carry the new
  state, so `make crdgen` and `make examples` produce no drift.
- Additive condition reason and error code; no existing value changes meaning, and no webhook
  change.
- On upgrade, records already parked `Invalid` by the old behavior are pruned by the first
  complete scan, and phantom recreations stop immediately.
- Deletion latency increases by up to one discovery consumption cycle (normally well under the
  grace) for retention-driven deletes; behavior is unchanged when no pre-deletion scan is in
  flight.

## Operational behavior and rollback

- `KaniopBackupValidationFailures` now fires only for real validation problems, and a
  `phase: Invalid` backup with `reason: ManifestAbsent` means the operator's own record points at a
  manifest that no longer exists.
- A `KanidmBackup` stuck `Terminating` for up to 30 minutes after a retention delete is the
  discovery gate working; the operator logs the conflicting discover Job name, and after the grace
  it proceeds with a warning.
- `kaniop_backup_discovery_dangling_pruned_total` rising is normal cleanup after retention, not an
  error.
- Rollback: revert the branch. No persisted state needs migration; the rollback restores the old
  resurrection behavior (accumulating `Invalid` records and the noisy alert) but strands nothing.

## Concerns and unresolved questions

- The gate is per repository, not per backup: a stale scan holding one deletion holds nothing else
  permanently, but a discover Job that never completes delays each concurrently deleted backup in
  that repository by up to the grace.
- A phantom whose payload upload never completed is still cleaned through the deletion Job, so a
  discard can issue an S3 delete for a prefix whose manifest is already gone. That matches the
  existing prefix-scoped deletion contract.
- Pruning relies on `phase: Invalid` being the terminal parked state. If a future phase is added
  for "validated but unusable", the pruning predicate must be revisited.
- Discovery consumes a completed Job up to one scan tick later; the gate is what closes that skew,
  so the grace must stay comfortably above `BACKUP_DISCOVERY_SCAN_INTERVAL_SECS` and the Job TTL.
- **Residual gate window (clock domains).** `deletedAt` is taken from the operator clock
  (`Timestamp::now()` when the deletion Job completion is observed), while a discover Job's
  `startTime` comes from the cluster clock. If the operator clock lags the cluster clock by more
  than the delete-to-observe latency, a genuinely stale Job can be judged as started after the
  deletion and the gate does not hold. The window is narrow, and both outcomes stay contained: the
  phantom-discard path deletes a recreated record as soon as its validation Job reports
  `NoSuchKey`, and the dangling-prune path clears a record parked `Invalid` on the next complete
  scan. Fixing it properly means comparing the two timestamps in a single clock domain, which is
  out of scope here.
