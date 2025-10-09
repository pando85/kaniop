# Kaniop Repository Copilot Instructions (Lean)

Optimized high-signal guidance for this Rust monorepo (Kubernetes operator + domain crates + Helm
chart + e2e harness). Always prefer existing Make targets over ad‑hoc commands.

## Project Overview

- Purpose: Reconcile Kanidm clusters + declarative identity resources (person/group/oauth2) on
  Kubernetes.
- Stack: Rust latest stable version (edition 2024), Cargo workspace, Kind + Helm for e2e, Docker
  multi-arch, mdBook docs.
- Crates: `cmd/operator`, `cmd/crdgen`, `cmd/examples`,
  `libs/{operator,group,person,oauth2,k8s-util}`, `tests`.
- Helm chart: `charts/kaniop` (Chart.yaml, templates, values, crds/).
- Tests feature flags: `integration-test`, `e2e-test` (never together).

## Engineering Contract (FAANG-10x Style)

- Smallest correct diff; no speculative refactors unless requested.
- Search & confirm target module before editing; keep crate boundaries clean.
- Behavior change => add/adjust tests first (happy path + 1 edge) then implement.
- All clippy warnings must be zero (`make lint`).
- Async: never block Tokio; use existing retry/backoff for transient K8s/network failures.
- Errors/logging: reuse `anyhow::Context`, domain error enums, `tracing` setup; no new frameworks.
- Dependencies: add only if no internal equivalent & minimal; put shared ones in
  `[workspace.dependencies]`.
- CRD/API changes: run `make crdgen`; if version bump needed, `make update-version`. Never hand-edit
  generated CRDs.
- Performance: avoid unnecessary `clone`; prefer refs/iterators; early-return fast-fail paths.
- Robustness: handle empty lists, deletion mid-reconcile, transient 5xx/timeouts, context cancel,
  idempotency, upgrade skew.
- Output: provide minimal diff hunks, not whole files.
- YAML formatting: emit compact arrays without extra inner spaces (e.g. `[master,main]` or
  `[master, main]` consistently—no trailing or double spaces) and quote reserved keys like `on` in
  GitHub Actions when needed (`'on':`).
- Imports: Always place `use` statements at the top level of the file, grouped by origin (std, external crates, crate-local). Never add imports inline within functions or impl blocks.
- Underspecified ask: state ≤2 assumptions, proceed.
- New Make target: add `##` comment (shows in `make help`).
- Success = build + tests green, zero clippy warnings, CRDs synced, tests cover new logic, minimal
  diff.

LLM Style:

- Lead with intent + next action; bullets > prose; no filler.
- Map each user requirement → explicit action/result.
- Provide deltas only on iterative turns.
- Trust this file first; search only if info missing or command fails.

## Make Targets Cheat Sheet

Lint / Build / Test:

- `make lint` (clippy + fmt check)
- `make test` (runs lint then tests)
- `make build` (debug `kaniop`)
- `make release` (optimized build)

Images & Release:

- `make image` (single-arch local)
- `make push-images` (multi-arch buildx)
- `make update-version` (sync versions + chart annotations)
- `make crdgen` (regenerate CRDs) — ALWAYS after CRD spec edits

Docs / Examples:

- `make examples` (YAML examples)
- `make book` (mdBook build)
  - Generated example YAML lives in `examples/` and is produced by the `examples-gen` binary
    (`cmd/examples`, invoked via `make examples`). Do NOT hand-edit generated files;

Integration & e2e:

- `make integration-test` (Tempo + feature `integration-test`)
- `make e2e` (Kind cluster + image + Helm install)
- `make e2e-test` (feature `e2e-test`, logs on failure)
- `make update-e2e-kaniop` (rebuild + reload + Helm upgrade + restart)
- `make clean-e2e` (purge resources, keep cluster)
- `make delete-kind` (destroy cluster)

Safe Ordering:

1. `make lint`
2. `make test`
3. (optional) `make integration-test`
4. `make e2e` → `make e2e-test`
5. Iterate: code → `make update-e2e-kaniop` → `make e2e-test`
6. Release: `make crdgen` (if CRDs) → `make update-version` → `make push-images`

## e2e Workflow (Kind + Helm)

1. `make e2e` creates Kind cluster `chart-testing` (context `kind-chart-testing`), loads image
   `<git sha>`, installs ingress + chart, waits for Deployment & ingress controller readiness.
2. `make e2e-test` (context required) runs tests with feature `e2e-test`; dumps operator logs on
   failure.
3. Rapid iteration: `make update-e2e-kaniop` then `make e2e-test`.
4. Cleanup: `make clean-e2e` (soft) or `make delete-kind` (hard).

Preconditions: correct kube context; kind, kubectl, helm, docker installed (buildx for multi-arch).
Optional env: `VERSION`, `CARGO_TARGET`, `E2E_LOGGING_LEVEL` (default `info,kaniop=debug`).

Diagnostics: readiness failure auto-describe & logs; test failure prints operator logs.

## Edge Cases & Quick Facts

Checklist: empty CR list, deletion mid-reconcile, transient API errors (retry), finalizer removal,
upgrade skew, backoff sanity, CRD version mismatch (forgot `make crdgen`).

Facts:

- Cluster/context: `chart-testing` / `kind-chart-testing`
- Namespace / release: `kaniop`
- Image repo: `ghcr.io/pando85/kaniop`
- Cargo profiles: `release`, `release-e2e`, `e2e`
- Tests lib entry: `tests/e2e/lib.rs`
- Generated CRDs: `charts/kaniop/crds/crds.yaml`

## Avoid / Anti-Patterns

- Hand-editing generated CRDs and examples.
- Adding dependencies for trivial helpers.
- Blocking calls in async code.
- Duplicating image build shell pipelines (use Make).
- Running e2e tests outside Kind context.
- Publishing before `make update-version`.

## Trust These Instructions

Default to these; search only when commands fail or new subsystems emerge. Keep diffs minimal, tests
authoritative, crate boundaries clean.

---

If reality diverges from this file, update this file first, then proceed.
