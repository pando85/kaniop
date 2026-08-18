# Backup and restore implementation plan

Issue: [#434](https://github.com/pando85/kaniop/issues/434)
ADR: [Kanidm backup and restore orchestration](../architecture/backup-restore-adr.md)

## Scope

This plan implements the ADR in dependency order. It deliberately separates the
parts Kaniop can implement today from the S3 transport that is still blocked on
Kanidm [#3816](https://github.com/kanidm/kanidm/issues/3816).

The first complete implementation provides:

- Kanidm-native scheduled online backups on exactly one primary node;
- persistent local backup storage under `/data/backups`;
- an explicit `KanidmRestore` API for one-shot destructive restores;
- fail-closed, restart-safe restore orchestration;
- same-target UID and same-version/image safety checks;
- quiescing of normal Kaniop writes while a restore is active;
- offline `kanidmd database restore` and `database verify` jobs;
- single-node restore and safe replicated restore by rebuilding secondary data;
- status, events, RBAC, metrics, documentation and end-to-end coverage.

Native S3 shipping is intentionally not reimplemented in Kaniop. Once Kanidm
#3816 provides a supported configuration, Kaniop can map a Secret-backed API to
that native facility in a follow-up change.

## Work packages

| Package | Primary changes | Dependency |
|---|---|---|
| ADR and API contract | ADR, `KanidmBackupSpec`, `KanidmRestore` contract | None |
| Native online backup | primary-only `[online_backup]`, storage validation, examples | API contract |
| Maintenance gate | shared check preventing identity writes during restore | Restore API |
| Single-node restore | controller, state machine, Jobs, PVC orchestration, verify | Maintenance gate |
| Replicated restore | restored primary as authoritative seed, clean secondary rebuild | Single-node restore |
| Security and RBAC | immutable restore spec, target UID, Job hardening, permissions | Restore controller |
| Observability | Conditions, Events and restore lifecycle metrics | Restore controller |
| E2E/chaos | recovery semantics, version mismatch, corruption, controller restart | Above |
| Native S3 adapter | map accepted Kanidm S3 configuration | Kanidm #3816 |

## API changes

### `Kanidm.spec.backup`

Add an optional backup policy:

```yaml
spec:
  backup:
    schedule: "0 2 * * *"
    versions: 7
```

The operator owns the local path and uses `/data/backups`. A local-only backup
configuration requires persistent storage. The online-backup stanza is rendered
only for the pod selected by the existing primary-node mechanism.

### `KanidmRestore`

Add a namespaced one-shot resource:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmRestore
metadata:
  name: my-idm-restore
spec:
  targetRef:
    name: my-idm
    uid: "<kanidm-kubernetes-uid>"
  source:
    local:
      fileName: "backup-2026-08-18T020000.json.gz"
  restoreImage: "kanidm/server@sha256:..."
```

The spec is immutable. `targetRef.uid` prevents a stale restore object from
operating on a newly-created `Kanidm` with the same name. `fileName` is a
validated basename, not a path. `restoreImage` must be pinned and must not use
`latest`.

## Restore state machine

```text
Pending
  -> Validating
  -> Quiescing
  -> RestoringPrimary
  -> Verifying
  -> RebuildingReplicas
  -> Resuming
  -> Completed
```

Any unsafe or unrecoverable transition enters `Failed`. Once database mutation
has started, failure is fail-closed: the target remains in restore/maintenance
state until the controller proves the restored database is valid or an
administrator takes an explicit recovery action.

### Validation

Before downtime, validate as much as possible:

- target exists in the same namespace;
- target UID matches;
- no other active restore targets the same Kanidm;
- persistent storage is configured;
- backup basename is safe;
- restore image is immutable/pinned;
- source exists where it can be checked before quiescing.

### Quiescing

Mark the target as restoring and prevent Person, Group, OAuth2 and Service
Account reconcilers from issuing writes. Scale Kanidm workloads to zero and wait
for pods to terminate before mounting the data volume in a restore Job.

### Restore and verification

Create deterministic Jobs with `automountServiceAccountToken: false` and
`backoffLimit: 0` that run the pinned Kanidm image against the primary PVC:

```text
kanidmd database restore -c /run/kanidm/server.toml /data/backups/<file>
kanidmd database verify -c /run/kanidm/server.toml
```

Do not resume service until verification succeeds.

### Replicated restore

The restored primary is the only authoritative recovery seed. Secondary
storage containing state newer than the selected recovery point must not be
allowed to rejoin unchanged. Reset/reprovision secondary database state, start
the primary, and rebuild consumers through the supported Kanidm
refresh/replication path before restoring normal traffic.

## Patch plan

### Documentation

- `Documentation/src/architecture/backup-restore-adr.md`
- `Documentation/src/architecture/backup-restore-plan.md`
- `Documentation/src/usage/backup-restore.md`
- `Documentation/src/SUMMARY.md`

### Backup API and configuration

- `libs/operator/src/kanidm/crd.rs`
  - add `KanidmBackupSpec` and optional `KanidmSpec.backup`;
  - document persistence and retention semantics.
- `libs/operator/src/kanidm/reconcile/statefulset.rs`
  - render native `[online_backup]` only on the existing primary node;
  - keep `/data/backups` operator-controlled.
- `cmd/examples/src/kanidm.rs`
  - add representative backup values.
- regenerate `charts/kaniop/crds/crds.yaml` and `examples/` using project tools.

### Restore API and controller

- add `KanidmRestore` CRD/status/controller modules under
  `libs/operator/src/kanidm/restore/`;
- export/start the controller from the existing operator entrypoints;
- add `KanidmRestore::crd()` to `cmd/crdgen`;
- add a generated restore example;
- use deterministic Job names and idempotent phase transitions;
- add finalizer handling for in-progress destructive operations.

### Reconciliation gate

Add a shared maintenance/write-allowed helper and apply it to the Person, Group,
OAuth2 and Service Account reconcilers so declared resources do not mutate the
Kanidm database while recovery is in progress.

### Helm/RBAC/monitoring

- allow the operator to watch/update `KanidmRestore` resources and manage the
  Jobs/PVC/StatefulSet operations needed by restore;
- expose restore attempts, failures and durations;
- emit Events/Conditions for state transitions and failures;
- do not claim a last-successful-backup metric until a reliable completion
  signal exists.

## Test plan

### Unit tests

Cover:

- backup serialization/defaults;
- primary-only online-backup config;
- persistent-storage validation;
- restore spec serialization and immutability schema;
- target UID validation;
- safe backup basename validation;
- pinned-image validation;
- deterministic Job names;
- restart-safe state transitions;
- Job security settings.

### Integration/controller tests

Cover missing targets, UID mismatch, concurrent restores, finalizer behavior,
Job success/failure, transient Kubernetes errors, controller restart at every
phase, volume-detach waiting, reconciliation gating and status conflicts.

### End-to-end tests

A recovery test must prove database semantics, not only file creation:

1. create a Kanidm cluster and wait for readiness;
2. create data A directly in Kanidm;
3. wait for a native backup;
4. create/change/delete data B after the backup;
5. create `KanidmRestore`;
6. assert workloads quiesce and identity reconcilers pause;
7. assert restore and verify complete;
8. for replicated clusters, assert stale secondary state is discarded and
   rebuilt from the restored primary;
9. assert data A is present and post-backup data B follows recovery-point
   semantics;
10. assert normal Kaniop reconciliation resumes.

Also cover version mismatch before quiescing, corrupt backup fail-closed
behavior, operator restart mid-restore, stale target UID, retention, and a
secondary containing a post-backup mutation.

## Acceptance criteria

- Existing `Kanidm` resources remain valid when `backup` is absent.
- With backup enabled, exactly one primary pod owns the online backup scheduler.
- Unsafe ephemeral local-backup configurations are rejected.
- Restore never mutates a same-name replacement target with a different UID.
- Version/image mismatch fails before database mutation.
- Restore is idempotent across operator restarts.
- A failed/corrupt restore never resumes unverified authentication service.
- Normal identity reconciliation is blocked for the target during restore and
  resumes only after successful recovery.
- Replicated recovery cannot reintroduce post-recovery-point state from a stale
  secondary.
- CRDs/examples are generated with the repository build targets and all lint,
  unit, integration and relevant e2e checks pass.

## Deferred: native S3 shipping

Do not add an operator-owned uploader, restic integration or guessed S3 config.
When Kanidm #3816 lands, add a follow-up adapter using Secret references,
namespace/UID-isolated object prefixes, TLS verification and an S3-compatible
in-cluster test fixture.
