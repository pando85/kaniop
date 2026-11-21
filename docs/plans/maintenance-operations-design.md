# Rolling Kanidm Database Maintenance Implementation Plan

Status: **Proposed**

Related:

- [ADR-0002: Rolling Kanidm database maintenance with init containers](../adr/0002-rolling-kanidm-database-maintenance.md)
- [Kaniop issue #950](https://github.com/pando85/kaniop/issues/950)
- [Kanidm database maintenance documentation](https://kanidm.github.io/kanidm/stable/database_maintenance.html)
- [ADR-0001: Production Kanidm backup and restore orchestration](../adr/0001-production-kanidm-backup-and-restore.md)
- [Kubidm PR #386](https://github.com/pando85/kubidm/pull/386) — node drain, replication fences and maintenance control plane

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

Maintenance requests are immutable one-shot resources, analogous to a Kubernetes Job.

Conceptually:

## Summary

Kaniop should model database maintenance as an explicit, restart-safe Kubernetes operation rather than as an imperative side effect of the `Kanidm` reconciler.

The target API is a single immutable `KanidmMaintenance` CRD covering maintenance operations such as reindex and verify. Kaniop chooses one of two execution backends:

1. **native maintenance protocol** — preferred when the target server exposes the Kubidm maintenance capabilities introduced by Kubidm PR #386. Kaniop can drain one replica, prove handoff of its replication history to a surviving replica, perform maintenance, catch the maintained replica back up to a peer fence, and resume it;
2. **offline compatibility protocol** — fallback for upstream Kanidm or a server without the required native capability. Kaniop quiesces the topology, waits for Pods/volume attachments to disappear, and runs the exact server image against each PVC using the existing restore/offline-operation safety machinery.

Kaniop owns orchestration and Kubernetes resource safety. The identity server owns database and replication semantics. Kaniop must never infer replication safety from Pod readiness, logs, timestamps, or StatefulSet rollout state when a native fence protocol is available.

## Motivation

Kanidm documents reindex, vacuum and verify as offline database-administration operations. That model is safe for a host administrator but creates an impedance mismatch with a declarative Kubernetes controller:

- a normal Pod cannot safely run beside a maintenance Job mounting the same RWO database PVC;
- `Pod Ready` says nothing about whether a peer contains every write known to a replica about to be stopped;
- parsing a warning such as `YOU MUST REINDEX YOUR DATABASE` is not a stable control-plane API;
- a controller restart must not accidentally execute a destructive/exclusive operation twice;
- restore, upgrade, maintenance and normal topology reconciliation must not race each other.

Kubidm PR #386 addresses the server-side consistency gap with node drain, RUV-based replication fences, idempotent maintenance operations and maintenance-aware readiness. This document defines how Kaniop should consume those primitives while retaining a safe fallback for upstream Kanidm.

## Goals

- Provide a declarative, observable maintenance API.
- Make the controller restart-safe and idempotent.
- Support rolling reindex/verify on capable replicated Kubidm topologies without claiming false zero-downtime guarantees.
- Support safe single-node maintenance with expected service downtime.
- Retain a conservative full-offline fallback for upstream Kanidm.
- Reuse the Kubernetes/PVC safety primitives already implemented for `KanidmRestore`.
- Prevent maintenance from racing restore, image upgrades, topology changes or another maintenance operation.
- Keep target selection semantic and tied to the `Kanidm` resource model rather than arbitrary Pod label selectors.
- Make capability negotiation explicit so Kaniop never assumes that a Kanidm-compatible image implements Kubidm extensions.

## Non-goals

- Automatically taking a cluster offline because a log line suggests reindexing.
- Implementing a distributed consensus protocol in Kaniop.
- Electing a permanent primary/leader for normal identity writes.
- Treating normal Kubernetes readiness as proof of replication convergence.
- Scheduling periodic reindex/verify in the first version.
- Using maintenance as a substitute for restore or disaster recovery.
- Supporting historical rollback through this API.

## API

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
  strategy: Auto
  target:
    allReplicas: {}
  allowDowntime: false
```

The `uid` is optional at creation time if admission/controller resolution fills or validates it, but status must pin the operation to the observed target UID before any mutation. A delete/recreate with the same `Kanidm` name must never inherit an old operation.

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
    pub strategy: KanidmMaintenanceStrategy,
    #[serde(default)]
    pub target: KanidmMaintenanceTarget,
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub allow_downtime: bool,
}

pub struct KanidmMaintenanceTargetRef {
    pub name: String,
    pub uid: String,
```

Defaults:

- `strategy: Auto`;
- `target: AllReplicas`;
- operation timeout is controller-defined and bounded.

Arbitrary label selectors are intentionally not supported. Kaniop already knows the `Kanidm -> ReplicaGroup -> StatefulSet -> ordinal -> PVC` relationship and should derive the physical resources itself.

### Why one CRD rather than one CRD per operation

Reindex and verify share the same lifecycle:

- validate target;
- acquire exclusive operation ownership;
- negotiate capabilities/strategy;
- quiesce or drain;
- execute one database operation;
- validate result;
- recover/resume;
- record an immutable result.

Separate `KanidmReindex`, `KanidmVerify`, etc. would duplicate controller state machines and make mutual exclusion harder. Restore remains a separate CRD because its semantics are materially different: it selects an external recovery artifact and may establish/rebuild cluster history.

```rust
pub enum KanidmMaintenanceOperation {
    Reindex,
    Verify,
    // Add only after a server/backend with safe semantics exists.
    Vacuum,
}

pub enum KanidmMaintenanceStrategy {
    Auto,
    Native,
    Offline,
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

## Status model

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

Suggested phases with strategy-specific sub-phases:

```text
Pending
  -> Validating
  -> AcquiringLock
  -> DiscoveringCapabilities
  -> Planning

native:
  -> Draining
  -> HandingOff
  -> Running
  -> CatchingUp
  -> Resuming

fallback:
  -> Quiescing
  -> WaitingForDetach
  -> Running
  -> Verifying
  -> Resuming

  -> Completed

or Failed
```

Suggested status:

```rust
pub struct KanidmMaintenanceStatus {
    pub observed_generation: Option<i64>,
    pub observed_target_uid: Option<String>,
    pub phase: KanidmMaintenancePhase,
    pub operation_id: Option<String>,
    pub selected_strategy: Option<KanidmMaintenanceStrategy>,
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

Fence values are **opaque tokens** to Kaniop. The controller may persist them in status if bounded by the negotiated protocol. If future fence formats can become large, store them in an owner-referenced ConfigMap and keep only its reference/hash in status. They are replication metadata, not credentials, but must still be treated as internal operational data.

`operation_id` should be deterministically derived from or equal to the `KanidmMaintenance` resource UID. Every native mutating request uses that ID. This gives at-least-once reconciler execution exactly-once *semantic* behaviour at the server boundary.

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
These can be added only when concrete operational requirements justify the
additional surface area.

## Status model

Suggested phases with strategy-specific sub-phases:

```text
Pending
  -> Validating
  -> AcquiringLock
  -> DiscoveringCapabilities
  -> Planning

native:
  -> Draining
  -> HandingOff
  -> Running
  -> CatchingUp
  -> Resuming

fallback:
  -> Quiescing
  -> WaitingForDetach
  -> Running
  -> Verifying
  -> Resuming

-> Completed
or Failed
```

Suggested status fields:

```rust
pub struct KanidmMaintenanceStatus {
    pub phase: KanidmMaintenancePhase,
    pub observed_generation: i64,
    pub target_uid: String,
    pub operation_id: String,
    pub selected_strategy: Option<KanidmMaintenanceStrategy>,
    pub current_target: Option<MaintenanceInstanceRef>,
    pub completed_targets: Vec<MaintenanceInstanceRef>,
    pub conditions: Vec<Condition>,
    pub started_at: Option<Time>,
    pub completed_at: Option<Time>,
    pub native_api_version: Option<String>,
    pub handoff_fence: Option<String>,
    pub recovery_fence: Option<String>,
}
```

Fence values are **opaque tokens** to Kaniop. The controller may persist them in status if bounded by the negotiated protocol. If future fence formats can become large, store them in an owner-referenced ConfigMap and keep only its reference/hash in status. They are replication metadata, not credentials, but must still be treated as internal operational data.

`operation_id` should be deterministically derived from or equal to the `KanidmMaintenance` resource UID. Every native mutating request uses that ID. This gives at-least-once reconciler execution exactly-once *semantic* behaviour at the server boundary.

## Global operation coordination

The existing restore implementation already introduces the concept that database-affecting operations need exclusive ownership of a `Kanidm` target. Maintenance should generalise this rather than create another independent annotation.

Target design:

```text
Kanidm
   |
   +-- OperationCoordinator
         |
         +-- Restore
         +-- Maintenance
         +-- Upgrade/topology mutation
```

Only one exclusive topology/database operation may own a `Kanidm` at a time.

A generic ownership annotation/status record can replace the restore-specific lock over time, for example conceptually:

```text
kaniop.rs/exclusive-operation = <kind>/<namespace>/<name>/<uid>
```

The ownership record must include a UID so stale names cannot retain authority.

### Identity reconcilers

The lock has two distinct effects and should not conflate them:

1. **exclusive topology/database ownership** — always prevents restore/maintenance/upgrade/topology mutation from racing;
2. **identity-write suspension** — required for the full-offline compatibility strategy and restore, but not inherently required for native rolling maintenance because writes can continue through surviving replicas and are included in the fence/catch-up protocol.

The current restore-specific `kanidm_write_allowed()` should eventually become an operation-aware policy rather than `restore == true`.

## Capability discovery

Before choosing a strategy Kaniop queries each targeted server instance through a `MaintenanceClient` abstraction.

Conceptual response:

```json
{
  "apiVersion": "v1",
  "drain": true,
  "replicationFence": true,
  "syncUntil": true,
  "reindex": true,
  "verify": true,
  "vacuum": false,
  "restore": false
}
```

### Strategy selection

`Auto` selects native only when all capabilities required by the requested operation and topology are present.

For rolling replicated maintenance, minimum native capability set is:

```text
drain
replicationFence
syncUntil
<requested operation>
```

If any targeted replica does not support a compatible native API version, `Auto` falls back to the offline strategy. `Native` fails validation instead of silently falling back. `Offline` never probes/uses native mutation primitives except optional read-only diagnostics.

Mixed native API versions during an image rollout should be treated conservatively. Kaniop must not compose fence tokens across incompatible API versions unless the server explicitly declares compatibility.

## Maintenance transport

The reconciler should depend on an internal trait rather than on a transport:

```rust
#[async_trait]
pub trait MaintenanceClient {
    async fn capabilities(&self) -> Result<Capabilities>;
    async fn status(&self) -> Result<MaintenanceStatus>;
    async fn drain(&self, operation_id: Uuid) -> Result<ReplicationFence>;
    async fn fence(&self) -> Result<ReplicationFence>;
    async fn sync_until(
        &self,
        operation_id: Uuid,
        fence: &ReplicationFence,
        timeout: Duration,
    ) -> Result<SyncResult>;
    async fn run(
        &self,
        operation_id: Uuid,
        operation: MaintenanceOperation,
    ) -> Result<RunResult>;
    async fn resume(&self, operation_id: Uuid) -> Result<()>;
}
```

Kubidm PR #386 initially defines the privileged semantics on its local Unix admin control path. The preferred production Kaniop transport should be a dedicated server-side operator mTLS endpoint exposing the same protocol. Kaniop should not make ordinary directory authentication a prerequisite for repairing the directory database.

A Kubernetes `pods/exec`/Unix-socket bridge may be useful during development or as a transitional implementation, but it should not become the long-term architecture because it expands RBAC requirements, couples the controller to CLI formatting/process execution, and is harder to make transport-stable.

## Native rolling algorithm

Assume replicas A and B and maintenance target B.

### 1. Preconditions

Kaniop requires:

- B is currently healthy enough to drain;
- at least one other replica A is a viable handoff peer for a zero-service-outage operation;
- A and B report compatible native capabilities;
- no exclusive restore/upgrade/maintenance operation owns the `Kanidm`;
- the target object UID still matches status;
- B's StatefulSet/PVC identity is the one derived from the `Kanidm` spec.

For a single-replica topology, native maintenance is still valid but necessarily causes service downtime: drain -> run -> resume. No peer fence handoff is needed because no other writer remains active.

### 2. Drain target

```text
B: drain(operation_id) -> F_B
```

Drain semantics are defined by the server. Kaniop waits for the native call to succeed; it does not substitute `Pod Ready=false` for this acknowledgement.

The server's readiness should become false as part of drain. Kaniop may additionally observe EndpointSlice/Pod readiness for user-visible progress, but that is not the consistency proof.

Persist `F_B` before progressing.

### 3. Handoff to survivor

```text
A: sync_until(operation_id, F_B)
```

Only `Satisfied` allows the operation to progress.

`TimedOut` is retryable within the maintenance timeout. Domain/generation mismatch is terminal and requires operator intervention/replanning. A refresh-required result must not be converted into "probably safe" based on timestamps.

At this point at least one serving node provably contains everything B knew when it drained.

### 4. Run maintenance on B

```text
B: run(operation_id, Reindex)
```

or Verify.

The result is idempotent under `operation_id`. Kaniop stores the returned result and any post-maintenance fence/verification errors.

If the operation fails, B stays non-serving/fenced. Kaniop reports `Failed` and must not automatically make a node with failed database verification ready merely to restore replica count.

### 5. Capture survivor recovery fence

```text
A: fence() -> F_A
```

This fence is captured *after* B's maintenance because writes may have continued on A during the operation.

Persist `F_A`.

### 6. Catch B up

```text
B: sync_until(operation_id, F_A)
```

The native server keeps B not-ready while allowing its replication consumer to catch up, then re-fences it before returning the current state.

Only `Satisfied` allows resume.

### 7. Resume B

```text
B: resume(operation_id)
```

Then wait for:

- native maintenance status `Serving`;
- normal `/readyz` success;
- Kubernetes Pod readiness/endpoints as secondary orchestration signals.

### 8. Continue with the next target

For `AllReplicas`, repeat one instance at a time. Never drain the selected handoff peer while another target still depends on it.

For topologies larger than two replicas, Kaniop needs only one trusted surviving peer to satisfy the drained node's fence before maintenance, but it should select a healthy, compatible peer deterministically and may prefer a different failure domain when topology information exists.

## Why readiness is not the handoff condition

A replica can be serving-ready while still catching up. Consider:

```text
A and B start converged
B accepts X
B is removed
A is Ready but has not received X yet
A becomes the sole server
```

Nothing about Kubernetes readiness proves X exists on A. A fence changes the question from "is A healthy?" to "does A contain at least this explicit replication history?".

This distinction must remain visible in the Kaniop state machine and tests.

## Offline compatibility strategy

For upstream Kanidm, use the same conservative safety boundary as restore/offline database operations.

### Full topology workflow

1. acquire exclusive operation ownership;
2. suspend Kaniop identity writes for the target;
3. record desired StatefulSet replica counts/images;
4. scale every target replica group to zero;
5. wait until target Pods are gone;
6. map PVC -> PV and wait until no `VolumeAttachment` references the target volumes;
7. for each target PVC, create a short-lived maintenance Job using the **exact immutable Kanidm image associated with that database**;
8. mount only that instance's data PVC and minimal generated server configuration;
9. execute the requested `kanidmd database <operation>` command;
10. observe Job completion and structured Kubernetes failure reason;
11. optionally run offline verify after mutating operations;
12. restore StatefulSet replica counts in the topology-defined order;
13. wait for readiness/replication recovery using the best supported server signals;
14. resume Kaniop identity writes;
15. release operation ownership.

This has downtime but has defensible storage semantics.

### Reuse restore machinery

The current restore implementation already contains critical primitives that maintenance should factor into a shared internal offline-operation coordinator:

- scale replica groups;
- wait for Pods to stop;
- resolve PVC/PV relationships;
- wait for `VolumeAttachment` detachment;
- construct Jobs with service-account token disabled;
- mount `/data` and generated server config;
- observe Job completion restart-safely;
- restore desired replica counts.

Do not copy this logic into a second controller implementation.

Conceptually:

```text
                    +-------------------+
KanidmRestore ----->|                   |
                    | Offline DB engine |----> quiesce/detach/job/observe/resume
Maintenance ------->|                   |
  (fallback)        +-------------------+
```

Restore retains its dedicated higher-level state machine because it replaces database state and rebuilds replication topology; maintenance only invokes a local database operation.

## Exact image invariant

An offline maintenance Job must use the same exact Kanidm/Kubidm binary lineage expected by the database.

Kaniop should resolve and persist the image used for each target before quiescing. Mutable tags such as `latest` must not be re-resolved after the operation starts. Prefer digest-pinned images where possible.

The native strategy naturally executes inside the running server process and therefore avoids this class of mismatch.

## Upgrade and topology interaction

Maintenance and upgrade cannot run concurrently.

Examples of unsafe races:

- Kaniop drains B for reindex while the normal `Kanidm` reconciler replaces B with a new image;
- an offline maintenance Job mounts a PVC while a StatefulSet scale-up reattaches it;
- restore deletes/rebuilds secondary PVCs while maintenance targets an ordinal derived from the old topology;
- replica count changes remove the handoff peer selected by an active operation.

While an exclusive operation is active, the normal `Kanidm` reconciler may continue non-conflicting status work but must defer topology/image/storage mutations that invalidate the operation plan.

The operation controller should record `observedGeneration` and either:

- finish against the pinned plan while deferring a newer `Kanidm.spec`, or
- fail safely before mutation if the plan has not yet crossed an irreversible boundary.

It must never silently retarget an in-progress maintenance CR to a new topology.

## Controller restart semantics

Every phase must be reconstructible from Kubernetes status/resources and native server status.

Examples:

- after restart in `HandingOff`, replay `sync_until(operation_id, F_B)`;
- after restart in `Running`, query native maintenance status/run result using the same operation ID before issuing a new run;
- after restart in offline `WaitingForDetach`, recompute current Pod/VolumeAttachment state;
- after restart with a maintenance Job present, adopt it by owner reference/operation UID rather than creating a duplicate;
- after restart in `CatchingUp`, replay the persisted recovery fence;
- after successful native `resume`, a repeated resume with the same ID must be harmless.

Do not rely on controller memory for a correctness boundary.

## Deletion/finalizers

`KanidmMaintenance` should use a finalizer only while deletion could orphan an exclusive server/Kubernetes operation.

Deleting a CR must **not** mean "force resume regardless of database state". Safe deletion behaviour is:

- before drain/quiesce: cancel and release ownership;
- after drain but before mutation: attempt normal safe resume/cancellation;
- after a failed mutating operation or failed verification: preserve the fenced/non-serving state and surface an explicit condition requiring operator action; do not hide the failure by automatically resuming;
- offline: ensure no maintenance Job/PVC mount is left unmanaged before removing the finalizer.

A separate explicit recovery/retry action or a new maintenance CR can be used after an operator resolves the failure.

## Conditions

Recommended conditions include:

- `Accepted` — target/UID/spec is valid;
- `OperationLockAcquired`;
- `NativeMaintenanceAvailable`;
- `TargetDrained`;
- `HandoffSatisfied`;
- `MaintenanceSucceeded`;
- `VerificationSucceeded`;
- `RecoveryFenceSatisfied`;
- `ReadyToResume`;
- `Completed`;
- `Degraded` / `Failed` with stable reason codes.

Reasons should be machine-readable, for example:

```text
TargetNotFound
TargetUidMismatch
ConflictingOperation
CapabilityMissing
NativeApiVersionMismatch
DrainTimeout
FenceDomainMismatch
FenceGenerationMismatch
HandoffTimeout
MaintenanceFailed
VerificationFailed
VolumeStillAttached
JobFailed
RecoveryTimeout
ResumeFailed
```

## Scheduling

Do not embed cron semantics in `KanidmMaintenance`.

A one-shot immutable operation object is easier to reason about and audit. If recurring maintenance becomes justified, add a separate scheduler analogous to `CronJob -> Job`:

```text
KanidmMaintenanceSchedule -> KanidmMaintenance
```

Even then, defaults should be conservative:

- **Reindex**: remedial/event-driven, not routinely scheduled;
- **Verify**: explicit diagnostics/maintenance policy, not automatically frequent;
- **Vacuum**: potentially schedulable only after server semantics and cost/downtime are well understood.

The warning that motivated #950 must not become a log-triggered automatic outage policy.

## Security

- Native mutation control must not depend on a healthy identity database for authentication.
- Preferred transport is a dedicated mTLS operator control plane with narrowly scoped client credentials.
- Kaniop Secrets used for control-plane client identity must follow normal least-privilege/rotation practices.
- The fallback Job gets `automountServiceAccountToken: false` unless a future operation specifically requires Kubernetes API access.
- Maintenance Jobs mount only the target PVC/config and run the exact intended server image.
- Fence tokens contain replication metadata, not user credential material, but should not be exposed through public status/UI unnecessarily.
- Audit native operations by operation ID, target instance and operation type.

## Observability

Kaniop should expose:

- phase/condition transitions as Kubernetes Events;
- controller metrics for operation duration and failures by operation/strategy/phase;
- current target and selected strategy in CR status;
- native capability version;
- handoff/recovery timeout counters;
- fallback Job identity and exit state.

Do not export raw fence content as a metric label.

## Proposed implementation sequence

### Phase 1 — shared operation coordination

1. Generalise the restore-only exclusive-operation marker into an operation coordinator.
2. Separate "topology mutation locked" from "identity writes suspended" policy.
3. Add helpers to pin target UID/generation/topology.

### Phase 2 — CRD and controller skeleton

4. Add `KanidmMaintenance` CRD/types/status/conditions.
5. Add immutable-spec validation and operation-ID derivation from CR UID.
6. Add controller restart/adoption tests.

### Phase 3 — native client

7. Define the internal `MaintenanceClient` trait.
8. Implement capability negotiation and native API-version checks.
9. Implement transport for the server-side operator control API once Kubidm exposes the production remote transport.
10. Add fake client/state-machine tests independent of transport.

### Phase 4 — native state machine

11. Implement drain -> handoff -> run -> recovery-fence -> resume.
12. Support single-replica native maintenance with explicit downtime condition.
13. Implement deterministic handoff-peer selection.
14. Persist opaque fences/status so every step survives controller restart.
15. Add E2E tests with writes occurring during rolling maintenance to prove no acknowledged pre-drain history is lost.

### Phase 5 — offline compatibility

16. Refactor restore's quiesce/detach/Job primitives into a shared offline-operation engine without changing restore semantics.
17. Add maintenance Job command generation for reindex/verify.
18. Enforce exact/pinned image semantics.
19. Add restart-safe Job adoption and failure handling.
20. Add E2E coverage against upstream Kanidm images.

### Phase 6 — operational polish

21. Add Events/metrics/documentation and kubectl examples.
22. Document capability/strategy matrix.
23. Decide separately whether `KanidmMaintenanceSchedule` is warranted.
24. Add vacuum only after Kubidm/upstream exposes a safe, documented implementation and capability.

## Required tests

At minimum:

### Native protocol

- two replicas converge, B drains, A satisfies `F_B`, B reindexes, B satisfies post-operation `F_A`, B resumes;
- a write accepted by B immediately before drain is present on A before B maintenance starts;
- writes accepted by A while B is maintained appear on B before resume;
- `Pod Ready=true` alone never advances `HandingOff`;
- repeated reconciliation/replayed operation IDs do not execute maintenance twice;
- controller restart at every phase resumes safely;
- domain/generation fence mismatch fails closed;
- maintenance verification failure leaves the target non-serving;
- single-replica native maintenance reports/observes downtime and resumes safely.

### Offline compatibility

- all Pods are gone before any maintenance Job mounts a data PVC;
- all relevant `VolumeAttachment`s are gone before Job creation;
- exact image is pinned before quiesce and reused after controller restart;
- only one Job per target/operation UID is created;
- failure does not race StatefulSet recreation;
- identity reconcilers remain paused for the entire offline window;
- normal desired replica counts are restored only after every required operation succeeds.

### Concurrency

- restore vs maintenance;
- maintenance vs maintenance;
- image upgrade vs maintenance;
- replica-group/replica-count change during maintenance;
- deletion of `KanidmMaintenance` in each phase.

## Decision

Kaniop will treat maintenance as a first-class declarative operation.

When a server exposes a compatible native maintenance protocol, Kaniop will orchestrate explicit replication-fence handoffs instead of inferring safety from Kubernetes readiness. Upstream Kanidm remains supported through a conservative full-offline PVC/Job workflow built on the same Kubernetes safety primitives as restore.

The server remains responsible for database/replication correctness. Kaniop remains responsible for sequencing, persistence of operation intent, Kubernetes resource ownership, retries, and user-visible status.
