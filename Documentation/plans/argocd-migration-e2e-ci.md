# Argo CD Migration E2E CI Plan

Status: proposed; no workflow changes have been implemented from this plan.

## Goal

Keep the Argo CD CRD migration path required for changes that can affect it without
spending approximately 30 minutes testing unrelated commits. Preserve periodic coverage
for dependency and platform drift, and make superseded pull request runs inexpensive.

## Current State

The `CRD Migration E2E` workflow uses trigger-level path filters and defines three jobs:

- Helm CRD migration E2E
- Argo CD CRD migration E2E
- CRD migration failure injection

The Argo CD job validates behavior not covered by a direct Helm upgrade, including Helm
hook mapping to Argo CD sync phases, Git revision handling, application health, server-side
apply, tracking annotation preservation, and repeated sync idempotency.

Trigger-level path filters avoid many unrelated runs, but they have two limitations:

- A workflow that does not start cannot report a stable required check for an unrelated PR.
- Broad files such as `Makefile` and `charts/kaniop/values.yaml` run every expensive job even
  when the changed section cannot affect migration.

The Helm job runs whenever the workflow starts. The Argo CD and failure-injection jobs also
run for pull requests and pushes, but their manual execution is controlled by workflow
inputs. The workflow lacks concurrency cancellation and scheduled drift validation.

## Decisions

1. Keep Argo CD migration E2E required while upgrades from the legacy
   `kanidmpersonsaccounts` CRD remain supported and the migration hooks are shipped.
2. Run expensive migration jobs only for migration-relevant changes, scheduled runs, pushes
   to `master`, or an explicit manual override.
3. Make one lightweight aggregate job the required branch-protection check. It must run on
   every PR and succeed when expensive jobs were intentionally skipped.
4. Cancel superseded runs only for pull requests. Never cancel `master`, scheduled, or manual
   runs automatically.
5. Prefer conservative change detection. A false-positive E2E run costs time; a false
   negative could ship an unsafe migration.

## Implementation

### 1. Replace trigger-level PR path filters

Update `.github/workflows/crd-migration-e2e.yml` so `pull_request` always starts the
workflow. Add a lightweight `changes` job that classifies the PR diff and publishes boolean
outputs for the Helm, Argo CD, and failure-injection jobs.

The detector should use the pull request base SHA and head SHA rather than the previous
commit. For pushes, compare the event's `before` and `after` SHAs. Treat an unavailable or
ambiguous diff as relevant and run the tests.

Initially classify the following as relevant to all migration paths:

- `cmd/crd-migrator/**`
- `cmd/crdgen/**`
- `libs/crd-migration/**`
- `libs/person/**`
- `tests/e2e/test/crd_migration.rs`
- `charts/kaniop/templates/crd-migration/**`
- `charts/kaniop/templates/_helpers.tpl`
- `charts/kaniop/templates/deployment.yaml`
- `charts/kaniop/templates/serviceaccount.yaml`
- `charts/kaniop/templates/clusterrole.yaml`
- `charts/kaniop/templates/clusterrolebinding.yaml`
- `charts/kaniop/templates/webhook/deployment.yaml`
- `charts/kaniop/templates/webhook/job-patch/**`
- `charts/kaniop/Chart.yaml`
- `charts/kaniop/values.yaml`
- `charts/kaniop/values.schema.json`
- `.github/workflows/crd-migration-e2e.yml`

Classify these as relevant to their corresponding path:

- `tests/e2e/scripts/crd-migration-helm.sh`: Helm migration
- `tests/e2e/scripts/crd-migration-argocd.sh`: Argo CD migration
- `tests/e2e/scripts/crd-migration-failures.sh`: failure injection

Keep these conservative shared inputs until a later review proves narrower detection safe:

- `Makefile`
- `Cargo.toml` and `Cargo.lock`; dependency-only changes can alter the migrator or test
  binaries, although this intentionally trades additional Renovate runs for safety
- `.github/kind-cluster.yaml`

The operator deployment, webhook hooks, and RBAC paths are included because they can affect
adoption, health, and sync ordering. Document every pattern next to the detector. Reassess
the broad `Makefile`, workspace manifest, lockfile, and values triggers after the two-week
monitoring period, but narrow them only with evidence that false negatives remain unlikely.

### 2. Conditionally run expensive jobs

Make each existing job depend on `changes` and run when its output is true. Force all three
jobs to run for:

- a scheduled event;
- a push to `master`;
- `workflow_dispatch` with a `force_run` input.

Retain manual inputs that allow Argo CD and failure-injection jobs to be disabled for focused
diagnosis. Add a `legacy_chart_version` input so maintainers can validate a specific upgrade
source without editing the workflow. Pass that input to each selected job as
`LEGACY_CHART_VERSION`; the existing scripts already read that environment variable.

Do not change the migration scripts, Rust assertions, chart hooks, or Makefile targets as
part of this CI-only change.

### 3. Add a stable required gate

Add a final `Migration E2E Gate` job with `if: always()` that depends on `changes` and all
three E2E jobs.

The gate must:

- fail when a required E2E job fails, is cancelled, or does not run despite a matching
  change;
- succeed when all required jobs pass;
- succeed with a clear message when no migration-relevant files changed;
- reject unexpected job result values rather than treating them as a skip.

Implement the gate from an explicit expected-job matrix. For each E2E job, derive whether it
was required from the `changes` outputs, event type, `force_run`, and manual enable/disable
inputs, then validate `needs.<job>.result` as follows:

| Expected? | Job result | Gate result |
|---|---|---|
| Yes | `success` | Continue |
| Yes | `failure`, `cancelled`, or `skipped` | Fail |
| No | `skipped` | Continue |
| No | `success` | Continue and report the extra validation |
| No | `failure`, `cancelled`, or any unknown value | Fail |

Do not infer that every `skipped` result is intentional. Log each job's expected flag and
actual result in the gate output so branch-protection failures are diagnosable.

Configure branch protection to require only `Migration E2E Gate`, not the conditional job
names. Make the branch-protection change only after verifying the gate on both a relevant
and an unrelated test PR.

### 4. Cancel superseded PR runs

Add workflow concurrency equivalent to:

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.run_id }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
```

The group must be stable across commits in the same PR and unique for non-PR events. Confirm
that two pushes to one PR cancel the older run while separate PRs continue independently.

### 5. Add drift and manual coverage

Add a weekly schedule that runs the complete migration suite. Keep `workflow_dispatch` for
on-demand diagnosis and full-suite execution.

Use Monday at 06:00 UTC initially, outside the repository's busiest PR period, and adjust it
if ownership or runner availability requires a different window.

Scheduled coverage should detect drift in:

- the supported Argo CD version;
- Kind and Kubernetes behavior;
- published legacy chart and images;
- Helm hook translation and sync semantics;
- ingress and cluster prerequisite manifests.

Before enabling the schedule, name the maintainer or team responsible for triaging failures
in the workflow comments or contributing documentation. Rely on GitHub Actions failure
notifications initially; do not add a separate notification integration unless recurring
unattended failures show it is necessary.

## Validation

Before making the new gate required, exercise these cases:

| Change or event | Helm | Argo CD | Failure injection | Gate |
|---|---:|---:|---:|---:|
| Unrelated documentation PR | Skip | Skip | Skip | Pass: intentional skips |
| Migrator library change | Run | Run | Run | Pass only if all succeed |
| Argo CD script change | Skip | Run | Skip | Pass only if Argo CD succeeds |
| Helm script change | Run | Skip | Skip | Pass only if Helm succeeds |
| Failure script change | Skip | Skip | Run | Pass only if failure job succeeds |
| Ambiguous diff | Run | Run | Run | Pass only if all succeed |
| Push to `master` | Run | Run | Run | Pass only if all succeed |
| Weekly schedule | Run | Run | Run | Pass only if all succeed |
| Manual force run | Run | Run | Run | Pass only if all succeed |
| Manual Argo CD disabled | As selected | Skip | As selected | Ignore intentional Argo CD skip |
| Manual failures disabled | As selected | As selected | Skip | Ignore intentional failure skip |
| Superseded PR commit | Cancel old run | Cancel old run | Cancel old run | New run decides |

Also verify that a failed, timed-out, or cancelled Argo CD job cannot produce a successful
gate and that skipped jobs do not block unrelated PRs.

## Rollout

1. Add concurrency cancellation first and observe several PR updates.
2. Add change detection and conditional jobs, then exercise every change classification.
3. Add the aggregate gate and validate failure, cancellation, and intentional-skip handling.
4. Add the schedule and manual inputs, including `LEGACY_CHART_VERSION` propagation.
5. Validate relevant and unrelated PR cases without changing branch protection. Use a
   temporary branch targeting `master`; do not alter protection until both cases report the
   gate reliably.
6. Require `Migration E2E Gate` and remove any conditional migration jobs from branch
   protection.
7. Monitor for two weeks for false-positive runs, false-negative classifications, and
   unattended scheduled failures.

## Lifecycle

Review this workflow at each release that changes the supported upgrade window:

- **Required on relevant PRs:** while direct upgrades from v0.10.2 or older are supported
  and migration hooks remain in the chart.
- **Scheduled and manual only:** after the documented minimum supported direct-upgrade
  version is newer than v0.10.2, while the migration hooks remain available for
  compatibility.
- **Remove the infrastructure E2E:** when the migration hooks and migrator are removed from
  the shipped chart. Retain fast unit tests for any migration code that remains.

Do not transition between stages solely because a particular version number was reached.
Base the decision on the documented upgrade policy, shipped chart behavior, and known user
support requirements.

## Completion Criteria

- Unrelated PRs receive a successful `Migration E2E Gate` within two minutes without
  creating a cluster.
- Every migration-relevant PR runs the required Argo CD path before merge.
- New commits cancel obsolete runs for the same PR.
- Weekly and manual full-suite runs work independently of changed paths.
- The aggregate gate cannot hide failed, cancelled, or unexpectedly skipped required jobs.
- The workflow's required-to-scheduled-to-removed lifecycle is tied to the supported upgrade
  path and documented at each transition.
