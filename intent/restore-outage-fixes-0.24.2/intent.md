# Intent: a restore never leaves Kanidm down with no way out

- Owner: kaniop maintainers
- Status: accepted for 0.24.2
- Work item: v0.24.1 live backup/restore review, findings B3 (F4A.6, F3A.6) and B4 (F3A.6,
  F3B.11); campaign plan in `intent/backup-restore-live-review/`

## Problem

A `KanidmRestore` scales the target Kanidm to zero replicas before it runs its Jobs. On v0.24.1
two classes of defect can then leave the identity service down indefinitely:

- **B3: restore Jobs can hang forever.** The safety backup, source preparation, source check,
  restore and verify Jobs do not copy the primary replica group's `nodeSelector`, `tolerations`
  or node affinity, and have no `activeDeadlineSeconds`. The controller only reads the Job's
  `status.failed`/`status.succeeded` counters. A Job pod that never starts (a taint only Kanidm
  tolerates, `CreateContainerConfigError`, image pull) stays Pending and the restore phase never
  changes. Once `databaseMutationStarted` is persisted (RestoringPrimary) there is no way out:
  deletion continues reconciliation and `restore.kaniop.rs/force-release` requires phase
  `Failed`, which can never be reached.
- **B4: validation misses dependencies that only fail after quiesce.** `validate()` checks the
  source repository's `Ready/Accepted` condition only; it never checks the safety backup
  repository, `EncryptionKeyReady`, or the KEK Secret. Live evidence:
  - F3A.6: a safety repository whose KEK Secret was missing quiesced Kanidm (replicas 0) and the
    safety Job sat in `CreateContainerConfigError: secret "p3a-kek-m3" not found` indefinitely.
  - F3B.11: `safetyBackup.repositoryRef` naming a missing repository kept the restore in
    `Validating` forever with an empty message and an ERROR log every 5 s, and, because it
    counted as active, made every other restore of that target fail.

## Proposed outcome

- A restore Job that cannot start or finish ends in a terminal state within a bounded time.
  Before the mutation boundary the target is resumed automatically; after it the restore becomes
  `Failed`, so the audited force-release path works.
- Restore Jobs can be scheduled wherever the primary Kanidm pod can.
- A missing or unusable repository or KEK fails the restore in `Validating`, before quiesce,
  with a message naming the cause, `databaseMutationStarted=false` and replicas untouched.

## Affected users and systems

Operators running restores, especially on tainted/dedicated nodes, node-local PVs, or with
client-side encrypted repositories. Code: `libs/operator/src/kanidm/restore/legacy.rs`.

## Constraints and non-goals

- Patch release 0.24.2: no CRD change, smallest correct diff.
- Do not copy pod anti-affinity, pod affinity or topology spread into Jobs.
- Backup and discovery Jobs (`libs/backup`) are out of scope: they mount no Kanidm PVC and are
  not tied to the Kanidm node.
- Not in scope: KEK fingerprint, domain or S3 preflight before quiesce (O2), richer Job failure
  messages (O1), local restore path (B10).

## Success measures

- Unit tests: `job_state` reports `Failed` for a `Failed=True`/`DeadlineExceeded` Job with no
  failed pod (fails on v0.24.1); scheduling copy; repository and KEK validation.
- e2e: restore Jobs carry the primary placement and deadlines; missing safety repository and
  missing KEK Secret end `Failed` before quiesce.

## Open questions

None for 0.24.2. Configurable deadlines can follow if real databases need more than 30 minutes.
