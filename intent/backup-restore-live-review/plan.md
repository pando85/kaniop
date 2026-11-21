# Implementation plan: Live review of every Kaniop backup and restore path

Derived from: [spec.md](spec.md) in the same change. Plan v3, approved 2026-11-21.

## Repository findings

- Local restore path: `libs/operator/src/kanidm/restore/legacy.rs:59` sets `BACKUP_PATH` to
  `/data`; `libs/operator/src/kanidm/reconcile/statefulset.rs:51` renders
  `online_backup.path = "/data/backups"`.
- `libs/backup/src/controller/schedule.rs:336` rejects `localVersions < 2`.
- `validate_backup_compatibility` in `libs/operator/src/kanidm/restore/legacy.rs` (around
  line 2868) runs for remote restores only.
- `tests/e2e/scripts/setup-minio.sh` uses the same MinIO-compatible image as production.
- The e2e cleanup script deletes `minio*` names, so test objects are prefixed `minio-test-`.

## Files and components

This change adds only this intent directory. No code, chart, CRD, or example changes.

Changes made during the campaign land elsewhere:

- Fix PRs in this repository, one per defect (the SIGILL release fix is the first).
- `evals/cases/` entries for significant escaped defects.
- Homelab GitOps repo: Kaniop version bump, restore template extension
  (`system/kanidm/templates/kanidm-restore.yaml`), runbook updates.

## Ordered implementation steps

### Phase 0: upgrade homelab Kaniop to the fixed release

Blocked on the SIGILL fix and release 0.23.1.

- Pre-flight: outside 02:30 to 03:45 UTC, a recent ZFS snapshot exists, Kanidm is healthy.
- Bump the chart version in the homelab GitOps repo, commit, push (owner approval).
- Post-checks: operator, webhook, and sidecar on the new version; repository and schedule
  `Ready`; `usageStarted` set; no WARN or ERROR logs for 5 minutes; CRDs match the release.

### Phase 1: normal restore of production Kanidm through GitOps

- Extend the homelab restore template with `backupRef` and `safetyBackup.repositoryRef`.
- Restore from the latest daily backup, accepting loss of manual changes since 03:00 UTC.
- Verify per `spec.md` (replicas, replication, logins, OAuth2 apps, admin password rotation,
  safety backup uploaded), then disable the restore in git.
- Rollback: the safety backup, or the ZFS snapshot.

### Phase 2: test environment in `default` (kubectl)

- Deploy `minio-test` (TLS, static KMS key, buckets, users) and `minio-test-http` per `spec.md`.
- Deploy `kanidm-test` and seed data through CRs and the CLI.
- Run the disaster-recovery rehearsal right after: copy one production backup read-only into
  `minio-test` and catalogue and restore it only there.

### Phase 3: option matrix

Run rows M1 to M8 and R1 to R4 from `spec.md`, each with its own repository and prefix, one
schedule per Kanidm at a time. R4 runs on a separate Kanidm.

### Phase 4: suspected defects and real-cluster faults

Run the fault cases from `spec.md`. Destructive cases run last: retention pruning, suspend
toggle, delete schedule or repository.

### Phase 5: cleanup and report

- Delete (or unfinalize) `KanidmBackup` objects while the repository and MinIO are still alive.
- Delete labelled objects, then find operator-created objects (not labelled) by owner and name.
- Verify no PV, Job, or Secret leftovers.
- Turn findings into issues, `evals/cases/` entries, and homelab runbook updates.

## Dependencies and coordination

- Phase 0 depends on the SIGILL fix PR and release 0.23.1.
- Every production push needs owner approval.
- Avoid the daily backup window (03:00 UTC) for anything touching production.

## Execution and gating

- A coordinator plans and reviews; subagents execute each phase and append to a findings log.
- Gate with the owner after every phase before starting the next.
- Code fixes get their own change and PR; this campaign does not edit code.

## Risks and blast radius

- Production data loss on restore: mitigated by the safety backup, a fresh ZFS snapshot, and
  accepting only the loss of manual changes since the last daily backup.
- Production bucket deletion by a test `KanidmBackup` finalizer: mitigated by the pre-apply
  guard and per-row test buckets.
- Production operator restart: mitigated by never running the e2e harness against `grigri`.
- Alert noise from `default`: accepted.

## Alternatives rejected

- Run the campaign on Kind: does not exercise node-local ZFS, `Retain` PVs, real scheduling, or
  production data. Kind is already covered by e2e.
- Test against the production bucket: the finalizer can delete production backups.
- Re-test what e2e already covers (concurrent restores, delete mid-restore, force-release,
  finalizer, `keepLast`): no added value.

## Verification

- `make agent-framework-check` passes for this change.
- Each phase is verified against the acceptance criteria in `spec.md`, with evidence recorded
  in the findings log.

## Rollback

- This change: revert the commit.
- Phase 0: revert the version bump in the homelab GitOps repo.
- Phase 1: restore from the safety backup or roll back to the ZFS snapshot.
- Phases 2 to 4: delete the labelled test objects in `default` (Phase 5 procedure).

## Deviations

Changes from plan v2 after independent review, folded into this plan:

- Production repository has no encryption (`EncryptionKeyReady` was stale).
- Operator upgrade moved to a GitOps-only Phase 0; the chart hook applies CRDs.
- Disaster-recovery rehearsal uses a read-only copy of a production backup in `minio-test`.
- Restore verification relies on CLI-created data.
- `minAge` and `keepLast` set at creation because they are immutable.
- Out of scope because e2e covers them: concurrent restores, delete mid-restore,
  force-release, finalizer deletion, `keepLast` retention.
