# Intent: Harden backup/restore correctness, observability, and e2e coverage

- Owner: pando85
- Status: draft
- Work item: https://github.com/pando85/kaniop/issues/1000

## Problem

Backup and restore are functional but gated behind `TransportExperimental`. An audit
found five correctness bugs and several coverage gaps:

- **A1** -- Local restore resolves files under `/data/<fileName>` instead of
  `/data/backups/<fileName>`, where Kanidm actually writes online backups.
- **A2** -- A local restore without `safetyBackup.repositoryRef` passes validation
  and deadlocks after quiescing because no safety backup can be created.
- **A3** -- Deleting a post-mutation `Failed` restore silently clears the target
  lock annotation and resumes service on an unverified database.
- **A4** -- Safety-backup ID is derived from restore NAME, so reusing a restore
  name overwrites the prior safety payload.
- **A5** -- Object-Lock and access-denied deletion failures loop silently with no
  condition, metric, or alert, contradicting ADR-0001.

Additional gaps: a missing KEK can brick Kanidm pod startup (transport sidecar
injected unconditionally); documented alerts (`KaniopBackupStale`,
`KaniopBackupFailures`) do not exist; e2e coverage is missing for corrupt remote
payload, full-prefix S3 cleanup, restart-at-mutation-phase, remote HA drill,
Object-Lock deferral, and active-restore deferral; retention policy has no
property tests.

## Proposed outcome

Close the five correctness bugs, add KEK preflight gating on transport sidecar
injection, ship the two missing alerts that correspond to new metrics, add e2e
tests for each identified gap, and add retention property tests -- all without
removing the `TransportExperimental` gate (upstream-blocked).

## Affected users and systems

- Operators running Kaniop with client-side encryption: KEK-missing no longer
  bricks the Kanidm StatefulSet.
- Operators performing local or remote restores: path resolution, safety-backup
  validation, and deletion semantics are correct.
- Operators with Object-Lock or restricted-deleter S3 policies: backup GC
  deferral is visible via conditions, metrics, and alerts instead of silent loops.
- Downstream CI: e2e shards cover restore mutation-phase restart, corrupt remote
  payload, remote HA round-trip, Object-Lock deferral, and active-restore deferral.

## Constraints and non-goals

- **Non-goals** (deferred or upstream-blocked): cross-cluster/cross-UID restore,
  KEK rekey automation, RPO/RTO envelopes, removing `TransportExperimental`
  status, online-backup completion contract.
- Must not break existing v1alpha1/v1beta1 CRD storage versions.
- Must not introduce `#[allow]` to silence clippy.
- Must not hand-edit generated CRDs or examples.

## Success measures

- `make lint` passes with zero warnings.
- `cargo test --workspace` passes (unit + integration, excluding e2e).
- `helm unittest charts/kaniop` passes.
- `make check-e2e-shards` reports all shards with expected test counts.
- `make crdgen` and `make examples` produce no drift.
- Alert definitions in `prometheusrules.yaml` reference only shipped metric names.

## Open questions

None at this time. All decisions are captured in `CONTRACT.md`.
