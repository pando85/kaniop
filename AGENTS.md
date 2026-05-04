# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Kaniop is a Kubernetes operator for managing Kanidm identity services. It provides declarative management of Kanidm clusters and identity resources (persons, groups, OAuth2 clients, service accounts) through Kubernetes Custom Resources.

**Tech Stack**: Rust (edition 2024), Cargo workspace, Kubernetes controller-runtime (kube-rs), Kanidm client SDK, Helm charts, Kind for e2e testing.

## Essential Commands

### Development Workflow
```bash
# Lint and format check (must pass with zero warnings)
make lint

# Run unit tests (includes lint)
make test

# Build debug binaries (kaniop and kaniop-webhook)
make build

# Build optimized release binaries
make release

# Regenerate CRDs after any CRD spec changes (REQUIRED)
make crdgen

# Regenerate example YAML files (do not hand-edit examples/)
make examples

# Update version across workspace and Helm chart
make update-version
```

### Testing

```bash
# Integration tests (requires Tempo tracing container)
make integration-test

# Create Kind cluster, build images, install operator
make e2e

# Run e2e tests (requires Kind cluster from `make e2e`)
make e2e-test

# Rapid iteration: rebuild operator and reload into Kind cluster
make update-e2e-kaniop

# Clean e2e resources but keep cluster
make clean-e2e

# Delete Kind cluster completely
make delete-kind
```

### Test a Single e2e Test
```bash
# After `make e2e` has created the cluster:
RUST_TEST_THREADS=16 cargo test -p kaniop-e2e-tests --features e2e-test <test_name>
```

### Build and Push Images
```bash
# Build single-arch local images
make images

# Build and push multi-arch images (amd64, arm64)
make push-images
```

## Architecture

### Workspace Structure

**Binaries** (`cmd/`):
- `cmd/operator`: Main operator binary with multiple controllers
- `cmd/webhook`: Validating admission webhook
- `cmd/crdgen`: CRD generator (outputs to `charts/kaniop/crds/crds.yaml`)
- `cmd/examples`: Example YAML generator (outputs to `examples/`)

**Libraries** (`libs/`):
- `libs/operator`: Core operator framework (controller runner, metrics, telemetry, Kanidm cluster CRD)
- `libs/person`: KanidmPersonAccount controller and reconciler
- `libs/group`: KanidmGroup controller and reconciler
- `libs/oauth2`: KanidmOAuth2 controller and reconciler
- `libs/service-account`: KanidmServiceAccount controller and reconciler
- `libs/k8s-util`: Kubernetes client utilities, error types, patch helpers

**Tests** (`tests/`):
- `tests/e2e/`: End-to-end tests with feature flag `e2e-test`
- `tests/integration/`: Integration tests with feature flag `integration-test`

**Important**: Never use both feature flags together.

### Controller Pattern

Each identity resource follows the same pattern:
1. **CRD** (`crd.rs`): Defines the Kubernetes Custom Resource
2. **Controller** (`controller.rs`): Sets up the kube-rs controller with watchers and reconciler
3. **Reconciler** (`reconcile.rs`): Implements the reconciliation logic using Kanidm client SDK

All controllers:
- Use exponential backoff on errors via `backoff_reconciler!` macro
- Share a common `Context` trait pattern with `IdmClientContext` and `BackoffContext`
- Access Kanidm via `KanidmClient` cached per-cluster
- Report metrics via Prometheus client

### Key Components

**libs/operator/src/controller.rs**: Core controller infrastructure
- `State`: Shared state across all controllers (metrics registry, Kanidm client cache)
- `Context` trait: Provides access to k8s client, metrics, Kanidm client
- `create_subscriber()`: Sets up controller with retry backoff

**libs/operator/src/kanidm/**: Kanidm cluster management
- `controller.rs`: Reconciles Kanidm StatefulSet, Services, PVCs
- `crd.rs`: Kanidm cluster CRD definition
- `reconcile/`: Stateful set, service, secret management logic

**libs/k8s-util/src/error.rs**: Centralized error handling
- `Error` enum for all controller errors
- `Result<T>` type alias used throughout

### Reconciliation Flow

1. Kubernetes watch event triggers reconciler
2. Reconciler retrieves Kanidm client from shared cache (via `Context::get_idm_client()`)
3. Reconciler performs idempotent operations against Kanidm API
4. Controller tracks backoff state on errors, retries with exponential backoff
5. Status updates written back to CR

### e2e Testing

- **Cluster**: Kind cluster named `chart-testing` (context: `kind-chart-testing`)
- **Namespace**: `kaniop`
- **Test execution**: Runs in serial per-resource (`RUST_TEST_THREADS=16` for parallel tests, override via `E2E_TEST_THREADS`)
- **Diagnostics**: On failure, operator logs dumped automatically
- **Image tagging**: Uses git SHA as version tag

#### e2e Iteration Workflow

When iterating on code changes with e2e tests:

1. **Initial setup** (once per session):
   ```bash
   make e2e  # Creates Kind cluster, builds images, installs operator
   ```

2. **Rapid iteration cycle**:
   ```bash
   # After code changes, rebuild and reload operator into cluster
   make update-e2e-kaniop

   # Run a specific test
   RUST_TEST_THREADS=16 cargo test -p kaniop-e2e-tests --features e2e-test <test_name>

   # Or run all e2e tests
   make e2e-test
   ```

3. **Cleanup between test runs** (if tests fail with "already exists" errors):
   ```bash
   # Clean up leftover test resources but keep cluster running
   make clean-e2e

   # Then re-run tests
   make e2e-test
   ```

4. **Delete specific leftover resources** (for targeted cleanup):
   ```bash
   kubectl delete kanidmgroup <name> -n default --ignore-not-found=true
   kubectl delete kanidmpersonaccount <name> -n default --ignore-not-found=true
   kubectl delete kanidmoauth2client <name> -n default --ignore-not-found=true
   kubectl delete kanidmserviceaccount <name> -n default --ignore-not-found=true
   kubectl delete kanidm <name> -n default --ignore-not-found=true
   ```

5. **Full reset** (when cluster is in bad state):
   ```bash
   make delete-kind && make e2e
   ```

**Common e2e test failure patterns**:
- `"already exists"` errors → Run `make clean-e2e` to remove leftover resources
- `"admission webhook denied"` with duplicate message → Previous test didn't clean up; delete the specific resource
- Tests pass individually but fail in batch → Resource name collision; ensure test names are unique

## Development Practices

### Code Style
- **Imports**: Always at top-level, grouped: std → external crates → internal crates → crate-local
- **Never inline imports** within functions or impl blocks
- **Async**: Never block Tokio runtime; use retry/backoff for transient failures
- **Performance**: Avoid unnecessary `clone`, prefer references and iterators
- **Error handling**: Use `anyhow::Context` for error wrapping, domain-specific error enums

### CRD Changes Workflow
1. Modify CRD in `libs/*/src/crd.rs`
2. Run `make crdgen` to regenerate `charts/kaniop/crds/crds.yaml`
3. If version bump needed, run `make update-version`
4. Never hand-edit generated CRD files

### Examples
- `cmd/examples` should always include values for the CRD fields they demonstrate.
- Prefer using real, representative values or explicit defaults (when known) instead of leaving fields empty, `null`, or omitted.
- When introducing new optional fields, update the example generator so users can see the default/expected shape immediately.
- **Never add explanatory comments in example code** (`cmd/examples/src/*.rs`). Comments in Rust code are NOT rendered in the generated YAML output.
- **All field documentation must be in CRD definitions** (`libs/*/src/crd.rs`). CRD doc comments (`///`) are extracted by the schema generator and rendered in YAML.
- Set new fields to `Some(...)` with concrete values, not `None`, so they appear in generated YAML documentation.
- Run `make examples` after modifying example code or CRD definitions.

### Dependencies
- Add shared dependencies to `[workspace.dependencies]` in root `Cargo.toml`
- Minimize new dependencies; only add if no internal equivalent exists
- Use specific versions for reproducibility

### Testing Strategy
- **Unit tests**: Test individual functions and modules
- **Integration tests**: Test with external services (e.g., Tempo tracing)
- **e2e tests**: Primary testing method; full operator behavior in Kind cluster

### Feature Requirements
- All new features **must** include e2e tests in `tests/e2e/`
- All new features **must** be documented with examples in `cmd/examples/`
- Run `make examples` to regenerate example YAML files after adding/modifying examples

### Robustness Requirements
Handle these scenarios gracefully:
- Empty CR lists
- Deletion during reconciliation
- Transient 5xx errors and timeouts from Kanidm/K8s APIs
- Context cancellation
- Idempotent operations (repeated reconcile should be safe)
- Upgrade version skew

## Common Pitfalls

- **Forgetting `make crdgen`** after CRD changes → version mismatch errors
- **Hand-editing generated files** in `examples/` or `charts/kaniop/crds/`
- **Running e2e tests without Kind context** → wrong cluster targeted
- **Mixing test feature flags** (`integration-test` + `e2e-test` → compilation error)
- **Blocking calls in async code** → runtime stalls
- **Not checking `make lint`** before committing → CI failure

## Commit Workflow

### Commit Process

1. **Always run lint before committing**: `make lint` must pass with zero warnings
2. **Create atomic commits**: Each commit should represent one logical change
3. **Follow conventional commit format**: Use prefixes like `feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `chore:`, `build:`, `ci:`
4. **Write clear commit messages**:
   - First line: `<type>: <brief description>` (max 72 chars)
   - Blank line
   - Optional body with more details
   - Reference issues when applicable: `Fixes #123` or `Resolves: #456`

### Fix and Feature Workflow

When fixing a bug or implementing a feature:

1. **Create a feature branch** from master:
   ```bash
   git checkout -b fix/<issue-number>-brief-description
   # or
   git checkout -b feat/<issue-number>-brief-description
   ```

2. **Implement the fix/feature**:
   - Make minimal, focused changes
   - Follow existing code patterns and conventions
   - Add/update tests as needed
   - Update documentation if applicable

3. **Verify before committing**:
   ```bash
   make lint        # Must pass with zero warnings
   make test        # Run unit tests
   ```

4. **Commit with proper message**:
   ```bash
   git add <files>
   git commit -m "fix: prevent upgrade to incompatible Kanidm version"
   # or with body:
   git commit -m "fix: prevent upgrade to incompatible Kanidm version" -m "Check desired image version for compatibility before updating StatefulSet."
   ```

5. **Push immediately**:
   ```bash
   git push origin <branch-name>
   ```

6. **Create pull request** if working on an issue:
   ```bash
   git-api pr create
   ```
   Include `Resolves: #<issue-number>` in PR description.

### Commit Message Examples

**Bug fix**:
```
fix: check version compatibility against desired image, not running

Previously, the operator checked the running StatefulSet image for version
compatibility, allowing upgrades to incompatible versions. Now checks the
desired image tag from the Kanidm CRD spec before applying changes.

Resolves: #759
```

**Feature**:
```
feat: add webhook validation for Kanidm image version

Validates that the specified Kanidm image version is compatible with the
operator's client SDK version before accepting the create/update request.
```

**Test**:
```
test: add e2e test for incompatible version blocking

Adds tests to verify that:
- Upgrading to incompatible version is blocked
- Creating with incompatible version is blocked
- Uses Kanidm 99.0.0 as incompatible version (future-proof)
```

**Documentation**:
```
docs: add commit workflow section to AGENTS.md

Documents the proper process for creating commits, feature branches, and
pull requests in this repository.
```

### What NOT to Commit

- Never commit secrets, credentials, or API keys
- Never commit `.env` files or configuration with sensitive data
- Never commit generated files that can be regenerated (unless tracking is intentional)
- Never commit work-in-progress code without a clear message indicating it's WIP

## CI/CD

- **Linting**: `make lint` must pass with zero clippy warnings
- **Testing**: All tests run via `make test`
- **Commit format**: Conventional Commits (feat, fix, docs, chore, etc.)
- **Pre-commit hooks**: Configured via `.pre-commit-config.yaml`
- **Auto-updates**: Renovate handles dependency updates

## Profiles

- **release**: Full optimization (LTO fat, opt-level 3, stripped symbols)
- **release-e2e**: Fast release for e2e (LTO thin, 32 codegen units)
- **e2e**: Debug with thin LTO for faster e2e builds

## Environment Variables

- `VERSION`: Image tag version (default: git SHA)
- `CARGO_TARGET`: Cross-compilation target (default: `x86_64-unknown-linux-gnu`)
- `CARGO_RELEASE_PROFILE`: Cargo profile for release builds (default: `release`)
- `E2E_LOGGING_LEVEL`: Log filter for e2e tests (default: `info,kaniop=debug,kaniop_webhook=debug`)
- `E2E_TEST_THREADS`: Parallel e2e test threads for `make e2e-test` (default: `16`)
- `E2E_WAIT_TIMEOUT_SECONDS`: Timeout for e2e wait conditions (default: `180`)
- `E2E_EVENT_TIMEOUT_SECONDS`: Timeout for waiting on Kubernetes events (default: `10`)
- `E2E_EVENT_POLL_INTERVAL_MILLISECONDS`: Poll interval for event checks (default: `1000`)
- `KANIDM_DEV_YOLO=1`: Required for e2e tests to prevent Kanidm client silent exit with dev profiles
