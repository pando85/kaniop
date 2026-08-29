# ADR-0001: Production Kanidm backup and restore orchestration

## Status

Proposed

## Date

2026-08-22

## References

- [Kaniop issue #434](https://github.com/pando85/kaniop/issues/434)
- [Implementation plan](../plans/production-kanidm-backup-and-restore.md)
- [Kanidm issue #3816](https://github.com/kanidm/kanidm/issues/3816)

## Context

Kaniop manages the lifecycle of Kanidm servers, persistent storage and
replicated topologies inside Kubernetes.

Kanidm owns its database format and provides:

- scheduled online logical backups;
- offline logical backup and restore commands;
- offline database verification;
- its own replication protocol.

Kaniop must not implement another database serialization format, copy an active
SQLite database, or infer point-in-time recovery semantics from storage-level
operations.

The implementation already configures local online backups and includes a
restart-safe `KanidmRestore` controller. The restore controller validates the
target UID and pinned image, prevents concurrent restores, quiesces Kanidm,
waits for volume detach, runs restore and verification Jobs, starts the primary
first and rebuilds secondary replicas. It also persists a
`database_mutation_started` boundary and fails closed after crossing it.

Local backups stored on the same PVC as the database do not protect against
loss of that PVC or Kubernetes cluster. Production disaster recovery therefore
requires durable off-cluster storage, a catalog and a restore path that does not
depend on the original data volume.

Two upstream constraints shape the design:

1. Kanidm has no online "backup now" API, CLI operation or signal. Its online
   backup is driven only by its internal cron schedule.
2. Kanidm creates an online backup using its final filename and writes into that
   file. It does not currently expose a documented atomic rename, completion
   marker or event that an external data mover can use as an unambiguous
   completion signal.

A size or mtime stability interval, gzip/JSON parsing and repeated checksums are
useful experimental safeguards, but they are not a production completion
contract.

Restore is more dangerous than backup because it intentionally replaces live
identity state. It requires an explicit, auditable Kubernetes resource and must
not happen as an incidental effect of normal reconciliation.

## Decision

### Scope and delivery order

Kaniop adopts a restore-first production scope.

The first production-supported release provides:

- S3-compatible backup repositories;
- a Kubernetes catalog of committed remote backups;
- remote restore from a cataloged backup;
- an offline, remote and verified safety backup before restore mutation;
- same-version restore and offline database verification;
- primary-first recovery of replicated installations;
- deterministic retention and garbage collection;
- metrics, alerts, Events and runbooks.

Scheduled online transport through a Kaniop sidecar is implemented only behind
an explicit experimental feature gate until Kanidm supplies a documented
completion contract. This experimental work is not on the critical path for
production remote restore.

### Resource model

Kaniop uses four namespaced resources:

1. `KanidmBackupRepository` describes one S3-compatible destination,
   authentication modes, server-side encryption and transport limits.
2. `KanidmBackupSchedule` is the single source of the Kanidm online backup cron,
   local retention and remote retention policy.
3. `KanidmBackup` is immutable evidence and catalog metadata for a committed
   remote backup. It is not a promise that Kaniop can trigger an online backup
   immediately.
4. `KanidmRestore` represents one immutable, destructive restore request.

All references are namespace-local in the initial API. Resource names alone are
not sufficient identity: manifests and restore requests include Kubernetes UIDs.

Only one `KanidmBackupSchedule` may target a given `Kanidm` at a time. This
prevents conflicting values for the single `[online_backup]` configuration.
The Schedule's `suspend` control is mutable; identity, repository and scheduling
policy become immutable after the first committed backup unless a later API
explicitly defines safe migration.

A `(bucket, prefix)` is owned by one `KanidmBackupRepository`. Reusing a prefix
through another Repository is unsupported because Kubernetes cannot reliably
enforce uniqueness across credentials or clusters. The object layout adds the
namespace UID and Kanidm UID below that prefix.

### Canonical backup representation

The canonical payload is an unmodified, full Kanidm-native logical backup.
Kaniop never parses and rewrites the payload.

Every remote backup has:

- an immutable random backup ID;
- one immutable payload key;
- one manifest key;
- source namespace UID, Kanidm UID and domain;
- Kanidm version and preferably the exact image digest;
- payload size and SHA-256;
- consistency mode and reason;
- encryption metadata needed for recovery;
- manifest compatibility metadata.

Incremental backup, PITR and parent-child chains are not part of this decision.
They require a future supported Kanidm primitive and a new ADR.

### Remote commit protocol

The data mover uploads the payload first and `manifest.json` last. The manifest
is the logical commit record.

A payload without a manifest is staging, not a usable backup. A manifest whose
payload is absent or does not match its checksum is invalid. Payload and
manifest keys are immutable and never reused.

Confirmation uses a direct read or `HEAD` by exact key rather than relying on
listing consistency. Multipart uploads are completed or aborted explicitly.
Retries use the same backup ID and converge without creating another logical
backup.

An S3-compatible backend must pass a capability probe for the operations and
semantics required by this protocol. Compatibility by name alone is not a
support guarantee.

### Catalog and discovery

The online data mover does not receive Kubernetes API credentials. It writes the
payload and manifest only.

The operator uses a repository reader identity to discover committed manifests,
validates them and creates or reconciles `KanidmBackup` resources. The catalog
is derived and reconstructable. Deleting a catalog CR does not delete the
remote objects.

A restore safety backup is initiated by its `KanidmRestore` controller and is
also represented as a `KanidmBackup` after commit. The controller knows its
exact manifest key, so restore progress does not depend on object listing.

### Online backup transport

`KanidmBackupSchedule.spec.schedule` is the only cron authority. Kaniop renders
it into Kanidm's `[online_backup]` configuration. The operator does not create a
pending execution at each cron tick because it cannot trigger or reliably
correlate an online Kanidm backup.

The experimental data mover runs only as a sidecar of the designated primary
Pod and observes Kanidm's local backup directory. It never generates or mutates
the Kanidm backup and has no Kubernetes token.

Online transport becomes production-supported only after Kanidm documents and
Kaniop tests a minimum-version completion contract such as:

- atomic rename from a temporary filename after successful close;
- a completion marker written after close;
- an authenticated event/API returning a completed path; or
- native upstream object-storage shipping with equivalent commit semantics.

Until then, Kaniop must not report production backup success based only on file
stability heuristics.

### Pre-restore safety backup

A normal restore requires a fresh safety backup of the current database before
any database mutation.

Because Kanidm cannot be asked to create an online backup immediately, Kaniop:

1. performs all remote-source and image preflight checks;
2. pauses schedules and incompatible reconcilers;
3. scales every Kanidm server to zero;
4. waits for Pods to terminate and volumes to detach;
5. runs `kanidmd database backup` using the target's exact version against the
   primary PVC;
6. uploads the resulting logical backup to the selected Repository;
7. commits and verifies its remote manifest;
8. records the resulting `KanidmBackup` reference;
9. downloads and verifies the requested restore source;
10. only then persists `database_mutation_started=true` and starts restore.

The safety backup and restore share one downtime window. Kanidm is not restarted
between them.

The Repository owns a minimum retention period for restore safety backups. A
Schedule does not own this retention because safety backups are created by
Restore. Deleting a Restore never cascades to its safety backup.

### Break-glass

A narrowly scoped break-glass option may skip only the safety backup. It does
not skip target UID, namespace, domain, checksum, version, image, quiesce,
volume-detach, offline restore or database verification checks.

Break-glass requires a non-empty reason and approver, dedicated authorization
where Kubernetes RBAC and admission controls permit it, a Warning Event, a
Condition, a metric and a structured log. Kubernetes audit logs remain the
authoritative record of who submitted or changed the request.

### Restore state machine

A remote restore executes these phases:

```text
Pending
  -> Validating
  -> Quiescing
  -> SafetyBackup
  -> PreparingSource
  -> RestoringPrimary
  -> Verifying
  -> RebuildingReplicas
  -> Resuming
  -> Completed
```

A failure enters `Failed` and records Conditions and Events.

Before the mutation boundary, failure restores the original replica counts and
resumes paused reconciliation. After the boundary, Kaniop does not automatically
return an unknown database state to service. The target remains offline and in
maintenance until an administrator takes an explicit recovery action or the
controller proves the restore valid.

The source payload is downloaded and verified before crossing the mutation
boundary. The restore Job uses the exact Kanidm image recorded and approved by
preflight. `database verify` runs while the server remains offline.

### Replicated restore

The restored primary is the only authoritative recovery seed. Secondary
volumes can contain state newer than the selected recovery point and must not
rejoin unchanged.

Kaniop removes or reprovisions secondary database state, starts one primary,
performs readiness and semantic smoke checks, then rebuilds secondaries through
the supported Kanidm replication path. This path is production-supported only
after end-to-end validation against every supported replicated topology.

### Retention and deletion

Retention works only from committed manifests and catalog metadata. It never
identifies valid backups by listing old payload objects.

A backup is protected when it is:

- selected by `keepLast`, daily, weekly or monthly retention;
- younger than `minAge`;
- referenced by an active restore;
- a safety backup younger than the Repository's safety retention;
- protected by provider retention or Object Lock.

Garbage collection first withdraws the logical commit by deleting the manifest,
then deletes payload and staging objects. Deletion is executed with a separate
deleter identity. Object Lock refusal results in `DeletionDeferred`, not backup
failure.

Orphan payloads and multipart uploads use a separate, conservative staging TTL.

### Security and trust boundaries

Backup data is classified as highly sensitive identity data.

The preferred authentication order is workload identity, dynamic external
credentials, then namespace-local Kubernetes Secrets. Inline credentials are
forbidden. There is no `insecureSkipVerify` API. Private endpoints use a
referenced CA bundle.

Logical roles are separated:

- writer: creates immutable objects under one Kanidm prefix, without delete;
- reader: reads manifests and payloads, without write or delete;
- deleter: deletes only objects approved by retention;
- restore reader: reads one selected backup prefix where the provider supports
  that scope.

Jobs do not mount Kubernetes service account tokens. Containers use restricted
security contexts and resource limits.

`providerKms` means provider-side server-side encryption such as SSE-KMS. Kaniop
does not handle plaintext KMS keys or perform client-side envelope encryption in
the MVP. Encryption-key retention and exact Kanidm image retention are part of
the disaster-recovery runbook.

Kubernetes workload identity commonly belongs to a Pod rather than an
individual container. Mounting projected credentials only in the sidecar
reduces accidental exposure but does not turn containers in one Pod into a hard
security boundary. IAM policy therefore assumes compromise of the primary Pod
may expose writer capability.

SHA-256 protects against accidental corruption, not an attacker allowed to
replace both payload and manifest. Immutable keys, overwrite denial, split
roles and provider Object Lock reduce this risk. Cryptographic manifest signing
is required in a future threat model where the storage writer is not trusted.

### Manifest compatibility

Manifests have an explicit API version. An operator reads all older manifest
versions it still declares supported and never rewrites a committed manifest.
Unknown newer versions are not cataloged as `Ready`; they produce
`ManifestVersionTooNew` diagnostics.

Schema evolution adds optional fields within a version. Breaking changes use a
new manifest API version. `minimumManifestReader` is advisory compatibility
metadata and does not replace explicit parser support.

The restore remains constrained to the Kanidm version that created the backup.
Long-lived retention therefore requires retaining or mirroring exact server
images and preserving access to the relevant encryption keys.

### Observability

Kaniop exposes bounded-cardinality metrics for:

- backup phase duration, bytes and failures;
- last committed backup and effective backup age;
- repository/discovery failures;
- retention deletion and deferral;
- restore attempts, phase duration and outcomes;
- safety backup duration;
- break-glass use.

Backup IDs are not Prometheus labels. Events contain IDs and reasons but no
credentials, signed URLs or payload content.

The product reports observed RPO and RTO. It does not promise fixed values until
benchmarks establish supported size, bandwidth and topology envelopes.

## Consequences

### Benefits

- Kanidm remains authoritative for database consistency and format.
- A committed backup survives loss of the original PVC and cluster.
- Restore has a verified rollback point before destructive mutation.
- Catalog state can be reconstructed from remote manifests.
- The primary Pod never needs Kubernetes API write permissions for transport.
- The API can later delegate transport to native Kanidm S3 support without
  changing the catalog or restore model.
- Unsupported online completion heuristics are not presented as production
  guarantees.

### Costs

- A safety backup extends restore downtime.
- Kaniop must own and secure an S3 data mover and manifest protocol.
- Restore-first delivery does not immediately provide production scheduled
  offsite backups.
- In-place restore remains riskier than a future blue/green PVC restore.
- Repository capability differences require an explicit support matrix.
- Operators must retain KMS access and exact Kanidm images for the backup
  retention horizon.

## Rejected alternatives

### Treat local backups as disaster recovery

Rejected because loss of the PVC or cluster loses both the database and backup.

### Publish online backups after a stable-size interval

Rejected as a production contract because Kanidm writes directly to the final
filename and exposes no unambiguous external completion signal.

### Create an online backup on demand

Rejected because Kanidm currently has no supported online trigger. Temporarily
rewriting cron schedules is not a correctness protocol.

### Run every scheduled backup offline

Rejected as the default because it introduces planned authentication downtime
for every backup. It may be proposed later as an explicit policy, but is not
needed for restore-first delivery.

### Raw PVC snapshots as the canonical backup

Rejected because snapshot support is provider-specific and does not add Kanidm
application-consistency semantics. CSI snapshots remain a possible later layer.

### Restore without a safety backup

Rejected as the normal flow because an in-place failure could destroy the only
current state. The narrow break-glass path exists for already-corrupt or
unreadable source volumes.

### Give the sidecar Kubernetes API credentials

Rejected because remote commit followed by reader discovery provides a smaller
trust boundary and restart-safe catalog reconstruction.

### Let catalog deletion delete remote data

Rejected because the catalog is derived state. Remote deletion is an explicit,
policy-controlled GC operation with separate credentials.

### Restore every replica independently

Rejected because stale or newer secondary state could re-enter the restored
cluster. One restored primary is the authoritative recovery seed.

### Implement incremental backup or WAL shipping

Rejected until Kanidm exposes a supported primitive with explicit consistency
and recovery semantics.

### Support all object stores in the first release

Rejected to keep the initial security and compatibility surface bounded. The
MVP supports S3-compatible repositories that pass capability validation.

## Update — 2026-08-29

The experimental transport sidecar described in this ADR is now implemented.
The `data-mover transport` command runs as a sidecar in the primary Kanidm
StatefulSet when a non-suspended `KanidmBackupSchedule` targets the Kanidm and
its `KanidmBackupRepository` is Ready. Completion safety relies on a minimum
file age threshold and two-scan size stability (Kanidm still has no upstream
completion contract). Backup IDs are deterministic (UUIDv7 from filename
timestamp) and manifest commits are conditional, making uploads idempotent and
restart-safe. Local pruning remains with Kanidm `versions`. Discovery
reconciles committed manifests into `KanidmBackup` CRs on a configurable
cadence (`BACKUP_DISCOVERY_SCAN_INTERVAL_SECS` /
`BACKUP_DISCOVERY_STALE_SECS`). The `TransportExperimental` condition
remains in effect. See
[backup-transport.md](../plans/backup-transport.md) for the full design.
