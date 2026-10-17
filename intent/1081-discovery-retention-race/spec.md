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
   `deletedAt` is a single-clock-domain value: `handle_deletion` stamps the `Ready`/`Deleted`
   condition from the deletion Job's cluster-side `status.completionTime` through
   `deletion_completed_at(job, now)`, falling back to the operator clock only when the Job reports
   no completion time. Both sides of the comparison are therefore cluster-stamped, so operator
   clock lag can no longer make a genuinely stale Job look like it started after the deletion.

3. **The wait is bounded**: the hold applies only while
   `now - deletedAt <= DISCOVERY_CONFIRMATION_GRACE` (1800s). After that the controller logs a
   warning naming the conflicting Job and removes the finalizer, so a stuck discover Job costs
   deletion latency, never deletion liveness. The grace deliberately exceeds
   `BACKUP_JOB_TTL_SECONDS` (600s) so a Job still inside its own lifetime is always covered.
   `reconcile_cleanup` resolves the conflicting Job once with `conflicting_discover_job` and passes
   it to `discovery_confirmation_pending`, so the wait/no-wait decision and the Job named in the log
   line come from a single evaluation of the same snapshots. The bound is measured from that same
   cluster-stamped `deletedAt` against the operator's `now`; mixing the two clock domains here is
   deliberate because a 30-minute magnitude is insensitive to sub-minute skew, whereas the
   Job-versus-deletion comparison above is not.

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
   prefix, which may still hold the payload. A 404 on the delete is treated as already done, a
   precondition conflict (409 Conflict or 422 Invalid) requeues so the current incarnation is
   revalidated, and any other delete error fails the reconcile.

7. **User-authored records stay visible failures**: a backup without the discovered label whose
   manifest is absent becomes `phase: Invalid` with `Ready=False`, `reason: ManifestAbsent`, a
   message naming the manifest key, and does increment
   `kaniop_backup_validation_failures_total`. Non-absent failures keep `reason: ValidationFailed`.

8. **Dangling records are pruned** (`libs/backup/src/controller/discovery.rs`,
   `dangling_discovered_backups`): on every scan whose snapshot is complete (`truncated == false`)
   and self-consistent (`manifest_keys.len() == total_found`, checked by
   `snapshot_supports_pruning`), discovery deletes records that are simultaneously (a) labeled
   `kaniop.rs/discovered=true`, (b) `phase: Invalid`, (c) for the scanned Kanidm UID, (d) without
   `deletionTimestamp`, (e) absent from the snapshot, (f) inside the exact prefix the scan listed,
   (g) created strictly before the scan started, and (h) carrying a `metadata.uid`. This cleans up
   records parked by older versions. Pruning counts only successes; a delete error is logged and
   left for the next scan.
   (f) matches the LIST scope, not just the repository: the discover Job lists
   `RepositoryPath::manifests_prefix(namespaceUid, kanidmUid)`, so the prune rebuilds that same
   prefix and requires the record's `spec.manifestKey` to start with it, on top of the repository
   confinement that keeps traversal keys out. A snapshot cannot speak for keys it never scanned,
   and the `tenants/` segment currently carries the namespace name rather than a uid
   (issue #1084), so a producer/consumer keyspace divergence must not be able to turn
   absence-of-evidence into deletes.
   `truncated` is honest about every way the producer's LIST loop can stop early:
   `discover_truncated` in `cmd/data-mover/src/commands/discover.rs` sets it when the `maxResults`
   cap is reached **or** when `list_manifest_keys` returns its `incomplete` flag (formerly
   `hit_page_cap`), which `listing.rs` now raises on both incompleteness exits -- the internal
   `MAX_PAGES = 100` page cap, and a provider that reports `IsTruncated` with no usable
   `NextContinuationToken`. This matters because `truncated == false` is the prune's only
   completeness evidence: it has to mean the listing really did reach the end of the scanned prefix.
   The `len == total_found` clause is retained only as a cheap sanity check on the
   producer invariant that both fields come from the same list; on its own it never guarded
   anything, since `discover.rs` sets `total_found = manifest_keys.len()`.

9. **Pruning and discovery reporting are observable**: pruning increments
   `kaniop_backup_discovery_dangling_pruned_total{repository}` once per scan with the pruned count,
   and logs `pruned dangling discovered backup`. Those two are the durable signals. The
   `, pruned N dangling` suffix added to the `Discovered` condition message when `N > 0` is
   best-effort and does **not** reliably persist while issue #1082 is open: a successful scan tick
   applies the discovery status twice from the same stale in-memory schedule, and the second apply
   (`DiscoveryTruncated`) reverts `Discovered` to its pre-tick value. Nothing on this branch may
   treat that message -- or the e2e test -- as depending on it.

10. **Status writes cannot be blocked by field ownership** (`libs/backup/src/controller/backup.rs`,
    `patch_backup_status`): the `KanidmBackup` status apply is forced
    (`PatchParams::apply(CONTROLLER_ID).force()`, `CONTROLLER_ID = "backup"`). The applied document is
    the whole status object, so an unforced apply loses to any foreign manager that already owns
    `.status.phase` -- a `kubectl patch --subresource=status` runbook, a migration or GitOps job, or a
    later co-writer; the e2e harness was such a manager until this branch deleted its `.status`
    writes. That failure is not a retryable hiccup: the `409 Conflict / FieldManagerConflict` on the
    `phase = Deleting` write exits through `backoff_reconciler` before `handle_deletion` runs, so the
    finalizer is never removed and the S3 deletion Job is never created
    -- the unbounded deletion `intent.md` forbids, newly load-bearing because the discard and prune
    paths added here depend on that transition. Forced applies take the field instead of failing on
    it. This is neutral for the other controller and for issue #1082: discovery never writes
    `KanidmBackup.status` (it POST-creates the CR with `status: None` at `discovery.rs:1075` and
    `:1225`, and `update_discovery_status` targets `KanidmBackupSchedule` at `:1264`), no field or
    condition type is written by both, and `.status.conditions` is an `x-kubernetes-list-type: map`
    keyed on `type`, so an apply claims only the elements it sends. That audit -- `patch_backup_status`
    is the only in-tree writer of `KanidmBackup.status` -- together with the bounded-deletion
    constraint in `intent.md:61` is what justifies the force, which has no automated coverage.

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
- `deletion_completed_at` tests cover: a deletion Job with `status.completionTime` yields that
  cluster-stamped time (not the operator `now`), and the operator `now` is used when the Job has no
  completion time or no status at all.
- `DISCOVERY_CONFIRMATION_GRACE > BACKUP_JOB_TTL_SECONDS` is asserted.
- `classify_validation_failure` tests cover `MANIFEST_NOT_FOUND`, a non-success document carrying
  `MANIFEST_NOT_FOUND`, other codes, a document without an error, and no document.
- `is_discovery_owned` tests cover label `true`, `false`, an unrelated value, and no labels.
- `is_precondition_conflict` tests cover 409 Conflict, 422 Invalid, a 409 carrying only the code
  without a reason, other statuses (404/403/500), and a non-API error.
- `delete_params_with_uid_precondition` pins `preconditions.uid`, leaves `resourceVersion` unset,
  and keeps `dryRun` off.
- `dangling_discovered_backups` tests cover: prune an `Invalid` absent discovered record; keep a
  record present in the snapshot, a `Ready`/`Discovering`/`Deleting`/`Deleted` record, a record with
  no status, a user-authored record, a record of another Kanidm UID, and a record already
  terminating. Temporal evidence: prune only records created strictly before the scan started, keep
  records created at or after it, keep a record with no `creationTimestamp`, and keep everything
  when no snapshot time is available. Identity: keep a record with no `metadata.uid`. Scope: prune
  a key inside the prefix the scan listed and keep a key outside the repository (including a
  traversal key), a key under a different `tenants/<identity>` segment, a key under a different
  `clusters/<uid>` segment while carrying the scan's Kanidm UID, and a sibling directory that only
  shares the listed prefix's text.
- `snapshot_supports_pruning` tests cover: a complete self-consistent snapshot (including an empty
  one) may prune; a truncated snapshot may not; a snapshot whose key list disagrees with its
  reported total may not.
- `discover_truncated` tests cover: the incomplete flag alone truncates below the result cap
  (including with zero keys), the result cap alone truncates, either cap exceeded, both caps hit, and
  neither limit reached. No test exercises the `list_manifest_keys` loop itself -- the crate has no
  fake S3 client -- so its `incomplete` flag is covered only through this helper.
- E2e in `tests/e2e/test/kanidm/backup.rs` keeps the retention-delete scenario green through the
  new gate, and CI runs it in the `kanidm-backup` shard. The prune scenario builds a real dangling
  record without writing `.status` at all: it creates a user-authored `KanidmBackup` whose manifest
  key was never uploaded, waits for the controller to park it `Invalid` with
  `Ready{ManifestAbsent}`, only then labels it `kaniop.rs/discovered=true`, and asserts the CR is
  pruned with **no** `Warning`/`ManifestAbsent`/`PhantomBackupDiscarded` event for it -- which is
  what separates the prune from the discard path. It keeps `!DiscoveryTruncated` and never asserts
  on the `Discovered` condition message, which issue #1082 makes transient.
- The forced status apply has **no** automated coverage. Unit coverage is infeasible in this crate:
  `libs/backup` has no client-level test infrastructure (no mock/fake kube backend; `tower-test` is a
  dev-dependency of `libs/operator` only), and the only in-crate alternative -- asserting
  `PatchParams::apply(CONTROLLER_ID).force().force == true` -- restates the production line. The e2e
  coverage that would have exercised it was removed deliberately: the harness owning `.status.phase`
  first deadlocked and then flaked the prune test (see `plan.md`), so
  `grep patch_status tests/e2e/test/kanidm/backup.rs` is now empty. What justifies the force is the
  bounded-deletion constraint in `intent.md:61`, the audit that the backup controller is the only
  in-tree writer of `KanidmBackup.status`, and repo convention -- not a test.

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
| Field manager | `backup` (`CONTROLLER_ID`, `libs/backup/src/controller/backup.rs`), forced | the only claim on `KanidmBackup.status`; forced so a foreign owner of `.status.phase` cannot make the apply conflict |
| Constant | `DISCOVERY_CONFIRMATION_GRACE = 1800s` | upper bound on waiting for discovery to observe a deletion |

## Security and privacy

- No credential handling changes; the data mover remains the only component reading S3 objects.
- Pruning and discarding only delete Kubernetes records that discovery itself created, never
  user-authored records, and only from a complete snapshot -- a truncated listing can never justify
  a deletion.
- Event and condition messages carry manifest keys and provider error text, which contain no
  secrets.
- **Temporal guard on pruning.** A snapshot can only prove absence for records that existed
  before the listing began, so `dangling_discovered_backups` additionally requires the record's
  `metadata.creationTimestamp` to be strictly earlier than the Job's start time (falling back to
  its `creationTimestamp`, mirroring `job_start_time` in `backup.rs`). Without a Job timestamp,
  or without a record creation timestamp, the record is kept. This keeps a stale listing from
  deleting a record that was created after the scan began.
- **Identity preconditions on destructive deletes.** Both the prune loop and the phantom-discard
  path pin the delete to the record's `metadata.uid` via `DeleteParams.preconditions`. A
  deterministic `kb-<id8>` name can be reused by a different incarnation between the read and the
  delete, so the precondition makes sure the delete never hits the wrong record. A precondition
  rejection (HTTP 409 Conflict or 422 Invalid) is treated as "someone else changed or replaced the
  record": the prune loop skips and does not count it, and the discard path requeues rather than
  erroring.
- **Ownership label is lifecycle, not authenticity.** The `kaniop.rs/discovered=true` label is
  user-settable (the webhook validates spec immutability only), so a user could label their own
  record and have it discarded on `MANIFEST_NOT_FOUND`. This grants no new destructive authority:
  anyone able to edit a `KanidmBackup` can already delete it, and deletion has always removed the
  repository prefix through the finalizer. The label therefore marks lifecycle ownership, not
  authenticity.
- **Scope guard on pruning.** A snapshot only lists one tenant subtree, so the prune judges only
  records inside the exact prefix the scan listed: `dangling_discovered_backups` requires the
  record's `spec.manifestKey` to start with the same
  `RepositoryPath::manifests_prefix(namespaceUid, kanidmUid)` the discover Job lists, and keeps the
  repository-level `RepositoryPath::contains_key` check on top for confinement and traversal
  rejection. A record under a different `tenants/<identity>` or `clusters/<uid>` segment of the same
  repository is kept, because a snapshot cannot speak for keys it never scanned -- the guard that
  contains a namespace-identity keyspace divergence (issue #1084) instead of letting it delete a
  catalogue entry and, through the finalizer, its prefix.
- **Single clock domain in the deletion gate.** The gate's `deletedAt` is taken from the deletion
  Job's cluster-stamped `status.completionTime` (`deletion_completed_at`) instead of the operator
  clock, so it is comparable with discover Job `status.startTime` values. The residual skew risk
  this removes was the one that could release the finalizer early and let a stale snapshot
  resurrect a record.

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
- **Residual gate window (clock domains) -- closed.** `deletedAt` used to be the operator clock
  (`Timestamp::now()` when the deletion Job completion was observed) while a discover Job's
  `startTime` is the cluster clock, so operator lag could make a genuinely stale Job look like it
  started after the deletion and release the finalizer early. `handle_deletion` now stamps the
  `Ready`/`Deleted` condition from the deletion Job's `status.completionTime`
  (`deletion_completed_at`), so both sides share the cluster clock domain. The operator clock
  remains only as the fallback when a Job reports no completion time, and in the grace bound
  (`now - deletedAt <= 1800s`), where sub-minute skew is negligible.
- A payload whose manifest is gone is **not** a Kaniop recovery point. That accepted policy is why
  both the GET-confirmed discard path and the LIST-confirmed prune path may remove the whole
  repository prefix: it is consistent with the existing contract that a CR never leaves etcd while
  its objects may still exist, and with the prefix-scoped deletion Job.

## Decisions deliberately not implemented

- **Repository retargeting cannot make a snapshot disagree with a record's key.**
  `cmd/webhook/src/backup_validator.rs` rejects changes to `s3.bucket`, `s3.prefix`, `s3.endpoint`
  and the encryption identity once a repository has been used
  (`validate_repository_immutable_after_use`), and enforces one repository per
  `(bucket, prefix)` pair (`validate_repository_prefix_unique`). So a scan can never be judged
  against records written under a different prefix by a re-targeted repository. The scope
  predicate in `dangling_discovered_backups` is still kept: it covers the residual
  delete-and-recreate-the-repository-with-a-different-prefix case, and any key that simply does
  not belong to the scanned prefix.
- **A "two consecutive complete scans" rule before pruning was considered and rejected.** With
  honest truncation, a complete snapshot's omission already means object absence at listing time,
  and a candidate record must additionally be discovery-owned, `Invalid`, UID-matched, inside the
  scanned prefix, and older than the snapshot. Requiring a second scan would delay cleanup of
  records parked by older versions by a full extra scan interval while adding no evidence the
  first scan did not already provide.
- **The other unforced status applies in the tree are not fixed here.** The identical defect in
  `patch_repository_status` (`libs/backup/src/controller/repository.rs:370-374`, manager
  `backup-repository`) is the same class -- a foreign field manager owning `.status.phase` can make
  the apply conflict. So is the unforced apply in the oauth2 image-update path (`update_image`,
  `libs/oauth2/src/reconcile/mod.rs:1549-1554`), though it uses the same manager string as that
  crate's forced status apply, so it cannot conflict with itself and is latent rather than active.
  Repairing either is outside the scope of #1081 and would widen this diff; both are filed as
  issue #1085.
- **A deterministic foreign-manager e2e scenario for the forced apply is deferred, not rejected.**
  Patching `KanidmBackup.status` as `e2e-test` on a `Ready` backup, deleting it, and asserting the
  record terminates would exercise the force against a real API server. It is not added now because
  it races the controller's own status re-apply from its informer cache -- `reconcile_apply` and
  `reconcile_cleanup` each clone `obj.status` and patch it -- so a tick can reclaim
  `.status.phase` before the delete ever happens and let the scenario pass without proving the
  force. Kept as the follow-up proof if the forced apply is ever revisited.
- **`is_precondition_conflict` accepting 422 as well as 409 is left as-is.** The API server answers
  a failed `preconditions.uid` delete with 409, while 422 is a request-validation status, so a
  persistent 422 -- an admission policy denying the DELETE, for example -- makes
  `discard_phantom_backup` requeue forever with only an `info!` log instead of surfacing an error.
  The behaviour is fail-safe (nothing is destroyed) and visible through the Warning event the
  discard path publishes on every pass, and narrowing the match risks turning a legitimate
  precondition conflict into an error loop. Candidate for a future issue, not a defect of this PR.
