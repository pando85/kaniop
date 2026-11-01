# Intent: Retire the v0.11 KanidmPersonAccount CRD plural migration

- Owner: pando85
- Status: accepted
- Work item: maintainer decision to retire the migration added in #894 (v0.11.0)

## Problem

PR #894 (v0.11.0, July 2026) added a one-time migration that moves `KanidmPersonAccount`
objects from the legacy CRD `kanidmpersonsaccounts.kaniop.rs` (v0.10.2 and older) to the
corrected `kanidmpersonaccounts.kaniop.rs`. It still ships in every release as two Helm hook
Jobs, a `libs/crd-migration` crate of about 4,500 lines, chart values, and a dedicated CI
workflow that runs three Kind jobs (Helm, Argo CD, failure injection). Every supported
installation completed or skipped that migration long ago, so the code and CI only cost
maintenance and runner time.

The CRD upgrade workflow also only runs when migration files change. A CRD schema or chart
change can therefore break the `crdApply` upgrade path without any upgrade test running.

## Proposed outcome

- Releases after v0.20.x no longer contain the plural migration: no subcommand, no crate, no
  chart hooks or values, no plural e2e test.
- Upgrades are supported only from v0.11.0 or later. Users on v0.10.x or older get a clear,
  fail-safe stop and a documented path (upgrade to a v0.11.0 to v0.20.x release first).
- The `crdApply` hook, which server-side applies all generated CRDs before every install and
  upgrade, is kept unchanged in purpose.
- CI tests that `helm upgrade` and an Argo CD sync from the previous release apply the current
  CRDs via `crdApply`, and the test runs on any CRD source or chart change.

## Affected users and systems

- Users on v0.11.0 or later: no behavior change apart from `crdMigration` values now being
  rejected by the chart values schema.
- Users on v0.10.x or older: must step through a v0.11.0 to v0.20.x release first.
- Maintainers and CI: one crate fewer; the upgrade workflow has two jobs instead of three and
  no longer compiles Rust e2e tests.

## Constraints and non-goals

- The `crdApply` hook (`cmd/crd-migrator/src/apply_crds.rs`, `crdApply` values) must stay.
  Helm never updates CRDs itself.
- The binary keeps its name `kaniop-crd-migrator` and the `apply-crds` subcommand, so the
  chart and image layout do not change.
- The person finalizer string `kanidmpersonsaccounts.kaniop.rs/finalizer` stays: live objects
  carry it.
- No CRD schema change.
- The historical v0.11 migration guide stays for users who still need to step through it.

## Success measures

- `make lint`, `make test`, `helm unittest charts/kaniop`, `make agent-framework-check`,
  `make docs-nav-check`, and `make llms-check` pass; `make crdgen && make examples` leave the
  tree clean.
- The adapted Helm (and Argo CD) CRD upgrade scripts pass on a fresh Kind cluster locally and
  in CI.

## Open questions

None.
