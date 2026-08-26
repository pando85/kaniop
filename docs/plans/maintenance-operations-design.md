# Maintenance Operations Design

Status: **Proposed**

Related:

- [Kaniop issue #950](https://github.com/pando85/kaniop/issues/950)
- [Kubidm PR #386](https://github.com/pando85/kubidm/pull/386) — node drain, replication fences and maintenance control plane
- [Backup and Restore ADR](../adr/0001-production-kanidm-backup-and-restore.md)

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

### `KanidmMaintenance`

Maintenance requests are immutable one-shot resources, analogous to a Kubernetes Job.

Conceptually:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmMaintenance
metadata:
  name: my-idm-reindex-2026-08
spec:
  targetRef:
    name: my-idm
    uid: 1a2b3c4d-...
  operation: Reindex
  strategy: Auto
```

The `uid` is optional at creation time if admission/controller resolution fills or validates it, but status must pin the operation to the observed target UID before any mutation. A delete/recreate with the same `Kanidm` name must never inherit an old operation.

### Spec

Proposed Rust shape:

```rust
pub struct KanidmMaintenanceSpec {
    pub target_ref: KanidmTargetRef,
    pub operation: KanidmMaintenanceOperation,
    #[serde(default)]
    pub strategy: KanidmMaintenanceStrategy,
    pub target: Option<KanidmMaintenanceTarget>,
    pub timeout: Option<Duration>,
}

pub struct KanidmTargetRef {
    pub name: String,
    pub uid: Option<String>,
}

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
    Instance {
        replica_group: String,
        ordinal: u32,
    },
}
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

## Status model

Suggested phases:

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
