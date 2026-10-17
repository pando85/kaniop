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
- `cmd/data-mover/src/commands/listing.rs` -- `list_manifest_keys` returns
  `ListManifestResult { keys, incomplete }`, with `incomplete` (formerly `hit_page_cap`) set on both
  incompleteness exits: the internal `MAX_PAGES = 100` page cap and a provider that reports
  `IsTruncated` with no usable `NextContinuationToken`.
- `cmd/data-mover/src/commands/discover.rs` -- pure `discover_truncated` helper turning the result cap
  and the `incomplete` flag into the `truncated` field of the discover result document, with unit
  tests.
- `cmd/data-mover/src/commands/transport.rs` -- backfill call site adapted to `ListManifestResult`;
  warns when an incomplete listing may fail to recognise existing backups.
- `libs/backup/src/controller/mod.rs` -- `DISCOVERED_LABEL`.
- `libs/backup/src/controller/backup.rs` -- `DISCOVERY_CONFIRMATION_GRACE`, Job-timestamp helpers,
  `deletion_completed_at` (cluster-stamped `deletedAt`), `is_precondition_conflict`,
  `delete_params_with_uid_precondition`, `discover_job_may_reference`, `conflicting_discover_job`,
  `discovery_confirmation_pending`, `ValidationFailure`/`classify_validation_failure`,
  `is_discovery_owned`, `list_discover_jobs`, `mark_backup_invalid`, `handle_manifest_absent`,
  `discard_phantom_backup`, gated `reconcile_cleanup`, and a forced
  `PatchParams::apply(CONTROLLER_ID).force()` in `patch_backup_status` so a foreign field manager
  owning `.status.phase` cannot strand the CR `Terminating`.
- `libs/backup/src/controller/discovery.rs` -- `dangling_discovered_backups` (scope-, temporal-,
  UID-, phase- and label-gated), `snapshot_supports_pruning`, prune count threaded out of
  `reconcile_discovered_backups`, `snapshot_time` threaded from the consumed discover Job via
  `job_start_time`, `backup_discovery_dangling_pruned` counter, `Discovered` condition message,
  `read_discover_result` widened to `pub(crate)`.
- `Documentation/src/usage/backup-restore.md` -- stale/missing-manifest policy, bounded
  `Terminating` wait, alert-table and metric wording.
- `Documentation/src/troubleshooting.md` -- symptoms for accumulating `Invalid` discovered records
  and for a deletion held `Terminating`.
- `intent/1081-discovery-retention-race/` -- this artifact chain.
- `evals/cases/discovery-retention-resurrection/` -- eval case for the escaped defect.
- `tests/e2e/test/kanidm/backup.rs` -- owned by the e2e agent, not by this slice: the retention
  delete test waits through the bounded finalizer gate and asserts the surviving catalogue. The
  prune test was redesigned to park the record through the controller rather than the harness, and
  the `park_backup_in_invalid` helper was deleted (no other users).
- `tests/e2e/test/kanidm/mod.rs` -- owned by the e2e agent: the upload fixtures build the
  `tenants/<namespace name>` segment the producers actually use instead of the real Namespace UID,
  which is what made the discovery-based tests scan a prefix they had written to.

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
   `Ready`/`Deleted` condition (stamped by `handle_deletion` from the deletion Job's cluster-side
   `status.completionTime` via `deletion_completed_at`, falling back to the operator clock), list
   discover Jobs for `spec.repositoryRef.name`, keep only Jobs
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
   `discovery.manifest_keys.len() == discovery.total_found as usize`), with the scope predicate
   matching the LIST exactly -- `reconcile_discovered_backups` receives the scan's `namespaceUid`
   and rebuilds `repo_path.manifests_prefix(namespaceUid, kanidmUid)` to require the record's key to
   sit under it; count successes, ignore 404, warn on other errors, return the count, emit
   `backup_discovery_dangling_pruned` when non-zero, and extend the `Discovered` condition message and
   the `discover result processed` log. The
   message extension is best-effort only: issue #1082 makes the second discovery status apply of
   the same tick revert `Discovered`, so the counter and the log line are the durable signals and
   nothing (docs or e2e) may depend on the message persisting.
8. **Producer-side honest truncation**: `list_manifest_keys` returns
   `ListManifestResult { keys, incomplete }` and sets `incomplete` on **both** incompleteness exits --
   the internal `MAX_PAGES = 100` page cap and a provider that reports `IsTruncated` while giving no
   usable `NextContinuationToken` -- and `discover.rs` derives `truncated` from the `maxResults` cap
   **or** that flag through a pure `discover_truncated` helper, so the completeness claim the prune
   relies on is testable without an S3 client. The backfill call site in `transport.rs` warns instead
   of silently under-reporting existing backups.
9. **Unit tests** for each branch listed in `spec.md` acceptance criteria, including the grace
   boundary and the grace-above-Job-TTL invariant.
10. **Documentation**: `Documentation/src/usage/backup-restore.md` (new stale/missing-manifest
   subsection under the transport/discovery area, GC-and-deferral section, alert table, metric
   mention) and `Documentation/src/troubleshooting.md` (two new entries).
11. **Eval case**: add `evals/cases/discovery-retention-resurrection/{case.json,task.md}` from the
   template, validated by `make agent-framework-check`.
12. **Status apply force**: change `patch_backup_status` to
   `PatchParams::apply(CONTROLLER_ID).force()` with a short comment explaining why. No test covers
   it: the crate has no client-level test infrastructure, and asserting the struct field would
   merely restate the production line, while after the prune-test redesign the e2e harness writes no
   `KanidmBackup.status` at all. The change rests on the bounded-deletion constraint in
   `intent.md:61`, on the audit that this is the only in-tree writer of `KanidmBackup.status`, and on
   repo convention; the deterministic foreign-manager e2e scenario that would close the gap is
   recorded as deferred in `spec.md`. The other unforced applies --
   `libs/backup/src/controller/repository.rs:370-374` (manager `backup-repository`) and the oauth2
   image-update site `libs/oauth2/src/reconcile/mod.rs:1549-1554` -- are deferred to issue #1085.

## Dependencies and coordination

- Shared names are fixed here: `ManifestAbsent` (condition reason and Event reason),
  `PhantomBackupDiscarded` (Event action), `MANIFEST_NOT_FOUND`/`MANIFEST_FETCH_ERROR` (result
  document codes), `kaniop.rs/discovered`, `backup_discovery_dangling_pruned`. The e2e agent asserts
  against these exact strings, but only where they are durable: the prune scenario observes the
  dangling `KanidmBackup` CR being deleted and never the `Discovered` condition message, which
  issue #1082 makes transient within a single scan tick.
- Ownership: the e2e files (`tests/e2e/test/kanidm/backup.rs` and `tests/e2e/test/kanidm/mod.rs`) and
  the Rust in `cmd/data-mover/**` and `libs/backup/**` are implemented by other agents, so this plan
  records them as files this branch changes rather than edits it performs. Nothing here touches
  `charts/**`, and no file outside "Files and components" is modified.
- No CRD field is added, so `make crdgen` and `make examples` must stay clean and no
  `cmd/crdgen`/`crd.rs` edit is needed.
- No chart change is needed: the RBAC the new code uses was already granted before this change.

## Risks and blast radius

- **Deletion liveness**: holding the finalizer is the only new blocking behavior. It is bounded by
  the grace, keyed on a `Ready`/`Deleted` condition that only a completed S3 cleanup sets, and
  covered by unit tests at both sides of the boundary.
- **Wrongly discarding a user record**: both the discard and the prune require
  `kaniop.rs/discovered=true`, which only `build_backup_cr` sets. A hand-authored record can never
  be auto-deleted by this change. The label is user-settable, so a user could label their own
  record and have it discarded on `MANIFEST_NOT_FOUND`; this grants no new destructive authority
  (anyone who can edit a `KanidmBackup` can already delete it, and deletion has always removed the
  repository prefix through the finalizer), so the label marks lifecycle ownership, not
  authenticity.
- **Pruning on a partial listing**: blocked by `snapshot_supports_pruning` requiring
  `truncated == false`, plus the scope, temporal, UID, phase, and `deletionTimestamp` conditions.
  `truncated` is honest about every early exit in the producer's LIST loop: `discover_truncated` in
  `cmd/data-mover/src/commands/discover.rs` sets it when the `maxResults` cap is reached **or** when
  `list_manifest_keys` reports its `incomplete` flag, which `listing.rs` now raises on both the
  internal `MAX_PAGES = 100` page cap and a provider that answers `IsTruncated` with no usable
  `NextContinuationToken`. The extra `manifest_keys.len() == total_found` clause is only a cheap sanity
  check on the producer invariant that both fields come from the same list; it never guarded
  anything by itself because `discover.rs` sets `total_found = manifest_keys.len()`.
- **Pruning a record this snapshot never covered**: the snapshot is a LIST of one tenant subtree,
  so `dangling_discovered_backups` requires the record's `spec.manifestKey` to start with the same
  `RepositoryPath::manifests_prefix(namespaceUid, kanidmUid)` the discover Job listed, with the
  repository-level `RepositoryPath::contains_key` check kept on top for confinement and traversal
  rejection. A key under a different `tenants/<identity>` or `clusters/<uid>` segment is kept even
  when it carries the scan's Kanidm UID, which is what contains a namespace-identity keyspace
  divergence (issue #1084) instead of letting it delete a catalogue entry and, through the
  finalizer, its prefix. A repository cannot be re-targeted to hide the difference either: the
  webhook rejects changes to `s3.bucket`, `s3.prefix`, `s3.endpoint` and the encryption identity
  once a repository is in use (`validate_repository_immutable_after_use`) and enforces one
  repository per `(bucket, prefix)` pair (`validate_repository_prefix_unique`). The predicate still
  covers the residual delete-and-recreate-the-repository-with-a-different-prefix case.
- **Provider variance in "not found"**: absence is concluded only from HTTP 404 with an explicit
  `NoSuchKey` code. A gateway that reports a missing key with another status, a `NoSuchBucket` 404,
  or a bare 404 stays `MANIFEST_FETCH_ERROR`/`Retryable`, which is the pre-change behavior: the
  record parks `Invalid` and is cleaned up by a later complete scan instead of being destroyed on
  an ambiguous signal.
- **Clock domains in the deletion gate**: closed. `deletedAt` was the operator clock
  (`Timestamp::now()` when the deletion Job completion was observed) while a discover Job's
  `startTime` is the cluster clock, so skew could release the finalizer early. `handle_deletion`
  now stamps the `Ready`/`Deleted` condition from the deletion Job's `status.completionTime` via
  `deletion_completed_at(job, now)`, keeping both sides of the resurrection comparison in the
  cluster domain. The operator clock remains only as the fallback when a Job has no completion time
  and in the 1800s grace bound, where sub-minute skew is negligible.
- **Temporal guard on pruning**: a snapshot can only prove absence for records that existed
  before the listing began, so `dangling_discovered_backups` additionally requires the record's
  `metadata.creationTimestamp` to be strictly earlier than the Job's start time (falling back to
  its `creationTimestamp`, mirroring `job_start_time` in `backup.rs`). Without a Job timestamp,
  or without a record creation timestamp, the record is kept. This keeps a stale listing from
  deleting a record that was created after the scan began.
- **Identity preconditions on destructive deletes**: both the prune loop and the phantom-discard
  path pin the delete to the record's `metadata.uid` via `DeleteParams.preconditions`. A
  deterministic `kb-<id8>` name can be reused by a different incarnation between the read and the
  delete, so the precondition makes sure the delete never hits the wrong record. A precondition
  rejection (HTTP 409 Conflict or 422 Invalid) is treated as "someone else changed or replaced the
  record": the prune loop skips and does not count it, and the discard path requeues rather than
  erroring.
- **E2e can run locally on Kind with MinIO** (`kind-chart-testing`): the `kanidm-backup` shard
  is the primary proof, and the prune and phantom-discard scenarios were verified locally with
  `cargo test --features e2e-test` against the Kind cluster. CI's shard run remains the durable
  signal.
- **Status-apply ownership**: the `KanidmBackup` status apply is now forced, matching the
  controller-owned status applies elsewhere in `libs/**` (`schedule.rs:320`, `:543`;
  `discovery.rs:1312`; `group`; `oauth2`'s `status.rs`; `person`; `service-account`), all of which are
  forced -- with two exceptions, both deferred to issue #1085:
  `libs/backup/src/controller/repository.rs:370-374` (manager `backup-repository`), the identical
  defect, and the unforced apply in the oauth2 image-update path at
  `libs/oauth2/src/reconcile/mod.rs:1549-1554`, which is latent rather than active because it uses the
  same manager string as that crate's forced apply and so cannot conflict with itself. Discovery never
  writes `KanidmBackup.status`, no field or condition type is written by both controllers, and
  `.status.conditions` is a list-map keyed on `type`, so forcing the apply is neutral for discovery
  and for issue #1082 (which is a `KanidmBackupSchedule.status.discovery` clobber between two forced
  applies from one stale in-memory schedule).

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
- **Requiring two consecutive complete scans before pruning**: rejected -- with honest truncation a
  complete snapshot's omission is already object absence at listing time, and a candidate must
  additionally be discovery-owned, `Invalid`, UID-matched, inside the scanned prefix, and older than
  the snapshot. A second scan would only delay cleanup of records parked by older versions by a
  full scan interval while adding no evidence the first scan did not already provide.
- **Refusing to delete a prefix whose manifest is gone**: rejected -- a payload without its manifest
  is not a Kaniop recovery point. Both the GET-confirmed discard path and the LIST-confirmed prune
  path may therefore remove the prefix, consistent with the existing contract that a CR never
  leaves etcd while its objects may still exist.

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
| Local Kind (`kind-chart-testing`) + MinIO | `backup_discovery_prunes_dangling_invalid_record` ok (174.55s), `backup_phantom_discovered_record_is_discarded` ok (110.59s), exactly one `pruned dangling discovered backup` operator log line for the constructed `tenants/default` key and zero `discarding phantom backup record` lines for it |
| `make check-e2e-shards` | `kanidm-backup: 23`, `Shards OK: 223` |

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
- **Prune test redesign and `park_backup_in_invalid` deletion**: the `backup_discovery_prunes_dangling_invalid_record`
  test was rewritten to park the record through the controller rather than the harness. The old
  `park_backup_in_invalid` helper wrote `{"status":{"phase":"Invalid"}}` with
  `PatchParams::apply("e2e-test")`, creating a managedFields entry that exclusively owned
  `.status.phase`; the controller's unforced apply then 409'd and the test timed out. Even with
  the force applied, the old design would stay flaky because the controller applies status read from
  its informer cache, so a tick can reclaim and revert the harness-written `Invalid`, and the helper
  stopped re-parking as soon as it observed `Invalid` -- a scan can then miss the prune predicate.
  The new test creates a user-authored `KanidmBackup` (`status: None`, labels
  `kaniop.rs/repository` + `kaniop.rs/backup-id`, `spec.kanidmRef.uid` from the live Kanidm) whose
  `manifest_key` is `{s3_prefix}/v1/tenants/default/clusters/{kanidm_uid}/backups/{fresh uuid}/manifest.json`
  -- namespace NAME `default`, never uploaded, no `discovered` label yet. It waits for the controller
  to park the record `Invalid` itself (`mark_backup_invalid`, asserts `Ready` reason `ManifestAbsent`
  and that `status.createdAt` is absent so retention can never select it), and only then adds
  `kaniop.rs/discovered=true` with a metadata-only JSON merge patch. The test then waits for the CR
  to be pruned (300s) and asserts there is **no** `Warning`/`ManifestAbsent`/`PhantomBackupDiscarded`
  event for that record, which distinguishes the prune from the discard path. The `park_backup_in_invalid`
  helper was deleted (no other users). This is a better test: the dangling prune is provably the only
  possible deleter (the key never appears in a snapshot, so discovery cannot resurrect or re-validate
  it; retention cannot select it; `handle_deletion` only runs after the prune), and it reproduces the
  real production state described in `intent.md:19-23` -- records left `Invalid` by an older controller
  version -- instead of a state only a test harness can create.
- **No automated coverage for the forced status apply**: `libs/backup` has no client-level test
  infrastructure (no mock/fake kube backend; `tower-test` is a dev-dependency of `libs/operator`
  only), and asserting `PatchParams::apply(CONTROLLER_ID).force().force == true` would merely restate
  the production line. The non-tautological pattern that exists in this repo is the request-capturing
  mock in `libs/operator/src/kanidm/reconcile/mod.rs` -- `get_test_context` and the
  `tower_test::mock::pair` harness set up next to it -- which asserts `force=true` on the request URL;
  adopting it here would require new dev-dependencies and a `Context<KanidmBackup>` builder that does
  not exist in this crate. The e2e shard does not cover the force either: the prune-test redesign
  deleted `park_backup_in_invalid`, which was the only harness status writer, so
  `grep patch_status tests/e2e/test/kanidm/backup.rs` returns nothing and the earlier claim that the
  harness's `e2e-test` apply was the behavioural guard is false. Justification rests on the
  bounded-deletion constraint in `intent.md:61`, on the audit that `patch_backup_status` is the only
  in-tree writer of `KanidmBackup.status`, and on repo convention; the deterministic foreign-manager
  scenario that would test it is deferred in `spec.md`.
- **`list_manifest_keys` incompleteness hardening** (implemented in `cmd/data-mover` while this
  artifact was being corrected): the loop breaks on three conditions -- the `maxResults` cap, the
  internal `MAX_PAGES = 100` page cap, and a provider that reports `IsTruncated` with no usable
  `NextContinuationToken` -- and only the page-cap one reported incompleteness. The unreported exit
  is the dangerous one: the loop ended with the flag false and fewer keys than `max_results`, so
  `discover_truncated(...)` returned `false` and `snapshot_supports_pruning` admitted a PARTIAL
  snapshot as complete; the prune's `!discovered_keys.contains(key)` predicate then read absence into
  keys the LIST never returned, deleted the `Invalid` discovered record, and the finalizer ran
  `handle_deletion` -> `build_deletion_job`, removing the whole `backups/<id>/` prefix and destroying a
  recovery point. `hit_page_cap` is therefore renamed `incomplete` and set on both incompleteness
  exits, so `truncated == false` genuinely means the listing saw every manifest under the scanned
  prefix. Reachability is low -- AWS S3 and MinIO both conform to ListObjectsV2 -- but the guard must
  not depend on provider conformance, and this PR had already promoted `truncated == false` to the
  prune's sole completeness evidence in `snapshot_supports_pruning` and in the documentation. No unit
  test covers the LIST loop itself because the crate has no fake S3 client; the pure
  `discover_truncated` helper is tested, including a case with `incomplete` set below both caps.
- **Keyspace revert (commit `b49eef8b`)**: during this work an external automation
  (`forkline-dev[bot]`) pushed commit `b49eef8b` "fix(backup): use namespace UID instead of name in
  discovery" to the PR branch, changing the consumer to scan `tenants/<Namespace object UID>/`. That
  inverts the deployed keyspace: the `tenants/` segment carries the namespace NAME even though the
  operation-document field is called `namespaceUid` (producers
  `libs/operator/src/kanidm/reconcile/transport.rs:187` emitted at `:216`, and
  `libs/operator/src/kanidm/restore/legacy.rs:2693`; live objects are
  `kanidm/v1/tenants/kanidm/clusters/<uid>/backups/<id>/manifest.json` and the discover Job logs
  `namespace_uid="kanidm"`). Merged, it would have orphaned every existing object and stopped the
  catalogue advancing, firing `KaniopBackupStale` within 24h on a cluster whose backups were still
  uploading. It was reverted in the working tree, the call site now carries a comment pinning the
  convention, the real defect was in the e2e fixture (which used the UID) and is fixed there, and the
  naming debt is tracked as issue #1084. The `Rust` workflow was cancelled for that commit, so only
  `Helm`, `Pre-commit` and `verify-examples` -- none of which touch the keyspace -- ever ran against it.
