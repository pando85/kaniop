# Intent: Stop retention deletes from resurrecting phantom KanidmBackup records

- Owner: pando85
- Status: draft
- Work item: https://github.com/pando85/kaniop/issues/1081

## Problem

Retention deletes a `KanidmBackup` CR; the finalizer holds it while a deletion Job removes the
S3 objects. A discover Job whose S3 `LIST` began before that delete finished returns a snapshot
that still contains the deleted manifest key, and discovery consumes it up to one scan interval
later. Its only guard -- `existing_manifest_keys` built from the live CR list -- no longer holds
the key because the CR is gone, so discovery recreates a phantom `KanidmBackup`. The validation
Job then fails with HTTP 404 `NoSuchKey`, and the controller has no way to tell "the object is
gone" from "the object was unreachable": every manifest fetch error was reported as retryable
with no result document, so the CR is parked `phase: Invalid` forever and keeps incrementing
`kaniop_backup_validation_failures_total`.

Observed in production on v0.18.0: 38 `Invalid` versus 12 `Ready` records, with
`KaniopBackupValidationFailures` firing daily (2026-10-15/16/17; e.g. an artifact deleted from
2026-10-09 was resurrected at 03:23:48, 404 at 03:23:51, alert firing 03:26-03:34). No recovery
point was lost -- the deleted artifacts were already outside the retention window -- but the
catalogue lies, the alert is pure noise, and the junk accumulates.

## Proposed outcome

A retention delete is never undone by an in-flight discovery scan, and a manifest that is
demonstrably absent is handled as an absent manifest instead of being parked as a validation
failure:

- Deleting a backup completes, but only once discovery can no longer resurrect it; the wait is
  bounded so a stuck scan cannot block deletion indefinitely.
- The data mover reports an absent manifest distinctly from an unreachable one.
- A phantom record created by discovery is discarded; a user-authored record pointing at a gone
  manifest stays an error the operator must see.
- Records already parked by older versions are cleaned up automatically.
- `KaniopBackupValidationFailures` means "a backup you authored is broken", not "retention ran".

## Affected users and systems

- Operators running remote backups with retention: the daily validation alert stops firing for
  normal retention activity, and `kubectl get kanidmbackup` stops filling up with `Invalid`
  records that reappear after every delete.
- Operators who hand-author `KanidmBackup` records for cataloged cross-UID recovery: they now get
  a precise `ManifestAbsent` reason instead of a generic validation failure, so a deleted or
  mistyped manifest key is distinguishable from an outage.
- Backup deletion: `KanidmBackup` deletion can stay `Terminating` briefly by design before the
  finalizer is removed.
- Observability: one new counter, one new condition reason, one new error code in the result
  document contract shared with the data-mover Job.

## Constraints and non-goals

- Issue #1082 (discovery overwriting unrelated `KanidmBackupSchedule` status) is explicitly out of
  scope here.
- No restore-drill automation and no change to retention policy semantics, selection, or timing.
- No CRD schema change: everything is expressed through existing status conditions, the existing
  `kaniop.rs/discovered` label, and result documents.
- Must not delete S3 objects outside the existing deletion flow, and must not weaken the
  `existing_manifest_keys` guard or any restore-safety gate.
- Deletion must stay bounded: no state where a `KanidmBackup` can never terminate. That constraint
  is what makes the status apply in `patch_backup_status` forced: a foreign field manager owning
  `.status.phase` would otherwise make the `phase = Deleting` write conflict, leaving the finalizer
  in place and the S3 deletion Job never created.

## Success measures

- `make lint` and `cargo test --workspace` pass with the new classification, gate, and pruning
  tests.
- The e2e `kanidm-backup` shard passes in CI, including the retention-delete scenario that now
  waits through the bounded finalizer gate. The prune and phantom-discard scenarios were also
  verified locally on Kind (`kind-chart-testing`) with MinIO.
- `make crdgen` and `make examples` produce no drift.
- A retention delete followed by a stale scan leaves no `Invalid` discovered record behind, and
  `kaniop_backup_validation_failures_total` does not increase.
- Documentation states the new reason, the discard/prune policies, and the bounded deletion wait.

## Open questions

None. The grace bound and the discard-versus-park split are recorded in `spec.md`.
