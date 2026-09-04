# Current Project Report

**Updated:** 2026-08-29

**Repository ref:** `2c3c700185346ff3b00713776243deb938429a21`

**Latest release considered:** `v0.14.2`

This is the living project report. Values here may change as Kaniop evolves. See the [2026-08-29 historical snapshot](history/2026-08-29.md) for the immutable baseline that introduced this analysis framework.

## Executive summary

Kaniop has evolved from a declarative Kanidm Kubernetes operator into an increasingly operationally focused control plane. The most important change is not the number of CRDs or commits; it is the shift from representing Kanidm resources to managing long-lived operational concerns: version compatibility, HA upgrades, replication safety, CRD migrations, leader election, observability, and disaster-recovery workflows.

The project is still highly concentrated in maintainer ownership, but code authorship understates the community. A meaningful portion of Kaniop's operational design comes from external users reporting real deployments, failure modes, metrics, integration constraints, and validated workarounds. AI/agent tooling then converts some of that knowledge into implementation at high velocity under maintainer review.

At this point the most accurate Operator Framework summary is:

- **Level II (Seamless Upgrades): strong and production-relevant**;
- **Level III (Full Lifecycle): substantial implementation exists, but backup/restore is explicitly experimental and therefore not yet production-qualified**;
- **Level IV-like Deep Insights features:** significant metrics, events, status and Grafana/Prometheus integration exist, but the cumulative production-qualified level should not skip the Level III gap;
- **Level V (Auto Pilot): not implemented and not currently a necessary project goal.**

## Baseline KPIs

| KPI | Current value | Notes |
|---|---:|---|
| Repository created | 2024-09-17 | 711 days old at this report |
| Latest Kaniop release | `v0.14.2` | pre-1.0 |
| Tags | 67 | includes 12 beta tags |
| GitHub stars | 121 | exact repository API value at report time |
| Forks | 12 | exact repository API value |
| Subscribers | 3 | actual `subscribers_count`, not legacy `watchers_count` |
| Merged PRs | 831 | raw total; attribution matters |
| Issues | 80 | excludes pull requests |
| Owner-authored issues | 20 | `author:pando85` |
| Issues excluding owner and Renovate | 58 | includes substantial production/user input; not assumed to be 58 unique human users |
| Resolved `kanidm_client` | **1.11.1** | from `Cargo.lock` |
| Manifest Kanidm requirement | `1.10.0` | Cargo-compatible requirement, not an exact resolved version |
| Explicit HA upgrade e2e | **1.10.0 → 1.11.1** | two replicas, replication validated before and after |

## Development phases

### 1. Incubation and declarative modelling — Sep 2024 to Nov 2025

The repository was created on 2024-09-17 and spent more than a year in beta before `v0.1.0` on 2025-11-08. The primary problem was making Kanidm clusters and identity resources declarative and reconcilable through Kubernetes.

The long beta period is important: e2e testing and core reconciliation existed before the first ordinary release rather than being retrofitted after adoption.

### 2. Identity lifecycle and compatibility — Nov 2025 to Feb 2026

The first ordinary releases expanded from CRUD-style reconciliation into lifecycle behavior:

- automatic OAuth2 secret rotation;
- service-account password/API-token rotation;
- account policy management;
- `kanidmName` for Kanidm identities that are not valid Kubernetes names;
- IPv6/configurable IP-family support;
- runtime Kanidm compatibility checks.

This period contains a good example of production knowledge changing the operator. [Issue #636](https://github.com/pando85/kaniop/issues/636) described a real Kaniop `0.4.2` / Kanidm `1.9.0` version-skew failure. The resulting implementation introduced early compatibility detection rather than allowing obscure downstream reconciliation failures.

### 3. Kubernetes platform integration — May to Jun 2026

Kaniop grew deeper Kubernetes integration:

- Gateway API / `HTTPRoute` support;
- `BackendTLSPolicy` integration;
- `runtimeClassName` and pod security controls;
- domain appearance customization;
- Kanidm mail-sender orchestration;
- Kanidm 1.10 support;
- operator leader election for HA control-plane deployments.

The operator was no longer only making Kanidm work in Kubernetes; it was adapting to different Kubernetes networking, security and platform environments.

### 4. Upgrade and schema safety — Jul 2026

`v0.11` introduced migration machinery for the `KanidmPersonAccount` plural correction. The important part was not the rename itself but the operational treatment:

- idempotent migration Job;
- Helm pre-upgrade integration;
- Argo CD `PreSync`/`PostSync` paths;
- preservation of specs, metadata, Argo tracking and Kanidm identity UUIDs;
- failure injection and resume testing;
- fresh-install and zero-person cases.

This is one of the clearest maturity milestones because the project treated a CRD naming mistake as a data-preservation problem rather than a documentation-only breaking change.

### 5. Disaster recovery, observability and hardening — Aug 2026

`v0.12` onward added backup/restore orchestration, data movement, operator performance metrics, expanded Grafana observability and more scalable e2e execution.

The backup/restore subsystem is intentionally documented as **experimental and incomplete**. Earlier language briefly overstated production readiness; [PR #981](https://github.com/pando85/kaniop/pull/981) corrected that. The correction is itself a positive governance signal: support claims should follow demonstrated production gates, not implementation breadth.

## Kanidm compatibility history

### Effective dependency progression

The historical analysis must use the resolved `Cargo.lock` package, not only the manifest requirement. Selected milestones are:

| Kaniop point | Resolved `kanidm_client` |
|---|---:|
| `v0.1.0` | 1.7.4 |
| `v0.2.0` | 1.8.5 |
| `v0.3.0` | 1.8.5 |
| `v0.4.0` | 1.8.5 |
| `v0.5.0` | 1.9.0 |
| `v0.6.0` | 1.9.1 |
| `v0.7.0` | 1.10.0 |
| `v0.8.0` | 1.10.0 |
| `v0.9.0` | 1.10.0 |
| late Jun 2026 | 1.10.4 |
| 2026-08-14 onward | **1.11.1** |
| current | **1.11.1** |

The current workspace still declares `kanidm_client = "1.10.0"`, but Cargo resolves that compatible requirement to `1.11.1`. Kaniop's `libs/operator/build.rs` deliberately extracts the resolved version from `Cargo.lock` and exports it as `KANIDM_CLIENT_VERSION`, so runtime compatibility logic is based on **1.11.1**.

### Tested upgrade contract

`tests/e2e/test/kanidm/upgrade.rs` derives the current Kanidm version from the dependency and constructs the previous-minor image. With the current lockfile, the HA scenario is effectively:

**Kanidm 1.10.0 → Kanidm 1.11.1**

The test creates a two-replica cluster, verifies availability/initialization and replication, upgrades the StatefulSet through the Kaniop resource, waits for the new image/readiness, and verifies replication again.

This is stronger evidence than a static compatibility table because it continuously exercises the N-1 transition used by upstream Kanidm's supported upgrade model.

## Operator capability maturity

| Capability | Assessment | Evidence |
|---|---|---|
| Level I — Basic Install | Achieved | declarative installation and managed Kanidm/identity resources |
| Level II — Seamless Upgrades | **Strong** | compatibility gate, Kanidm upgrade-check, N-1 HA e2e, replication validation, CRD migration safety |
| Level III — Full Lifecycle | **Implemented substantially, not production-qualified** | backup/restore/data-mover exist, but subsystem is explicitly experimental |
| Level IV — Deep Insights | **Significant feature coverage** | Prometheus/OpenTelemetry metrics, events/status, Grafana dashboards, performance metrics |
| Level V — Auto Pilot | Not achieved | no metrics-driven autoscaling/autotuning/self-healing policy loop |

The project should report both feature coverage and the cumulative production-qualified level. Today, "strong Level II with experimental Level III and Level IV-like observability" is more accurate than simply claiming Level IV.

## Testing maturity

Kaniop's testing history is one of its strongest signals.

Important characteristics include:

- e2e coverage predating the first ordinary release;
- feature-specific e2e tests for credential rotation and later capabilities;
- N-1 HA upgrade testing with replication verification;
- CRD migration tests across Helm and Argo CD workflows;
- migration failure injection, resume and idempotency;
- exact Kanidm UUID preservation testing;
- backup/restore scenarios under active hardening;
- six e2e shards per architecture as the suite became large enough to require execution scaling.

The six current logical shards are `kanidm-core`, `kanidm-ha`, `kanidm-data`, `oauth2`, `resources`, and `misc`.

The meaningful story is not simply "more tests". The CI architecture had to evolve because operational behavior became broad enough that the verification system itself became a scaling concern.

## Community and production knowledge

### Code authorship alone is misleading

The 831 merged PRs break down approximately as:

| Attribution | Merged PRs | Share |
|---|---:|---:|
| `pando85` | 264 | 31.8% |
| Renovate | 489 | 58.8% |
| Forkline bot | 71 | 8.5% |
| Other after excluding those three | 7 | 0.8% |

Raw PR volume therefore dramatically overstates independent human implementation breadth.

However, the opposite mistake is to conclude from seven externally authored PRs that the wider community contributes little. GitHub currently shows 80 issues, of which 20 are owner-authored and 58 remain after excluding the owner and Renovate. Many of those issues contain real operational evidence rather than generic requests.

### Representative production-derived contributions

[Issue #687](https://github.com/pando85/kaniop/issues/687) is a particularly strong example. It documented a cluster with 92 managed identity resources (45 groups, 40 OAuth clients and 7 persons), controller latency distributions and a failure mechanism where bursty periodic reconciliation could increase Kanidm latency enough to affect health probes. The resulting implementation made the IDM reconciliation interval configurable.

[Issue #636](https://github.com/pando85/kaniop/issues/636) supplied a concrete version-skew reproduction and directly led to runtime compatibility checking.

Other externally derived changes include requirements around:

- Kanidm names that violate Kubernetes naming rules;
- IPv6-only environments;
- runtime classes/sandboxing;
- cert-manager TLS renewal behavior;
- sidecars and explicit container selection for exec;
- managed Secret metadata/SSA interaction;
- Gateway API and other Kubernetes ecosystem integration.

This kind of contribution should be tracked as **production knowledge**, **design input**, **integration knowledge**, and **validation**, not hidden behind a single PR-author metric.

### Response/conversion velocity

Two clean issue-to-implementation examples show unusually fast conversion of production knowledge into capability:

| Issue | Production input | Approx. issue → implementation/close |
|---|---|---:|
| #636 | version-skew failure and downgrade validation | ~5 h 33 min |
| #687 | scale/load measurements and cascading probe-risk analysis | ~6 h 17 min |

These are not typo fixes; both changed operator behavior. Tracking this conversion latency over time may be more meaningful than raw contributor count.

## AI-assisted development

AI use is explicit and repository-level rather than incidental.

Observable evidence includes:

- `.github/copilot-instructions.md`;
- `AGENTS.md` as the canonical guidance for coding agents;
- OpenCode-native skills, agents, and permissions under `.opencode/` and
  `opencode.json`;
- explicit AI co-author metadata in commits;
- Forkline-authored PRs describing autonomous issue analysis and implementation;
- GitHub operations performed through the ChatGPT/Codex connector.

The strongest conservative metric is explicit agent PR authorship:

- **71 / 831 merged PRs = 8.5%** of all merged PRs;
- after removing 489 Renovate dependency PRs, **71 / 342 = 20.8%** of non-Renovate merged PRs.

This is a lower bound on AI-assisted development, not a claim that 20.8% of source code was generated by AI. Human-authored PRs may also be AI-assisted, and public Git history does not provide reliable line-level provenance.

A notable emerging workflow is:

**external operator/user contributes production evidence → agent implements a candidate change → CI/e2e validates behavior → maintainer reviews and merges.**

That model can expand implementation capacity without requiring every production user to become a Rust contributor.

## Architecture growth

At `v0.1.0`, the Cargo workspace contained 11 members. The current workspace contains 16.

The additional components are concentrated in operational subsystems such as:

- CRD migration;
- backup core/controller logic;
- data movement;
- dedicated migration/data-mover commands.

This is roughly 45% workspace-member growth, but the useful interpretation is qualitative: Kaniop has added **operational depth**, not merely more identity resource types.

## Stargazers and project reach

The current exact repository count is **121 stars**, with **12 forks** and **3 actual subscribers**.

Historical star velocity should be derived from GitHub's timestamped stargazer API (`application/vnd.github.star+json`) as required by the [methodology](methodology.md). The connector used for this baseline did not expose the timestamped stargazer listing, so this report intentionally does **not** substitute search-engine or third-party approximations.

Future updates should populate:

- cumulative stars by month;
- 30/90-day velocity;
- time to 25/50/100 stars;
- stars at major release/maturity milestones.

For a specialist Kanidm operator, production-quality issue reports may be a stronger adoption signal than raw stars. A 121-star repository receiving detailed reports about multi-replica behavior, real controller latency, cert-manager rotation, Gateway API, Cilium, GitOps upgrades and security policies indicates a technically engaged user base.

## Current risks and gaps

### Backup/restore production qualification

This is the largest operator-maturity gap. The implementation is substantial, but the subsystem is explicitly experimental. Production qualification should require demonstrated recovery invariants and tested failure modes rather than a feature-count threshold.

### Maintainer concentration

Architectural ownership, final review and project direction remain highly concentrated. AI implementation capacity and external production input reduce implementation bottlenecks but do not remove bus-factor risk.

### Pre-1.0 API surface

Kaniop has operational behavior that is sophisticated for a pre-1.0 project, but API/CRD stability still requires caution. The v0.11 migration work shows the correct approach when breaking changes are unavoidable.

### Attribution quality

PR authorship is measurable; actual AI assistance, human review depth, production deployment count and unique active operators are not fully observable from public Git history. Reports should preserve that uncertainty rather than invent precision.

### Historical stargazer series

The official API can provide exact `starred_at` history, but that series was not retrievable through the connector used for this baseline. It remains a known data gap rather than being approximated.

## Overall assessment

Kaniop's history is best described as a transition from **resource modelling** to **safe operation**.

Its implementation ownership is concentrated, but its operational knowledge is increasingly community-derived. The project has also developed an unusually explicit AI-assisted implementation pipeline. The combination is significant: external users can contribute production evidence and design constraints without needing to implement every fix themselves, while the maintainer retains architectural and review control.

The next major maturity threshold is not another CRD. It is making disaster recovery production-qualified while preserving the current upgrade/migration safety discipline and continuing to turn real production feedback into reproducible e2e behavior.
