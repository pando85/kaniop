# Specification: Retire the v0.11 KanidmPersonAccount CRD plural migration

Derived from: `intent.md` in the same change

## Required behavior

1. `kaniop-crd-migrator` exposes only the `apply-crds` subcommand. The
   `migrate-person-account` and `verify-person-account` subcommands and their flags
   (`--namespace`, `--operator-namespace`, `--operator-deployment`, `--marker-name`) are gone.
2. `apply-crds` refuses to run when the legacy CRD `kanidmpersonsaccounts.kaniop.rs` exists.
   It checks before applying any CRD, fails the hook Job, and logs a message that names the
   legacy CRD and the supported upgrade path (a v0.11.0 to v0.19.3 release first).
3. `apply-crds` fails when an applied CRD reports `NamesAccepted=False`, with the CRD name,
   the API server reason, and the same upgrade hint. It used to skip such CRDs because the
   migration hook resolved the conflict; nothing resolves it now, and an unserved CRD would
   leave the operator broken.
4. The chart no longer renders the migration ServiceAccount, RBAC, or the migrate/verify Jobs,
   and `crdMigration` is no longer a chart value. Because the values schema sets
   `additionalProperties: false`, values that still set `crdMigration` fail rendering.
5. `crdApply` behavior, hook weights (`-40` RBAC, `-35` Job), field manager
   (`kaniop-helm-crds`), and values are unchanged.

## Acceptance criteria

- Unit tests cover both new error messages.
- A chart unit test asserts that `crdMigration` values are rejected.
- The CRD upgrade e2e (Helm and Argo CD) installs the previous published chart, makes every
  Kaniop CRD stale with the hook's field manager (stale schema description plus a marker
  annotation), upgrades to the current chart with `crdApply` enabled, and asserts that every
  live CRD spec matches `charts/kaniop/crds/crds.yaml`, that the marker annotation is gone
  (only the hook's field manager can remove it), that existing person objects are still
  Ready, and that a person created after the upgrade becomes Ready. A second upgrade or sync
  re-applies the CRDs again. Argo CD also applies the chart's `crds/` directory itself, and on
  the first sync after the hook manifests change it can report the PreSync Job as succeeded
  while the new Job still fails; the Argo CD test therefore requires only current CRDs after
  the upgrade sync and requires the hook (marker removed) on the following sync.
- The CRD upgrade workflow runs on changes to CRD sources (`libs/**/crd.rs`,
  `libs/operator/src/kanidm/restore/legacy.rs`, `cmd/crdgen/**`), dependency manifests
  (`Cargo.toml`, `Cargo.lock`, `libs/*/Cargo.toml`), `charts/**`, `cmd/crd-migrator/**`,
  `Dockerfile`, `Makefile`, and the workflow and scripts themselves.
- The upgrade docs recommend v0.19.3 or earlier as the intermediate release for v0.10.x users
  (v0.20.0's migration verification fails when persons exist) and document how to delete the
  RBAC, Jobs, marker ConfigMap and backup Secrets the migration hooks left behind.

## Design

Delete `libs/crd-migration` (no other crate uses it; `apply_crds.rs` has no dependency on it),
the two migration subcommands, `charts/kaniop/templates/crd-migration/`, the `crdMigration`
values and schema, `tests/e2e/test/crd_migration.rs`, and the failure-injection script. The
Helm and Argo CD scripts are renamed to `crd-upgrade-{helm,argocd}.sh`, share helpers in
`crd-upgrade-common.sh`, and no longer need Rust: the checks are `kubectl` and `jq`.

## Interfaces and data

- Removed CLI subcommands and flags (internal to the chart hooks).
- Removed chart values: `crdMigration.personAccountPlural.*`.
- Removed Makefile targets: `e2e-crd-migration-helm`, `e2e-crd-migration-argocd`,
  `test-crd-migration-failures`; added `e2e-crd-upgrade-helm`, `e2e-crd-upgrade-argocd`.

## Security and privacy

No new permissions. The `crdApply` ClusterRole already grants `get` on CRDs, which the legacy
CRD check uses.

## Kubernetes, upgrade, and compatibility considerations

- Breaking: the minimum upgrade source is v0.11.0. Users on v0.10.x or older hit the legacy
  CRD check in the pre-upgrade hook; Helm and Argo CD stop before any chart resource or CRD
  changes, so the old operator keeps running.
- Users with `crdMigration` in values must remove it.
- Downgrading to a v0.11.0 to v0.20.x chart restores the migration hooks, which are no-ops on
  a migrated cluster.

## Operational behavior and rollback

Rollback is a `helm rollback` or Argo CD sync to the previous chart. No data or CRD is deleted
by this change.

## Concerns and unresolved questions

A cluster that kept an empty legacy CRD after a manual migration is blocked by the legacy CRD
check. The message names the CRD; deleting it after confirming it holds no objects unblocks
the upgrade.
