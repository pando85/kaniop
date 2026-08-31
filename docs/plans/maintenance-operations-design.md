# Rolling Kanidm Database Maintenance Implementation Plan

Status: **Proposed**

Related:

- [ADR-0002: Rolling Kanidm database maintenance with init containers](../adr/0002-rolling-kanidm-database-maintenance.md)
- [Kaniop issue #950](https://github.com/pando85/kaniop/issues/950)
- [Kanidm database maintenance documentation](https://kanidm.github.io/kanidm/stable/database_maintenance.html)
- [ADR-0001: Production Kanidm backup and restore orchestration](../adr/0001-production-kanidm-backup-and-restore.md)

## Goal

Implement `KanidmMaintenance` as a restart-safe, one-shot Kubernetes operation
that performs Kanidm's offline database maintenance commands one replica at a
time while preserving service through the remaining healthy replicas whenever
the topology permits it.

The initial implementation should be intentionally smaller than backup/restore.
Kaniop is responsible for selecting a replica, ensuring enough service remains,
restarting exactly that Pod, supervising its maintenance init container and
waiting for it to return healthy before moving on. Kanidm remains responsible
for reindex, verify and vacuum database semantics.

## Architectural constraints

The implementation must preserve these invariants from ADR-0002:

1. At most one replica is intentionally unavailable for maintenance at a time.
2. A second replica is never selected until the current target has returned to
   service successfully.
3. Maintenance uses Pod replacement, not StatefulSet scale-down.
4. The target PVC remains owned by the StatefulSet Pod; no maintenance Job
   steals or remounts it.
5. The database command executes from the exact Kanidm image configured for the
   target replica.
6. Controller memory is never a correctness boundary.
7. A missing maintenance plan is a no-op and cannot prevent normal Pod startup.
8. `EmptyDir` and Pod-scoped ephemeral database storage are rejected.
9. Image, storage and topology changes cannot race an active maintenance
   operation.
10. Unsupported interruption semantics block an operation from being exposed,
    rather than being papered over with retries.

## Current state

Kaniop already has useful pieces, but maintenance should not inherit restore's
full orchestration model.

### StatefulSet generation

`libs/operator/src/kanidm/reconcile/statefulset.rs` already:

- creates one StatefulSet per `ReplicaGroup`;
- generates stable Pod names from replica-group name and ordinal;
- mounts the database as `kanidm-data` at `/data`;
- generates `/run/kanidm/server.toml` before the Kanidm server starts when
  replication/backup configuration is required;
- runs the main container from `Kanidm.spec.image`;
- uses normal StatefulSet ownership to recreate a deleted ordinal.

This is the core primitive required by rolling maintenance.

### Restore controller

`libs/operator/src/kanidm/restore.rs` already demonstrates:

- an immutable one-shot CRD;
- target UID pinning;
- a finalizer and restart-safe phases;
- Events, Conditions and metrics;
- a restore-specific operation marker preventing conflicting reconciliation.

Maintenance should reuse the patterns and small helpers that are actually
shared, not the restore quiesce/detach/Job state machine.

### Missing pieces

Kaniop does not currently have:

- a maintenance CRD/controller;
- an availability gate for intentionally restarting a database replica;
- a Pod-local maintenance runner;
- a durable per-PVC maintenance completion record;
- an operation lock understood by both normal reconciliation and maintenance;
- interruption/retry qualification tests for Kanidm's maintenance commands.

## V1 API

### `KanidmMaintenance`

Proposed shape:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmMaintenance
metadata:
  name: example-reindex
spec:
  targetRef:
    name: example
    uid: 01234567-89ab-cdef-0123-456789abcdef
  operation: Reindex
  target:
    allReplicas: {}
  allowDowntime: false
```

A specific replica is expressed semantically, not with labels:

```yaml
spec:
  targetRef:
    name: example
    uid: 01234567-89ab-cdef-0123-456789abcdef
  operation: Verify
  target:
    replica:
      replicaGroup: default
      ordinal: 2
```

Suggested Rust types:

```rust
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug)]
#[kube(
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmMaintenance",
    plural = "kanidmmaintenances",
    shortname = "idmmaintenance",
    namespaced,
    status = "KanidmMaintenanceStatus"
)]
pub struct KanidmMaintenanceSpec {
    pub target_ref: KanidmMaintenanceTargetRef,
    pub operation: KanidmMaintenanceOperation,
    #[serde(default)]
    pub target: KanidmMaintenanceTarget,
    #[serde(default)]
    pub allow_downtime: bool,
}

pub struct KanidmMaintenanceTargetRef {
    pub name: String,
    pub uid: String,
}

pub enum KanidmMaintenanceOperation {
    Reindex,
    Verify,
    Vacuum,
}

pub enum KanidmMaintenanceTarget {
    AllReplicas,
    Replica {
        replica_group: String,
        ordinal: u32,
    },
}
```

The spec is immutable with CEL validation, following `KanidmRestore`.

`Vacuum` may exist in the API only if Phase 0 qualifies it. If qualification is
not complete when the controller ships, omit the enum variant rather than
accepting a request that cannot be executed safely.

### Status

Keep the state machine smaller than restore:

```text
Pending
  -> Validating
  -> WaitingForAvailability
  -> PreparingReplica
  -> RestartingReplica
  -> Running
  -> WaitingForReady
  -> Completed

any phase -> Failed
```

`AllReplicas` loops through `PreparingReplica` to `WaitingForReady` for each
selected target.

Suggested status:

```rust
pub struct KanidmMaintenanceStatus {
    pub observed_generation: Option<i64>,
    pub observed_target_uid: Option<String>,
    pub phase: KanidmMaintenancePhase,
    pub operation_id: Option<String>,
    pub current_target: Option<MaintenanceInstanceRef>,
    pub completed_targets: Vec<MaintenanceInstanceRef>,
    pub started_at: Option<Time>,
    pub completed_at: Option<Time>,
    pub conditions: Vec<Condition>,
}

pub struct MaintenanceInstanceRef {
    pub replica_group: String,
    pub ordinal: u32,
    pub pod_name: String,
    pub pvc_name: String,
    pub replacement_pod_uid: Option<String>,
}
```

Do not copy per-phase internal state into status unless it is required to resume
correctly after controller restart.

Recommended stable Conditions/reasons:

```text
Accepted
AvailableCapacity
ReplicaPrepared
MaintenanceSucceeded
ReplicaReady
Completed
Failed

TargetNotFound
TargetUidMismatch
UnsupportedStorage
InsufficientAvailableReplicas
ConflictingOperation
TopologyChanged
ImageChanged
PlanConflict
PodDeletionFailed
MaintenanceCommandFailed
MaintenanceInterrupted
ReplacementPodFailed
ReplicaNotReady
Timeout
```

## Runtime design

### Maintenance plan ConfigMap

Use one optional ConfigMap name derived from the Kanidm resource, for example:

```text
<kanidm-name>-maintenance-plan
```

It is created only while maintenance is active and mounted into all managed Pods
with `optional: true`.

Store one small versioned JSON document:

```json
{
  "version": 1,
  "operationId": "<maintenance-uid>",
  "operation": "reindex",
  "targetPod": "example-default-2"
}
```

Properties:

- exact Pod name, not a selector;
- operation UID scopes stale data;
- update the plan before deleting the selected Pod;
- delete the plan after completion/cancellation;
- owner-reference it to `KanidmMaintenance` while the finalizer guarantees safe
  cleanup;
- do not put mutable progress into the ConfigMap: status/PVC markers own that.

### Pod wiring

Add two operator-managed init containers and two volumes to managed StatefulSets.
Names are reserved and must not be user-overridable.

```text
user/existing init containers
        |
        v
kanidm-generate-replication-config   (when required)
        |
        v
kaniop-maintenance-bootstrap
        |
        v
kanidm-maintenance
        |
        v
kanidm server
```

Volumes:

```text
kaniop-maintenance-plan   optional ConfigMap, read-only
kaniop-maintenance-tools  EmptyDir
kanidm-data               existing database volume
kanidm-config             existing generated config when used
```

`kaniop-maintenance-bootstrap` uses a small Kaniop runner image and copies a
statically linked `kaniop-maintenance-runner` into the tools `EmptyDir`. The
runner binary should support a self-install/copy mode so the bootstrap image
does not depend on shell/coreutils behaviour.

`kanidm-maintenance` uses exactly `Kanidm.spec.image`, mounts the same database
and generated configuration needed by the server, and executes:

```text
/kaniop-maintenance-tools/kaniop-maintenance-runner run
```

The runner then invokes the Kanidm image's own binary, conceptually:

```text
kanidmd database reindex -c /run/kanidm/server.toml
kanidmd database verify  -c /run/kanidm/server.toml
kanidmd database vacuum  -c /run/kanidm/server.toml
```

Use the actual CLI argument ordering verified against each supported Kanidm
version; do not encode documentation examples without an integration test.

Both maintenance init containers use `automountServiceAccountToken: false`
through Pod-level configuration and receive no Kubernetes credentials.

### Runner behaviour

Pseudocode:

```text
read POD_NAME
read optional plan file

if plan is absent:
    exit 0

validate plan version and operation UID

if plan.targetPod != POD_NAME:
    exit 0

marker = /data/.kaniop/maintenance/<operation-id>.json

if marker says this operation completed successfully:
    exit 0

if marker says this exact attempt failed:
    fail closed without executing kanidmd again

write atomic "started" record
execute kanidmd database <operation>

if success:
    atomically write completed marker
    exit 0
else:
    atomically write failed marker including stable reason/exit information
    exit non-zero
```

The marker format should be versioned JSON and written by tempfile + fsync +
rename where the filesystem permits. Do not treat timestamps as identity.

The runner must not parse Kanidm human log text to decide success. Process exit
status is the command contract unless upstream later exposes a structured
result.

### Why a PVC marker is needed

A controller can crash after the command succeeds but before it patches CR
status. The replacement Pod can also be recreated after successful maintenance.
The database-local marker closes that gap without making the controller infer
success from log history.

It does **not** solve a node/process crash in the middle of the Kanidm command.
That is why Phase 0 qualification is mandatory.

## Controller algorithm

### Plan targets

Resolve the immutable execution list from the pinned `Kanidm` generation before
the first Pod deletion.

For each instance capture:

- replica group;
- ordinal;
- Pod name;
- data PVC name;
- role;
- whether it is the configured primary node;
- target Kanidm image string and, when available, resolved image ID/digest.

Sort deterministically:

1. non-primary instances before the configured primary;
2. higher ordinals before lower ordinals within a StatefulSet;
3. stable replica-group name as a final tie-breaker.

A targeted single replica bypasses ordering but not availability checks.

### Availability gate

Immediately before deleting a target Pod:

1. list the current Pods belonging to the target Kanidm;
2. require the current target itself to be in a known state;
3. count Ready serving replicas excluding the target;
4. for a write-capable target, require at least one other Ready write-capable
   replica unless `allowDowntime=true`;
5. reject/defer if another replica is already NotReady and the remaining
   capability would fall below the invariant;
6. revalidate target UID, topology and image against the pinned plan.

This gate is recalculated for every replica. Do not rely only on the initial
plan.

`allowDowntime` is primarily for a one-replica installation. It does not disable
storage, UID, concurrency or command-safety validation.

### Per-replica sequence

For each target:

```text
1. set status.currentTarget
2. check availability
3. publish plan ConfigMap for exact Pod
4. persist PreparingReplica
5. delete exact Pod with UID/resourceVersion precondition when practical
6. observe old Pod disappear
7. observe replacement Pod with a different UID
8. inspect init-container state
9. wait for maintenance success marker / init success
10. wait for main Kanidm container Ready
11. wait normal stability interval/minReadySeconds
12. record target in completedTargets
13. clear/update current plan
14. continue to next target
```

Never delete the next Pod from the same reconcile invocation that first observes
the previous Pod as merely created. Progression requires explicit successful
maintenance and Ready/stability state.

### Completion

After all targets:

1. delete/clear the maintenance plan ConfigMap;
2. verify every planned target remains Ready unless downtime was explicitly
   requested and the topology cannot satisfy that condition;
3. set `Completed=True` and terminal phase;
4. release exclusive operation ownership;
5. remove finalizer only when no active plan can affect a future Pod start.

## Concurrency and normal reconciliation

The first implementation should make the smallest safe change to the current
restore lock model.

Required behaviour while maintenance owns a Kanidm:

- reject another `KanidmMaintenance`;
- reject/defer `KanidmRestore`;
- normal reconciliation may update status and non-disruptive resources;
- defer StatefulSet image, replica-count, storage and replica-group topology
  changes until maintenance releases ownership;
- do not allow a user spec edit to retarget an in-progress operation;
- TLS/config changes that would necessarily roll the selected StatefulSets must
  be deferred unless explicitly proven harmless.

A generic operation coordinator may be extracted if it makes restore and
maintenance simpler, but this is not a prerequisite milestone by itself.
Avoid turning #950 into a broad controller-framework refactor.

## Failure semantics

### Failure matrix

| Failure | Expected behaviour |
|---|---|
| No maintenance plan | Init runner exits 0; Kanidm starts normally |
| Plan targets another Pod | Init runner exits 0 |
| Invalid/stale operation UID | Init fails closed; controller reports `PlanConflict` |
| Pod deletion API error | No availability change; retry controller action |
| Operator dies before Pod deletion | Resume from persisted status/plan; delete once |
| Operator dies after Pod deletion | StatefulSet recreates Pod; adopt replacement by name/UID |
| Operator dies after command success | PVC completion marker proves local completion |
| `kanidmd` exits non-zero | Record failed marker; Pod stays in init failure; do not advance |
| Target Pod/node dies during command | Behaviour depends on Phase 0 retry qualification; never advance based on absence alone |
| Replacement server never becomes Ready | Hold current target, report failure/timeout, do not advance |
| Another replica becomes NotReady | Do not start another maintenance target |
| User changes topology/image | Defer disruptive reconciliation and surface condition |
| Maintenance CR deleted before Pod deletion | Cancel, remove plan, release lock |
| Maintenance CR deleted after attempt starts | Finalizer performs operation-specific safe abort/cleanup; never silently advance |

### Retry/abort UX

Do not invent complicated retry controls until interruption behaviour is known.
The initial policy should be:

- controller/API retries are automatic before the Kanidm command starts;
- a known `kanidmd` command failure is terminal for that maintenance CR;
- the runner does not automatically execute the same known failed attempt again;
- the failed Pod remains isolated;
- deleting/aborting a failed operation may allow normal server startup only for
  operations whose Phase 0 contract proves that failed/interrupted execution
  leaves the database safe to reopen;
- otherwise document manual recovery and keep the finalizer fail-closed.

If real usage demonstrates a need for explicit retry of a failed local attempt,
add a separate operation/attempt mechanism rather than making the immutable
spec mutable.

## Phase 0: qualify Kanidm command safety

### Problem

The init-container design is at-least-once across catastrophic Pod/node failure.
Kubernetes cannot guarantee that a process executed exactly once.

### Approach

Build a small integration harness using the exact supported Kanidm container
versions and persistent test databases.

For each candidate operation:

1. create/populate a representative database;
2. snapshot/hash logical contents before maintenance;
3. run the operation normally;
4. run it again immediately;
5. verify the server starts and logical contents remain correct;
6. repeat while killing the maintenance process at multiple points;
7. rerun the operation after each kill;
8. start the server and verify the database after each case.

Minimum matrix:

| Operation | Normal repeat | SIGTERM | SIGKILL | Pod/container kill | Node-style abrupt kill | Reopen DB | Retry |
|---|---:|---:|---:|---:|---:|---:|---:|
| Verify | required | required | required | required | required | required | required |
| Reindex | required | required | required | required | required | required | required |
| Vacuum | required | required | required | required | required | required | required |

Run this against every Kanidm minor version Kaniop claims to support if the
implementation changed materially. The Kanidm source currently shows explicit
transaction boundaries for reindex and a backend-open vacuum path, but code
inspection is evidence for where to test, not a substitute for interruption
tests.

### Files

Prefer a dedicated integration/E2E helper under `tests/` rather than production
feature flags in the controller.

### Verify

An operation is enabled only when all kill/reopen/retry cases have a documented
expected result. If vacuum fails qualification, ship reindex/verify without it.

### Estimated effort

1-2 focused engineering days.

## Phase 1: CRD and exclusive-operation coordination

### Problem

Kaniop needs a durable user intent and must prevent restore/topology mutation
from racing maintenance.

### Approach

1. Add `KanidmMaintenance` types, phases and status.
2. Add immutable-spec CEL validation.
3. Pin target name + UID before any disruptive action.
4. Add controller registration and RBAC for maintenance CRs, Pods and the plan
   ConfigMap.
5. Extend the current restore operation exclusion so restore and maintenance
   cannot own the same Kanidm simultaneously.
6. Make normal StatefulSet-changing reconciliation respect active maintenance.

### Likely files

- `libs/operator/src/kanidm/maintenance.rs` (new)
- `libs/operator/src/kanidm/mod.rs`
- controller startup/registration under `cmd/operator/` or current controller
  registration module
- CRD generation output / Helm CRDs
- RBAC manifests/templates
- `libs/operator/src/kanidm/restore.rs` only for shared locking semantics
- normal Kanidm reconcile entry point for the mutation gate

### Verify

- CRD schema round-trip and immutable-spec tests;
- UID mismatch is terminal before Pod deletion;
- restore vs maintenance mutual exclusion in both creation orders;
- normal image/replica change is deferred while maintenance owns the target.

### Estimated effort

1-2 days.

## Phase 2: maintenance runner and no-op Pod plumbing

### Problem

Every replacement Pod needs a safe conditional gate before Kanidm starts, but
the Kanidm image cannot be assumed to contain a shell.

### Approach

1. Add a minimal `kaniop-maintenance-runner` Rust binary.
2. Make it static/portable enough to execute inside the supported Kanidm
   distroless images on each published architecture.
3. Add a small runner OCI image/stage and release plumbing.
4. Add optional maintenance-plan ConfigMap volume to every managed Pod.
5. Add tools `EmptyDir`.
6. Add bootstrap init container.
7. Add Kanidm-image maintenance init container after generated configuration.
8. Add POD_NAME through the downward API.
9. Implement versioned plan parsing and target mismatch no-op.
10. Implement versioned atomic PVC markers.

### Likely files

- `cmd/maintenance-runner/Cargo.toml` (new)
- `cmd/maintenance-runner/src/main.rs` (new)
- workspace `Cargo.toml`
- `Dockerfile`
- build/release Makefile and CI image targets
- a small image helper analogous to `libs/backup-core/src/image.rs`
- `libs/operator/src/kanidm/reconcile/statefulset.rs`

### Important tests

- ordinary Kanidm Pod starts with no ConfigMap;
- missing optional ConfigMap is a no-op;
- plan for another Pod is a no-op;
- correct target executes a fake/controlled command in runner unit tests;
- marker is atomic and operation-UID scoped;
- an existing success marker prevents duplicate command execution;
- a known failed marker fails without rerunning the command;
- user-supplied init containers cannot override reserved maintenance containers;
- existing generated replication config is available before maintenance executes;
- no service-account token is introduced.

### Estimated effort

2-3 days, including image/release plumbing.

## Phase 3: single-replica controller path

### Problem

Prove the orchestration with the smallest possible blast radius before rolling
across a topology.

### Approach

Implement `target.replica` first:

1. validate storage and target ordinal;
2. acquire operation lock;
3. verify availability/downtime policy;
4. create exact-Pod plan;
5. delete the selected Pod;
6. adopt the replacement Pod;
7. map init-container state/marker into CR status;
8. wait for server readiness/stability;
9. clean plan and complete.

### Verify

- a three-replica deployment loses exactly one Pod;
- Service remains available during reindex/verify;
- only the selected PVC receives an operation marker;
- operator restart before/after Pod deletion converges;
- deleting an unrelated replica during the active plan does not execute
  maintenance there;
- single-replica request fails unless `allowDowntime=true`;
- EmptyDir/ephemeral storage fails before Pod deletion.

### Estimated effort

2 days.

## Phase 4: deterministic rolling `AllReplicas`

### Problem

Rolling every local database needs deterministic sequencing and an availability
check before each disruption.

### Approach

1. Build and pin the execution list.
2. Sort non-primary before primary and high ordinal before low ordinal.
3. Loop the single-replica state machine using persisted `currentTarget` and
   `completedTargets`.
4. Recalculate availability immediately before each delete.
5. Stop if any unrelated replica becomes unhealthy.
6. Remove the plan only after the final target has returned Ready.

### Verify

For three write replicas:

```text
all Ready
  -> maintain 2
  -> 2 Ready
  -> maintain 1
  -> 1 Ready
  -> maintain 0
  -> 0 Ready
  -> Completed
```

Assertions:

- never two intentionally unavailable replicas;
- primary ordinal zero is last;
- a target is never marked completed on init success alone; main server readiness
  is required;
- a failed target prevents any later target from being deleted;
- controller restart between every pair of phases preserves order.

### Estimated effort

1-2 days.

## Phase 5: fault-injection and restart hardening

### Problem

The feature is only valuable if an operator/node failure during maintenance
cannot turn one unavailable replica into a topology-wide incident.

### Required scenarios

Controller restart at each boundary:

- after lock acquisition;
- after plan creation;
- immediately after Pod deletion;
- while replacement Pod is Pending;
- while maintenance init is Running;
- after marker success but before status update;
- after main container Ready but before target completion;
- between two replicas;
- during final cleanup.

Kubernetes/runtime failures:

- target node becomes unavailable before Pod deletion;
- Pod is force-deleted while init is running;
- replacement Pod cannot schedule;
- runner image cannot pull;
- Kanidm image cannot pull;
- PVC cannot mount;
- another replica becomes NotReady during maintenance;
- unrelated replica is deleted while the plan targets another Pod.

User/concurrency failures:

- `Kanidm` image changes during operation;
- replica count changes during operation;
- replica group is renamed/removed during operation;
- restore is requested concurrently;
- second maintenance is requested concurrently;
- maintenance CR is deleted in every phase.

### Verify

For every scenario, assert both:

1. no later replica is intentionally deleted after the fault; and
2. recovery after operator restart is derived entirely from Kubernetes objects,
   Pod/init status and PVC markers.

### Estimated effort

2-3 days.

## Phase 6: observability and user documentation

### Events

Emit concise Events for:

- maintenance accepted;
- waiting for available capacity;
- replica selected;
- replica restart initiated;
- maintenance command succeeded/failed;
- replica returned Ready;
- operation completed/failed.

### Metrics

Suggested bounded metrics:

```text
kaniop_maintenance_operations_total{operation,result}
kaniop_maintenance_duration_seconds{operation}
kaniop_maintenance_replica_duration_seconds{operation}
kaniop_maintenance_failures_total{operation,reason}
```

Do not use Kanidm name, Pod name or operation UID as metric labels.

### User documentation

Document:

- operations supported by Kanidm version;
- persistent-storage requirement;
- single-replica `allowDowntime` semantics;
- rolling availability guarantee and its limits;
- failure/abort procedure;
- why readiness is not a replication fence;
- no schedule in v1;
- examples for whole-topology and single-replica maintenance.

### Estimated effort

1 day.

## E2E acceptance scenario

The principal E2E should make the intended value obvious.

Start a three-write-replica Kanidm and continuously perform health/authenticated
requests while creating:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmMaintenance
metadata:
  name: rolling-reindex
spec:
  targetRef:
    name: kanidm
    uid: <uid>
  operation: Reindex
  target:
    allReplicas: {}
```

Observe:

```text
t=0   [0 Ready] [1 Ready] [2 Ready]
t=1   [0 Ready] [1 Ready] [2 Init:maintenance]
t=2   [0 Ready] [1 Ready] [2 Ready]
t=3   [0 Ready] [1 Init:maintenance] [2 Ready]
t=4   [0 Ready] [1 Ready] [2 Ready]
t=5   [0 Init:maintenance] [1 Ready] [2 Ready]
t=6   [0 Ready] [1 Ready] [2 Ready] -> Completed
```

The test must assert that requests continue succeeding through the operation and
that Kaniop never causes two replicas to be unavailable simultaneously.

A second E2E deliberately fails maintenance on replica 2 and proves replicas 1
and 0 remain serving and are never restarted.

## Sequencing and review strategy

Prefer several reviewable PRs after this ADR rather than one large implementation
branch:

1. **Safety qualification** — interruption/retry harness and documented results.
2. **Pod substrate** — runner image, optional plan, init containers and no-op
   regression tests.
3. **CRD + single replica** — API, lock, controller and one-target E2E.
4. **Rolling topology** — all-replica ordering, availability gate and failure
   tests.
5. **Operational polish** — metrics, docs and vacuum if qualified.

Phase 0 can run in parallel with CRD/pod-substrate work, but no mutating
operation should be declared production-supported until its safety result is
known.

## Effort and impact summary

Assuming reindex/verify interruption behaviour is safe:

| Area | Estimate | Risk |
|---|---:|---|
| Kanidm command safety spike | 1-2 days | Medium; upstream semantics decide scope |
| CRD + operation lock | 1-2 days | Low-medium |
| Runner + image + Pod wiring | 2-3 days | Medium |
| Single-replica controller | ~2 days | Medium |
| Rolling ordering/availability | 1-2 days | Medium |
| Fault/restart E2E | 2-3 days | Medium-high but bounded |
| Docs/metrics | ~1 day | Low |
| **Production-grade initial feature** | **9-14 engineering days** | **Moderate** |

This is substantially smaller than backup/restore. There is no object-storage
protocol, backup catalog, retention, global quiescence, PVC ownership transfer,
restore source preparation, database replacement or replica reconstruction. The
new hard parts are limited to per-replica availability gating, a small Pod-local
runner, and proving upstream maintenance commands survive interruption safely.

If the safety spike finds that reindex or vacuum cannot safely recover from
arbitrary termination, effort changes qualitatively: either the affected
operation remains unsupported or Kaniop needs an upstream Kanidm primitive. Do
not compensate with a more elaborate Kubernetes state machine.

## Completion criteria

#950 is implementation-ready when this ADR is accepted and Phase 0 has answered
the command-safety question. The feature itself is complete when:

- `KanidmMaintenance` is an immutable one-shot CRD;
- reindex and verify are supported for qualified Kanidm versions;
- vacuum is supported only if independently qualified;
- PVC-backed multi-replica deployments are maintained one replica at a time;
- single-replica maintenance requires explicit downtime permission;
- normal Pod startup with no maintenance request is regression-tested;
- one failed maintenance replica never causes Kaniop to restart another replica;
- controller restart at every phase converges safely;
- restore/topology/image races are blocked;
- Events, Conditions, metrics and user-facing failure guidance exist;
- E2E demonstrates continuous service during successful three-replica rolling
  maintenance.

## Explicitly deferred

The first implementation does not include:

- `KanidmMaintenanceSchedule`;
- log-triggered automatic reindex;
- arbitrary label selectors;
- `maxUnavailable > 1`;
- full-topology offline fallback Jobs;
- a new Kanidm/Kubidm network maintenance API;
- replication-fence semantics stronger than normal Pod restart;
- automatic backup before maintenance;
- automatic repair of a database that fails verification;
- a generic workflow engine shared by every Kaniop controller.

These can be added only when concrete operational requirements justify the
additional surface area.