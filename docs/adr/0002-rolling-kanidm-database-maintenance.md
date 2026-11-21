# ADR-0002: Rolling Kanidm database maintenance with init containers

## Status

Proposed

## Date

2026-08-31

## References

- [Kaniop issue #950](https://github.com/pando85/kaniop/issues/950)
- [Implementation plan](../plans/maintenance-operations-design.md)
- [Kanidm database maintenance documentation](https://kanidm.github.io/kanidm/stable/database_maintenance.html)
- [ADR-0001: Production Kanidm backup and restore orchestration](0001-production-kanidm-backup-and-restore.md)

## Context

Kanidm exposes reindex, vacuum and verification as offline database operations.
The server using a database must be stopped while these commands run.

A Kubernetes operator should not interpret that requirement as "stop the whole
Kanidm topology". In a replicated deployment the useful availability contract
is different from restore: take one replica offline, maintain its local
database, return it to service, and only then continue with another replica.
At least one suitable replica should remain serving whenever the topology makes
that possible.

This is fundamentally different from restore. Restore intentionally replaces
identity state and may need a cluster-wide quiescence boundary. Database
maintenance is local to an existing replica and is intended to preserve its
logical database contents. Reusing restore's scale-to-zero/PVC/Job mechanism as
the default maintenance path would throw away the availability provided by
replication.

A StatefulSet does not provide an API to "scale down ordinal 1 but keep ordinal
2". Reducing `spec.replicas` always removes the highest ordinal. Deleting a Pod
is different: the StatefulSet recreates exactly that ordinal and reattaches its
existing PVC. That gives Kaniop a natural per-replica restart primitive.

An external maintenance Job is awkward for the same reason. Kaniop would have
to stop a StatefulSet-owned Pod, prevent the StatefulSet from recreating it,
wait for volume detach, transfer the PVC to a Job, then restore ownership. This
is possible, but it makes Kubernetes object ownership and volume attachment part
of the critical path even though the operation only needs exclusive access to
the database while the server process is stopped.

An init container has the opposite property. It runs in the replacement Pod,
with the same PVC, before the Kanidm server container starts. The database is
therefore offline for that replica without detaching the volume or changing
StatefulSet ownership.

The existing proposed maintenance plan also explored a server-native drain and
replication-fence protocol. Such a protocol could provide stronger consistency
proofs in the future, but Kaniop should not require a new Kanidm control-plane
API merely to orchestrate the offline maintenance commands that Kanidm already
provides.

## Decision

### Maintenance is a first-class one-shot operation

Kaniop will expose one immutable `KanidmMaintenance` resource rather than a
separate CRD for each database command.

The initial operations are:

- `Reindex`;
- `Verify`;
- `Vacuum` only after its interruption/retry behaviour passes the safety gate
  described below.

The resource targets one `Kanidm` by name and UID. The default target is all
replicas. A specific replica may be selected by Kaniop replica-group name and
ordinal. Arbitrary label selectors are not part of the API.

Recurring schedules and automatic log-triggered maintenance are not part of
this decision.

### Availability is the primary orchestration invariant

Kaniop performs maintenance on at most one replica at a time.

Before intentionally restarting a replica, Kaniop verifies that taking it out of
service will not reduce the topology below the required serving capability. At
minimum, a write-capable replica must remain Ready when maintaining another
write-capable replica. Single-replica maintenance requires an explicit
`allowDowntime` request.

The guarantee is deliberately phrased as an operator action guarantee:

> Kaniop will not intentionally take another replica out of service while the
> current maintenance target has not successfully returned to service, and it
> will not intentionally reduce service below the configured maintenance
> availability requirement.

Kaniop cannot guarantee availability against an unrelated node, network or
storage failure that occurs while one replica is already under maintenance.

### Use per-replica Pod replacement, not StatefulSet scaling

For a selected replica Kaniop:

1. persists the selected target in `KanidmMaintenance.status`;
2. publishes a maintenance plan identifying the operation UID, operation and
   exact Pod name;
3. deletes exactly that Pod;
4. lets its owning StatefulSet recreate the same ordinal with the same PVC;
5. observes the maintenance init container and replacement Pod;
6. waits for the Kanidm server to become Ready again;
7. records the replica as completed;
8. only then selects the next replica.

Kaniop does not scale the StatefulSet down and does not move the PVC to a Job.

Replica ordering is deterministic. Non-primary replicas are maintained before
the configured primary node; within a StatefulSet higher ordinals are preferred
before ordinal zero. The controller recomputes availability before every Pod
deletion rather than assuming the topology stayed healthy since planning.

### Every managed Pod has a normally no-op maintenance init path

The StatefulSet template contains an operator-managed maintenance bootstrap and
maintenance init path in normal operation. This avoids mutating the Pod template
for each maintenance request and avoids an automatic StatefulSet-wide rollout.

The maintenance plan is supplied through an optional operator-managed ConfigMap.
When no plan exists, the init path exits successfully without touching the
database. During maintenance the plan contains at least:

```json
{
  "version": 1,
  "operationId": "<KanidmMaintenance UID>",
  "operation": "reindex",
  "targetPod": "example-default-2"
}
```

Only the replacement Pod whose name matches `targetPod` executes the database
command. Other Pods, including an unrelated Pod recreated by Kubernetes while a
maintenance operation is active, take the no-op path.

The plan ConfigMap is optional from the Pod's perspective. Missing maintenance
control state must never prevent an ordinary Kanidm Pod from starting.

### Execute the database command from the exact Kanidm image

The maintenance command runs in an init container using the same Kanidm image
as the server container for that replica. Kaniop must not reimplement Kanidm's
database format or link a different Kanidm version into a maintenance utility.

Because the Kanidm image is intentionally minimal and cannot be assumed to
contain a shell, conditional orchestration is implemented by a small,
statically linked Kaniop maintenance runner. A tiny bootstrap container copies
the runner into a shared `emptyDir`; the Kanidm-image init container executes
that runner, which validates the plan and then invokes the image's own `kanidmd
database <operation>` binary.

The runner has no Kubernetes API credentials. It receives only the Pod identity,
the read-only maintenance plan, the generated Kanidm configuration and the
replica's data volume.

The generated Kanidm configuration init container must complete before the
maintenance init container.

### Persistent storage is required

Rolling maintenance by Pod replacement is valid only when the database survives
the replacement Pod. Kaniop therefore rejects this strategy for `EmptyDir` and
Pod-scoped generic ephemeral storage.

The initial implementation supports the existing PVC-backed
`volumeClaimTemplate` storage path. Extending this to another storage mode
requires a separate proof that Pod replacement preserves the same database.

### Completion is durable and controller restarts are safe

The `KanidmMaintenance` UID is the operation ID.

The runner records successful completion on the replica's data PVC using an
atomic marker below an operator-owned directory such as:

```text
/data/.kaniop/maintenance/<operation-uid>.json
```

The marker identifies the operation, operation type, target Pod and successful
completion. It is written only after the Kanidm command exits successfully.

This marker complements Kubernetes status; it is not a substitute for it. Its
purpose is to make the local database's completion state survive:

- Kaniop restarts;
- Pod recreation;
- loss of an in-memory reconcile step;
- success followed by a controller crash before status was patched.

Kaniop status remains the user-visible state machine and records the current
replica, completed replicas, phase, timestamps and stable failure reasons.

### Failure stops progression

If maintenance on replica `N` fails, Kaniop does not touch replica `N-1`.

The failed replica remains outside normal service until the operation is
explicitly retried or abandoned according to the recovery semantics supported
for that operation. Other replicas continue serving.

A command failure and an interrupted process are different cases. The runner
must not blindly turn an ordinary non-zero command result into an unbounded loop
that repeatedly mutates the database. It records enough local state to
recognise a known failed attempt and fail closed.

There is an unavoidable crash window between a database mutation and writing a
Kaniop completion/failure marker. Kubernetes cannot provide exactly-once process
execution across node loss. Therefore Kaniop may enable an operation only when
that Kanidm command has been shown to be safe to retry after:

- successful completion;
- ordinary command failure;
- termination at arbitrary points in the operation.

`Verify` is read-only and is expected to satisfy this easily. `Reindex` must be
validated against the supported Kanidm versions before production enablement.
`Vacuum` remains disabled until the same interruption tests establish its safety
contract.

This safety gate is a prerequisite, not optional test coverage.

### Readiness is an availability gate, not a replication proof

After maintenance succeeds, Kaniop waits for the replacement Kanidm container
to become Ready and for the replica to remain healthy long enough to satisfy
normal rollout stability checks before moving on.

Kubernetes readiness does not prove that every acknowledged replication event
has converged to every peer. The initial maintenance implementation therefore
claims rolling service availability, not a new cross-replica consistency
protocol. This is no weaker than the assumption already made during an ordinary
planned Pod restart, and maintenance does not intentionally replace logical
identity data.

If Kanidm later exposes a stable replication fence/drain primitive, Kaniop may
add it as a stronger pre/post-maintenance gate without changing the
`KanidmMaintenance` API or the per-replica Pod/PVC execution model.

### Maintenance is mutually exclusive with topology-changing operations

An active maintenance operation owns the target Kanidm for disruptive topology
and database changes. Restore, another maintenance operation, image rollout,
replica-count changes and storage changes must not race it.

Normal status observation and non-conflicting reconciliation may continue.
Implementation should reuse/generalise the existing restore operation lock only
as far as required; building a broad operation framework is not a prerequisite
for the first maintenance release.

## Consequences

### Benefits

- Replicated Kanidm can remain available while each local database is maintained.
- There is no full-topology scale-to-zero window.
- No maintenance Job needs to acquire a StatefulSet-owned PVC.
- No `VolumeAttachment` handoff is needed for the normal maintenance path.
- The exact Kanidm image owns database semantics.
- Failure is naturally isolated to one replica and progression stops there.
- Controller restart recovery is small compared with restore because the
  StatefulSet continues to own and reconstruct the Pod.
- A future native replication fence can strengthen correctness without replacing
  the API.

### Costs

- Normal Kanidm Pod startup gains a small operator-managed init path, including a
  maintenance-runner bootstrap image.
- Kaniop must publish/version the small runner image and make its pull policy and
  image selection predictable.
- Maintenance requires persistent per-replica storage.
- The controller must coordinate with normal StatefulSet reconciliation so a
  spec/image/topology change cannot invalidate the active plan.
- The runner and marker format become a small internal compatibility surface.

### Risks and mitigations

**The maintenance runner image becomes an extra Pod-start dependency.** The plan
ConfigMap is optional, the runner image must be small and version-pinned, and
`IfNotPresent` should be used so the no-maintenance path remains cheap after the
image is cached. E2E tests must include ordinary Pod recreation while no
maintenance CR exists.

**A second failure can occur while one replica is intentionally offline.** Kaniop
rechecks healthy serving capacity immediately before every Pod deletion and
never takes a second replica down until the previous target has returned.

**Readiness may precede full replication convergence.** The initial contract is
availability, not replication fencing. A future server-native fence can be
inserted as an additional gate.

**A node can die while `kanidmd database <operation>` is running.** No Kubernetes
or controller mechanism can manufacture exactly-once execution. Supported
operations must pass explicit kill/retry tests; unsafe commands are not exposed.

**A stale maintenance plan could be observed by a later Pod recreation.** Plans
are scoped to an operation UID and exact Pod name, successful replicas carry a
PVC completion marker, and the plan is removed when the operation terminates.

## Rejected alternatives

### Quiesce the whole topology and run maintenance Jobs

This reuses restore machinery but unnecessarily discards HA for an operation
that is local to each replica. It remains a possible break-glass/manual fallback,
not the default architecture.

### Scale down one replica of a StatefulSet

StatefulSet replica scaling removes the highest ordinal and cannot independently
suspend an arbitrary member while leaving higher ordinals managed. It is not an
appropriate per-replica maintenance primitive.

### Delete a Pod and let an external Job claim its PVC

The StatefulSet immediately recreates the deleted Pod, creating an ownership and
volume-mount race. Preventing that race requires more invasive StatefulSet
manipulation than running maintenance before the replacement server starts.

### Trigger a normal StatefulSet rollout with a temporary maintenance init container

This can serialize replicas, but removing the temporary init container requires
another template revision/rollout or leaves future Pod recreations able to rerun
the maintenance command. It also gives the StatefulSet controller more authority
over progression than Kaniop needs. Explicit per-Pod deletion plus a normally
no-op gate is easier to stop and reconstruct.

### Require a new native Kanidm maintenance protocol first

A native drain/fence/idempotency API would be valuable, especially for stronger
replication guarantees, but making #950 depend on it repeats the mistake of
turning a Kubernetes lifecycle problem into a prerequisite server project. The
existing offline commands are sufficient if their interruption semantics are
safe.

### Separate CRDs for reindex, vacuum and verify

They share target selection, availability checks, Pod replacement, locking,
status and failure handling. Separate CRDs would duplicate the difficult parts.

### Label-selected targets and built-in schedules

Labels are too dynamic for selecting a database replica for a disruptive
operation, and periodic reindex/verify would imply routine maintenance semantics
that Kanidm does not document. Both can be reconsidered only after the one-shot
operation is proven in production.
