---
title: Architecture
weight: 500
---

# Architecture

Kaniop is a Kubernetes operator for Kanidm. Its role is to turn Kubernetes desired state into safe,
repeatable operations on Kanidm clusters and identity resources.

The architecture deliberately separates **orchestration policy** from **identity and data-plane
correctness**. Kubernetes and Kaniop decide what the deployment should look like and reconcile it
toward that state. Kanidm remains responsible for the semantics and invariants of identity,
persistence, and replication.

## Responsibility Boundary

| Kaniop and Kubernetes | Kanidm |
| --- | --- |
| Desired cluster topology | Identity and authorization semantics |
| Workload lifecycle and placement | Database correctness |
| Storage and Kubernetes resource lifecycle | Replication protocol and conflict semantics |
| Upgrade and replacement sequencing | Replication state and convergence semantics |
| Maintenance and recovery orchestration | Safe server-side maintenance/recovery primitives |
| Kubernetes status, events, and conditions | Machine-readable server/data-plane state |
| Declarative identity-resource reconciliation | Validation and application of identity mutations |

This boundary is a safety property, not only a code-organization preference. Kaniop should not
reimplement Kanidm's distributed-system invariants, and Kanidm-specific correctness should not be
inferred from generic Kubernetes state.

## Reconciliation Is the Control Model

A Kaniop reconcile loop may be interrupted, retried, or executed more than once. Workflows must be
designed around that fact.

Operations initiated by Kaniop should therefore be:

- **idempotent**, or carry an operation identity that makes replay safe;
- **observable**, so the reconciler can distinguish pending, successful, failed, and retryable
  states;
- **restart-safe**, so controller failover does not lose the meaning of an in-progress operation;
- **convergent**, so repeated reconciliation moves the system toward the declared state rather than
  depending on a one-shot sequence of imperative commands.

Kubernetes resources and status are the durable control-plane record for orchestration. They are
not a substitute for server-side evidence about the contents or safety of the identity database.

## Kubernetes Health Is Not Database Correctness

Kubernetes exposes valuable orchestration signals: Pod readiness, EndpointSlice membership,
StatefulSet rollout state, volume attachment state, events, and object generations. Kaniop uses
those signals for the questions they can answer.

They do **not**, by themselves, prove distributed data invariants.

For example:

- a `Ready` Pod does not prove that it contains every write known to another replica;
- a completed StatefulSet rollout does not prove replication convergence;
- the absence of an error log does not prove that a replica is safe to remove;
- timestamps and retry delays are not substitutes for replication state;
- process liveness does not prove that an offline database operation is safe to start.

When a workflow depends on a Kanidm data or replication invariant, Kaniop should prefer an explicit,
machine-readable server signal or protocol that expresses that invariant. If Kanidm does not expose
enough state to establish safety, Kaniop should use a conservative workflow rather than inventing a
heuristic proof.

## Mechanism and Policy

Kaniop should decide **when and in what order** a cluster-level operation happens. The server should
decide **whether the corresponding data-plane transition is valid and safe**.

Examples include:

- Kaniop may select a replica for maintenance; the server must provide the semantic state needed to
  know when that replica can be taken out of service safely.
- Kaniop may orchestrate a recovery; the server must define the rules that distinguish a valid
  recovery from a conflicting history.
- Kaniop may coordinate an upgrade; server compatibility and on-disk migration invariants remain
  server responsibilities.

This keeps generic infrastructure concerns in the Kubernetes control plane while keeping
Kanidm-specific correctness in Kanidm.

## Compatibility and Conservative Fallbacks

Kaniop only automates an operation when it has a defensible safety model for that operation.
Capabilities may differ between Kanidm versions, so workflows should discover or validate the
signals and operations they depend on rather than assuming they exist.

When a native semantic primitive is unavailable, a conservative compatibility workflow is
acceptable if it preserves correctness. Kaniop should not preserve feature parity by parsing
human-oriented logs, depending on CLI formatting, guessing from timing, or duplicating
Kanidm's replication logic inside the operator.

This principle intentionally allows availability or automation to be reduced when the alternative
would be an unprovable data-safety assumption.

## Non-goals

Kaniop is not intended to:

- implement a second database replication protocol or distributed consensus system;
- infer replication convergence solely from Kubernetes health or rollout state;
- use human-oriented logs as a stable control-plane API;
- encode Kanidm database internals that should instead be expressed by supported server semantics;
- make unsafe topology or maintenance transitions only to preserve an automated workflow;
- replace Kubernetes with a separate generic orchestration substrate inside the operator.

## Design Direction

New operational features should preserve this boundary. When an implementation becomes complicated
because Kaniop cannot observe or request a Kanidm-specific state transition directly, the preferred
long-term solution is a small, explicit server-side primitive that can be consumed by a reconciler,
not additional inference in the operator.

The result should be a deliberately boring control loop: Kubernetes stores desired and observed
orchestration state, Kaniop reconciles policy, and Kanidm provides the semantic guarantees required
to manipulate identity and replicated data safely.
