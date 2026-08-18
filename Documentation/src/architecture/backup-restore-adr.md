# ADR: Kanidm backup and restore orchestration

Status: Proposed
Issue: pando85/kaniop#434

## Context

Kaniop manages the lifecycle of Kanidm servers inside Kubernetes.

Kanidm provides its own database, backup format, online backup scheduler,
offline restore command, and replication protocol. Kaniop should not implement
a second database serialization format.

The backup system has two separate responsibilities:

1. Produce a database-consistent backup artifact.
2. Place that artifact in a durable failure domain and safely restore it.

Kanidm owns responsibility (1). Kaniop owns the Kubernetes orchestration
around both responsibilities.

A restore is more dangerous than a backup because it intentionally destroys
or replaces live identity state. Restore therefore requires an explicit,
auditable Kubernetes resource and must not happen as an incidental side
effect of normal Kanidm reconciliation.

## Decision

### Backup representation

The canonical Kaniop backup is a Kanidm-native logical backup.

Physical volume snapshots are an optional second layer. They are not the
canonical restore format and must never be represented as equivalent to a
Kanidm-native logical backup.

Kaniop does not implement PostgreSQL backup semantics, WAL archiving, or PITR.

### Backup execution

Kaniop configures Kanidm's native online backup scheduler.

Exactly one server in a replicated Kanidm installation produces scheduled
logical backups. The existing Kaniop primary node is the default backup node.

The backup directory is a fixed operator-controlled location under the
Kanidm data volume. The path is not user-controlled in v1.

The initial implementation supports local persistent storage. S3-compatible
shipping is added by mapping Kaniop configuration to Kanidm native S3 backup
configuration after upstream Kanidm provides a supported interface.

Kaniop will not initially implement its own S3 uploader, restic repository,
or object-store lifecycle engine.

### Consistency guarantee

An online backup represents the database state of the selected Kanidm server.

Kaniop does not claim that it represents a globally linearizable
point-in-time state of every replicated server.

No incremental or point-in-time-recovery guarantee is exposed unless Kanidm
itself introduces a supported primitive with those semantics.

### Retention

Local logical backup retention maps to Kanidm's native versions setting.

Remote-object retention is delegated to the object-storage service by
default. Kaniop does not run destructive bucket-wide pruning.

Documentation recommends separate retention horizons, for example short local
retention and longer remote retention.

### Restore API

A new namespaced KanidmRestore CRD represents a single destructive restore
operation.

Its spec is immutable.

The restore target contains both a Kanidm resource name and the expected
Kubernetes UID. A restore may not continue if the UID does not match, which
prevents a stale restore object from operating on a newly-created resource
with the same name.

The restore source identifies a single logical backup artifact.

The restore operation requires an explicit, immutable Kanidm image/version
compatible with the backup. Mutable image references such as `latest` are not
accepted for a restore operation.

### Restore state machine

A restore executes these phases:

Pending
Validating
Quiescing
RestoringPrimary
Verifying
RebuildingReplicas
Resuming
Completed

A failure transitions to Failed and records a Kubernetes condition and Event.

Before changing data, Kaniop marks the target Kanidm as under restoration.
All Kanidm identity-resource reconcilers must then stop writing to that
Kanidm instance.

Kaniop scales the Kanidm servers to zero and waits for the pods and volume
attachments to terminate before mounting the data volume in a restore Job.

The restore Job uses the exact Kanidm image declared by the restore request,
mounts the target data volume, and invokes Kanidm's database restore command.

After restore, Kaniop invokes Kanidm database verification while the server
remains offline.

Only after verification succeeds may the primary server start.

### Replicated restore

A replicated topology must not restart secondary databases that contain state
newer than the selected recovery point, because that state could replicate
back into the restored primary.

The full-cluster restore algorithm therefore treats the restored primary as
the only authoritative recovery seed.

Secondary database state is replaced/reset before secondaries are allowed to
rejoin. They are then populated using Kanidm's supported refresh/replication
mechanism from the restored primary.

This path is considered production-supported only after an upstream-compatible
procedure has been validated in Kaniop end-to-end tests.

### Failure behavior

Restore is fail-closed.

After database mutation has started, Kaniop does not automatically return the
old workload to service following an unknown restore failure.

The target remains in maintenance/restoring state until the controller proves
that the database is verified or an administrator performs an explicit
recovery action.

Controller restart is safe: phase transitions are derived from Kubernetes
objects and deterministic child-resource names, not process memory.

### Security

Backup data is treated as highly sensitive.

Credentials are never embedded directly in a Kanidm or KanidmRestore object.
Object-storage credentials, when supported, are referenced by Kubernetes
Secret.

Secret references may not cross namespaces.

Restore Jobs have automountServiceAccountToken=false unless a concrete
Kubernetes API requirement is introduced.

The S3 default requires TLS certificate verification. Custom endpoints may
supply a CA bundle; disabling TLS verification is not part of the public API.

For Amazon S3 deployments, documentation recommends encryption at rest,
versioning, Lifecycle retention, and optionally Object Lock. Kaniop does not
provision or own buckets or KMS keys.

### Isolation

A KanidmRestore may target a Kanidm resource only in its own namespace.

Remote backup object prefixes include namespace, immutable Kanidm UID, and
backup generation/version information so Kubernetes name reuse cannot collide
with historical backups.

A full database restore is the only backup-based granularity in v1.

Single-object recovery is performed through Kaniop's declarative resources or
Kanidm's own recycle-bin functionality where applicable.

### Observability

Kanidm and KanidmRestore status conditions expose configuration and restore
state.

Kaniop publishes metrics for restore attempts, failures and duration.

Kaniop does not publish a "last successful backup" metric unless it has a
reliable completed-backup signal from Kanidm or the storage transport.
Configuration success is not reported as backup success.

### Compatibility

Adding an optional backup field to Kanidm v1beta1 is backward-compatible for
existing resources.

KanidmRestore is a new CRD.

No SQL or internal Kaniop database migration is required.

Generated CRDs and examples are regenerated using the existing Kaniop build
targets.

### Consequences

Benefits:

- Kanidm remains the authority on its database format.
- Online scheduled backups do not require planned outages.
- Restore is explicit, auditable and restart-safe.
- Kaniop does not duplicate an S3 client before Kanidm's upstream design is
  stable.
- Storage snapshots remain available as an independent DR layer.
- The API does not promise unsupported PITR or cross-version behavior.

Costs:

- Initial remote S3 support remains dependent on Kanidm upstream.
- Full restore necessarily causes downtime.
- HA restore requires destructive handling of secondary database state.
- Reliable backup-success metrics require an upstream completion signal or a
  future backup catalog mechanism.

## Rejected alternatives

### Raw PVC snapshots as the only backup

Rejected because Kubernetes snapshots do not add application-consistency
semantics, storage support is CSI/provider-specific, and the artifact remains
coupled to the physical database representation.

### Run kanidmd database backup from a Kubernetes CronJob

Rejected as the default because the documented manual backup operation
requires the server to be stopped, introducing an outage for every scheduled
backup even though Kanidm already supports online backups.

### Implement an S3 uploader in the Kaniop operator

Rejected initially because it duplicates a feature already being designed
upstream, expands Kaniop's credential/security surface, and would couple the
operator process to bulk data transfer.

### Embed restic or Velero in Kaniop

Rejected. They are useful infrastructure backup systems, but neither should
replace Kanidm's application-aware backup format. They remain supported as
external complementary DR systems.

### Restore every replica independently from the same backup

Rejected. A replicated server can contain node/replication state, and
restarting multiple independently restored replicas risks an unsupported
topology. Restore one canonical seed and rebuild consumers instead.

### Incremental/PITR API implemented by Kaniop

Rejected until Kanidm offers a native primitive. Kaniop must not manufacture
database semantics from storage-level deltas.
