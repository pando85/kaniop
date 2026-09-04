# Kaniop

Kaniop is a Rust Kubernetes operator for Kanidm clusters and identity resources.
This file is the canonical entry point for coding agents. Detailed workflows live
in `.opencode/skills/` and must be loaded when their descriptions match the task.

## Working contract

- Start non-trivial work by reading the issue and relevant code, then produce a
  reviewable plan before editing.
- For substantial or high-risk changes, maintain the durable artifacts described
  in `intent/README.md`: `intent.md`, `spec.md`, and `plan.md`.
- Keep the smallest correct diff. Do not introduce speculative refactors.
- For a bug, reproduce the failure first when practical. Confirm the regression
  test fails for the expected reason before implementing the fix.
- If implementation departs materially from an approved `plan.md`, update the
  plan in the same change and explain the deviation.
- Never weaken tests or verification merely to obtain a passing result.
- Before reporting completion, run the relevant checks and include their actual
  results. Use an independent verifier agent for substantial changes.
- Review against `REVIEW.md` before requesting human review.
- Convert significant escaped defects, repeated corrections, and incidents into
  eval cases under `evals/cases/`.

## Essential commands

```bash
make lint                    # rustfmt + clippy, zero warnings
make test                    # lint + unit tests
make build                   # debug binaries
make integration-test        # integration tests; requires Tempo
make crdgen                  # required after CRD changes
make examples                # regenerate example YAML
make agent-framework-check   # validate agent configuration and eval metadata
```

Do not run e2e locally by default. Let PR CI run it, then reproduce only a failing
shard with `make e2e && make e2e-test-shard SHARD=<name>`.

## Repository map

- `cmd/`: operator, webhook, CRD generator, and example generator binaries.
- `libs/operator/`: controller framework and Kanidm cluster reconciliation.
- `libs/{person,group,oauth2,service-account}/`: identity controllers.
- `libs/k8s-util/`: shared Kubernetes utilities and error handling.
- `charts/kaniop/`: Helm chart and generated CRDs.
- `tests/e2e/`: Kind-based end-to-end tests.
- `Documentation/`: mdBook user and architecture documentation.

## Non-negotiable repository rules

- Never hand-edit `charts/kaniop/crds/crds.yaml` or files under `examples/`;
  edit their Rust sources and regenerate them.
- Never enable `integration-test` and `e2e-test` together.
- Keep imports at module scope, grouped as std, external, internal, and local.
- Never block the Tokio runtime; preserve idempotency, cancellation, deletion,
  transient-error, and version-skew behavior in reconcilers.
- Shared dependencies belong in root `[workspace.dependencies]`; avoid new
  dependencies when an internal or standard-library solution exists.
- Behavioral features require tests; user-facing CRD changes require regenerated
  CRDs, examples, and documentation.
- Commits use Conventional Commits and must include the DCO sign-off.

## Skills to load

- `kaniop-development`: architecture, Rust/operator conventions, commands,
  testing, and detailed development workflows.
- `examples`: CRD and generated-example changes.
- `helm-chart`: Helm values, templates, schemas, and chart tests.
- `grafana-dashboard`: dashboard or PromQL changes.
- `release`: versioning and release preparation.
- `document-learnings`: capturing reusable knowledge after non-obvious work.

Instructions guide behavior; tests, CI, repository permissions, and human
approval are the enforcement layers. A coding agent may prepare a production
change but may not approve or merge its own pull request.
