# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.0.0-beta.2](https://github.com/pando85/kaniop/tree/v0.0.0-beta.2) - 2025-10-09

### Added

- group: Support cross namespace reference
- person: Support cross namespace reference

### Documentation

- kanidm: Add default security context
- oauth2: Add namespace to kanidm ref and secret creation documentation

### Build

- ci: Fix cargo login token
- deps: Update Rust crate serde to v1.0.228
- deps: Update Rust crate axum to v0.8.6
- deps: Update Rust crate thiserror to v2.0.17
- Fix cargo publish and change to `--workspace`

### Refactor

- operator: Implement KanidmResource trait in library
- operator: Move `is_resource_watched` logic to library

## [v0.0.0-beta.1](https://github.com/pando85/kaniop/tree/v0.0.0-beta.1) - 2025-10-05

### Added

- operator: Make KanidmRef as inmutable
- person: Make reset cred token TTL configurable

### Fixed

- ci: Remove dead create manifest code on docker images workflow
- ci: Add fmt and clippy for build tests
- kanidm: Make PersistentVolumeClaim metadata field optional

### Documentation

- kanidm: Add LDAP port protocol docs
- Add copilot instructions
- Add examples-gen feature
- Add enum options with default markers to examples
- Fix quickstart guide for getting Kanidm working

### Build

- ci: Add `tracing-opentelemetry` to opentelemetry renovate PRs
- deps: Update actions/checkout action to v5
- deps: Update opentelemetry
- deps: Update Cargo.lock
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.23.0

### Testing

- ci: Add verify examples tests

## [v0.0.0-beta.0](https://github.com/pando85/kaniop/tree/v0.0.0-beta.0) - 2025-09-23

### Added

- chart: Add livenessProbe
- chart: Add validating admission policy for checking names
- ci: Add scope-enum to commitlintrc
- ci: Enable pre-commit renovate updates
- ci: Add renovate auto migrate configuration
- error: Add context and deprecate metrics error labels
- group: Add controller
- group: Enchance CRD columns with new status fields
- group: Enable `entryManagedBy` field
- k8s-util: Add recorder with aggregation logic
- kanidm: Add ingress, service and LDAP configuration
- kanidm: Add storage generation
- kanidm: Add TLS configuration
- kanidm: Add env to allow config params
- kanidm: Allow service type and annotations configuration
- kanidm: Add services and ingress controller watchers and stores
- kanidm: Use real statefulset, service and ingress
- kanidm: Rework Kanidm status
- kanidm: Add secret watcher and store
- kanidm: Generate admin secrets
- kanidm: Add initialized condition and e2e tests for admin secrets
- kanidm: Add different replication groups support
- kanidm: Add external replication nodes configuration
- kanidm: Enhance CRD columns with new status fields
- oauth2: Add controller
- oauth2: Allow cross-namespace deployments
- oauth2: Add secret
- oauth2: Final implementation of oauth2 secret with tests
- oauth2: Enchance CRD columns with new status fields
- operator: Support multiple stores per context
- operator: Add backoff when reconcile fails
- operator: Add Kanidm system clients
- person: Add controller
- person: Enable controller, finish feature and add tests
- person: Add posix attributes
- person: Add credentials reset link
- person: Add event when update fails
- person: Enchance CRD columns with new status fields
- Add helm chart
- Add clap for handling args and rework telemetry init
- Add owner references and react to changes on owned resources
- Add state reconciliation
- Add status.conditions and ready column
- Add echo status tests and refactor reconcile
- Add e2e tests
- Add kubernetes client metrics
- Add metrics to kubernetes client requests total per status code
- Add metrics to controller
- Change to crdgen and implement StatefulSetExt for Kanidm
- Add transparent and svg logo
- Add Kanidm store to Context
- Split `controller::Context` and create `kanidm::Context`
- Split person `Context`
- Add kanidm_ref to columns

### Fixed

- chart: Truncate version label to 63 chars
- chart: Version label equal to left side of `@` symbol
- ci: Remove deprecated `crd-code` target and add mkdir for crdgen
- ci: Clippy Github Action name typo
- ci: Just run e2e-tests in x86 and add cache for release target
- ci: Change log level to info in e2e tests
- ci: Renovate update just patch versions of kind image
- ci: Schedule renovate for `renovatebot/pre-commit-hooks` once per month
- ci: Migrate pre-commit configuration
- ci: Add permissions for publishing releases on github actions
- ci: Add `.yml` files to enable kind
- ci: Group opentelemetry update PRs
- ci: Configure mdbook version
- ci: Remove `/opt/hostedtoolcache` dir on github actions runners
- cmd: Handle SIGTERM signal
- crd: Add pattern for domain field and fix tests
- crd: Add `kaniop` category
- k8s-util: Update events version from e8e4b54
- k8s-util: Update events version from d85f31
- kanidm: Add service per pod for workaround replication
- kanidm: Delete all objects at once using `store.state()`
- kanidm: Add different keys for certs on replication configuration
- kanidm: Change version to v1beta1
- Cleaner log messages reusing spans from kube_runtime and remove trace_id
- Add trace_id to logs
- Handle unwrap on metrics encode
- Handle unwraps in echo controller
- Clean small TODOs
- Add `metrics.status_update_errors_inc()`
- Correct crd status types and typo
- Replace `map_or` with `is_some_and`
- Make clippy happy for rust 1.86.0
- Use an empty dir volume for server config file
- Make clippy happy for rust 1.87.0
- Cargo clippy errors 1.88

### Documentation

- chart: Add artifacthub annotations
- kanidm: Add env link to Kanidm official documentation
- person: Add posix person to examples
- Add README features
- Show correct binary path in make build target
- Add logo
- Fix logo URL
- Generate examples programmatically
- Add supported versions
- Update TODO comment PR number
- Add book
- Fix URL links
- Reorder Oauth2 client, group and person quickstart
- Fix logic for commenting line if parent is optional
- Fix readme links

### Build

- ci: Change rash image to ghcr.io registry and add renovate
- ci: Publish helm chart
- ci: Add kind version to renovate
- ci: Fix repository name on Github Actions references
- ci: Enable push-images on Makefile
- ci: Add `--provenance false` to docker buildx
- ci: Fix docker multiarch image push
- ci: Fix CRD gen on helm release
- ci: Auto update renovate pre-commit once a month automatically
- ci: Configure debian image to versioning loose in renovate
- ci: Fix `managerFilePatterns` expresions in renovate
- ci: Handle mdbook version with renovate
- deps: Update Rust crate tonic to v0.12
- deps: Update Rust opentelemetry crates to v0.26
- deps: Update Rust crate futures to v0.3.31
- deps: Update Rust crate tokio to v1.40.0
- deps: Update Rust crate hyper to v1.5.0
- deps: Update kube-rs to 0.96 and tower to 0.5
- deps: Update Rust crate serde_json to v1.0.129
- deps: Update Rust crate anyhow to v1.0.90
- deps: Update Rust crate serde_json to v1.0.130
- deps: Update Rust crate serde_json to v1.0.131
- deps: Update Rust crate serde_json to v1.0.132
- deps: Update Rust crate serde to v1.0.211
- deps: Update Rust crate serde to v1.0.212
- deps: Update Rust crate thiserror to v1.0.65
- deps: Update Rust crate anyhow to v1.0.91
- deps: Update Rust crate serde to v1.0.213
- deps: Update Rust crate tokio to v1.41.0
- deps: Update Rust crate serde to v1.0.214
- deps: Update Rust crate hyper-util to v0.1.10
- deps: Update Rust crate tokio to v1.41.1
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.0
- deps: Update Rust crate serde to v1.0.215
- deps: Update Rust crate clap to v4.5.21
- deps: Update Rust crates tracing-opentelemetry to 0.28
- deps: Update Rust crate serde_json to v1.0.133
- deps: Update Rust crate anyhow to v1.0.93
- deps: Update Rust crate thiserror to v1.0.69
- deps: Update Rust crate tempfile to v3.14.0
- deps: Update Rust crate hyper to v1.5.1
- deps: Update Rust crate axum to 0.7
- deps: Update Rust crate thiserror to v2
- deps: Update Rust crate kube to 0.97
- deps: Update Rust crate kanidm_client to v1.4.3
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.24
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.18.0
- deps: Update pre-commit hook adrienverge/yamllint to v1.35.1
- deps: Update pre-commit hook pre-commit/pre-commit-hooks to v4.6.0
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.26.3
- deps: Update pre-commit hook pre-commit/pre-commit-hooks to v5
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.27.0
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.28.0
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.29.0
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.30.0
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.31.2
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.31.3
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.32.0
- deps: Update Rust crate tracing to v0.1.41
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.19.0
- deps: Update Rust crate tracing-subscriber to v0.3.19
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.42.4
- deps: Update opentelemetry-rust monorepo to v0.27.1
- deps: Update Rust crate hostname to 0.4
- deps: Update Rust crate thiserror to v2.0.4
- deps: Update Rust crate tokio to v1.42.0
- deps: Update Rust crate time to v0.3.37
- deps: Update Rust crate anyhow to v1.0.94
- deps: Update Rust crate clap to v4.5.22
- deps: Update Rust crate http to v1.2.0
- deps: Update Rust crate tokio-util to v0.7.13
- deps: Update Rust crate clap to v4.5.23
- deps: Update Rust crate thiserror to v2.0.5
- deps: Update Rust crate thiserror to v2.0.6
- deps: Update Rust crate chrono to v0.4.39
- deps: Update Rust crate serde to v1.0.216
- deps: Update Rust crate tower to v0.5.2
- deps: Update Rust crate thiserror to v2.0.7
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.25
- deps: Update Rust crate hyper to v1.5.2
- deps: Update Rust crate thiserror to v2.0.8
- deps: Update helm/kind-action action to v1.11.0
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.20.0
- deps: Update wagoid/commitlint-github-action action to v6.2.0
- deps: Update Rust crate kanidm_client to v1.4.5
- deps: Update Rust crate thiserror to v2.0.9
- deps: Update Rust crate serde_json to v1.0.134
- deps: Update Rust crate anyhow to v1.0.95
- deps: Update helm/kind-action action to v1.12.0
- deps: Update Rust crate kube to 0.98...
- deps: Update Rust crate serde to v1.0.217
- deps: Update Rust crate serde_json to v1.0.135
- deps: Update Rust crate clap to v4.5.24
- deps: Update Rust crate tokio to v1.43.0
- deps: Update Rust crate thiserror to v2.0.10
- deps: Update Rust crate tempfile to v3.15.0
- deps: Update Rust crate prometheus-client to 0.23.0
- deps: Update Rust crate clap to v4.5.28
- deps: Update Rust crate serde_json to v1.0.138
- deps: Update Rust crate thiserror to v2.0.11
- deps: Update wagoid/commitlint-github-action action to v6.2.1
- deps: Update Rust crate openssl to v0.10.70
- deps: Update Rust crate hyper to v1.6.0
- deps: Update Rust crate testcontainers to v0.23.2
- deps: Update Rust crate tempfile to v3.16.0
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.164.1
- deps: Update clechasseur/rs-clippy-check action to v4
- deps: Update Rust crate prometheus-client to v0.23.1
- deps: Update Rust crate axum to 0.8
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.1
- deps: Update Rust crate kanidm_client to v1.5.0
- deps: Update Rust crate time to v0.3.37
- deps: Update Rust crate clap to v4.5.29
- deps: Update opentelemetry-rust monorepo to 0.28
- deps: Update Rust crate tracing-opentelemetry to 0.29
- deps: Update Rust crate tempfile to v3.17.1
- deps: Update Rust crate clap to v4.5.30
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.21.0
- deps: Update Rust crate openssl to v0.10.71
- deps: Update azure/setup-helm action to v4.3.0
- deps: Update Rust crate backon to v1.4.0
- deps: Update Rust crate serde to v1.0.218
- deps: Update Rust crate anyhow to v1.0.96
- deps: Update Rust crate serde_json to v1.0.139
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.26
- deps: Update Rust crate clap to v4.5.31
- deps: Update Rust crate schemars to v0.8.22
- deps: Update Rust crate chrono to v0.4.40
- deps: Update Rust crate testcontainers to v0.23.3
- deps: Update Rust crate json-patch to v4
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.182.3
- deps: Update Rust crate anyhow to v1.0.97
- deps: Update Rust crate thiserror to v2.0.12
- deps: Update Rust crate tokio to v1.44.0
- deps: Update Rust crate tempfile to v3.18.0
- deps: Update Rust crate time to v0.3.39
- deps: Update Rust crate serde_json to v1.0.140
- deps: Update Rust crate serde to v1.0.219
- deps: Update Rust crate clap to v4.5.32
- deps: Update Rust crate tokio to v1.44.1
- deps: Update Rust crate tempfile to v3.19.0
- deps: Update Rust crate tokio-util to v0.7.14
- deps: Update Rust crate kube to 0.99
- deps: Update Rust crate http to v1.3.1
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.22.0
- deps: Update pre-commit hook adrienverge/yamllint to v1.36.0
- deps: Update pre-commit hook adrienverge/yamllint to v1.36.1
- deps: Update pre-commit hook adrienverge/yamllint to v1.36.2
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.2
- deps: Update Rust crate backon to v1.4.1
- deps: Update Rust crate tempfile to v3.19.1
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.3
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.28
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.4
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.5
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.6
- deps: Update Rust crate time to v0.3.41
- deps: Update pre-commit hook adrienverge/yamllint to v1.37.0
- deps: Update Rust crate tracing-opentelemetry to 0.30
- deps: Update opentelemetry-rust monorepo to 0.29
- deps: Update Rust crate clap to v4.5.33
- deps: Update Rust crate tonic to 0.13
- deps: Update Rust crate clap to v4.5.34
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.7
- deps: Update Rust crate hyper-util to v0.1.11
- deps: Update Rust crate clap to v4.5.35
- deps: Update Rust crate axum to v0.8.3
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.227.2
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.9
- deps: Update Rust crate opentelemetry to v0.29.1
- deps: Update Kubernetes version to v1.32.3
- deps: Update Rust crate openssl to v0.10.72
- deps: Update Rust crate tokio to v1.44.2
- deps: Update Rust crate backon to v1.5.0
- deps: Update Rust crate clap to v4.5.36
- deps: Update Rust crate anyhow to v1.0.98
- deps: Update Rust crate clap to v4.5.37
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.29
- deps: Update Rust crate tokio-util to v0.7.15
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.10
- deps: Update Rust crate chrono to v0.4.41
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.11
- deps: Update Rust crate axum to v0.8.4
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v40
- deps: Update Rust crate tonic to v0.13.1
- deps: Update pre-commit hook adrienverge/yamllint to v1.37.1
- deps: Update Rust crate tokio to v1.45.0
- deps: Update Rust crate testcontainers to 0.24
- deps: Update Kanidm to 1.6.2
- deps: Update Rust crate clap to v4.5.38
- deps: Update Rust crate tempfile to v3.20.0
- deps: Update Rust crate kanidm_client to v1.6.3
- deps: Update Rust crate kube to v1
- deps: Update Rust crate k8s-openapi to v0.25
- deps: Update Rust crate hyper-util to v0.1.12
- deps: Update dependency kubernetes-sigs/kind to v0.29.0
- deps: Update Rust crate tokio to v1.45.1
- deps: Update Rust crate kube to v1.1.0
- deps: Update Rust crate clap to v4.5.39
- deps: Update Rust crate hyper-util to v0.1.13
- deps: Update Rust crate openssl to v0.10.73
- deps: Update Rust crate backon to v1.5.1
- deps: Update Rust crate hyper-util to v0.1.14
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v40.48.3
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.12
- deps: Update Rust crate tracing-opentelemetry to 0.31
- deps: Update Rust crate opentelemetry to 0.30
- deps: Update Rust crate clap to v4.5.40
- deps: Update Rust crate kanidm_client to v1.6.4
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.14.2
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.15.0
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.16.0
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.16.1
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v40.62.1
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v41
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.30
- deps: Update Rust crate clap to v4.5.41
- deps: Update Rust crate serde_json to v1.0.141
- deps: Update Rust crate hyper-util to v0.1.16
- deps: Update Rust crate tokio to v1.46.1
- deps: Update appany/helm-oci-chart-releaser action to v0.5.0
- deps: Update Rust crate testcontainers to 0.25
- deps: Update Rust crate tokio to v1.47.0
- deps: Update Rust crate clap to v4.5.42
- deps: Update Rust crate backon to v1.5.2
- deps: Update Rust crate serde_json to v1.0.142
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v41.43.0
- deps: Update Rust crate tokio to v1.47.1
- deps: Update Rust crate clap to v4.5.43
- deps: Update Rust crate clap to v4.5.44
- deps: Update Rust crate thiserror to v2.0.13
- deps: Update actions/checkout action to v5
- deps: Update pre-commit hook pre-commit/pre-commit-hooks to v6
- deps: Update Rust crate tokio-util to v0.7.16
- deps: Update Rust crate anyhow to v1.0.99
- deps: Update Rust crate thiserror to v2.0.14
- deps: Update Rust crate clap to v4.5.45
- deps: Update Rust crate url to v2.5.6
- deps: Update azure/setup-helm action to v4.3.1
- deps: Update Rust crate serde_json to v1.0.143
- deps: Update Rust crate thiserror to v2.0.16
- deps: Update Rust crate url to v2.5.7
- deps: Update Rust crate clap to v4.5.46
- deps: Update Rust crate clap to v4.5.47
- deps: Update Rust crate time to v0.3.43
- deps: Update actions/setup-python action to v6
- deps: Update clechasseur/rs-clippy-check action to v5
- deps: Update Rust crate tempfile to v3.22.0
- deps: Update Rust crate chrono to v0.4.42
- deps: Update Rust crate tracing-subscriber to v0.3.20
- deps: Update dependency kubernetes-sigs/kind to v0.30.0
- deps: Update Rust crate kube to v2
- deps: Update Rust crate hyper to v1.7.0
- deps: Update Rust crate prometheus-client to 0.24.0
- deps: Update Rust crate tonic to 0.14
- deps: Update Rust crate kube to v2.0.1
- deps: Update Rust crate serde_json to v1.0.144
- deps: Update Rust crate serde to v1.0.221
- deps: Update Rust crate serde_json to v1.0.145
- deps: Update Rust crate serde to v1.0.223
- deps: Update Rust crate serde to v1.0.224
- deps: Update Rust crate hyper-util to v0.1.17
- deps: Update Rust crate json-patch to v4.1.0
- deps: Update Rust crate serde to v1.0.225
- deps: Update Rust crate anyhow to v1.0.100
- deps: Update Rust crate clap to v4.5.48
- deps: Update Rust crate serde to v1.0.226
- deps: Update Rust crate time to v0.3.44
- deps: Update Rust crate tempfile to v3.23.0
- Add multi-arch docker build and releases
- Change release `--frozen` by `--locked`
- Add permissions for package write
- Optimize release binary
- Reduce to minimum dependencies
- Improv ecompile time
- Remove deprecated NOTPARALLEL instruction
- Add openssl vendored to workaround kanidm cross compilation
- Update rust to 1.85.0 and rust edition to 2024
- Update cargo lock

### Refactor

- ci: Migrate config renovate.json5
- ci: Migrate config renovate.json5
- ci: Migrate config renovate.json5
- cmd: Replace actix with axum
- deps: Upgrade schemars to v1.0
- deps: Move validations from admission policy to schemars
- kanidm: Move status to a different file
- kanidm: Change from deployment to statefulset
- kanidm: Simplify controller watchers and stores code
- kanidm: Break down statefulset creation into smaller functions
- kanidm: Reduce exposure in SecretExt trait
- operator: Add generic trait for patch and delete
- Change structure for libs and cmd
- Add telemetry, axum and new dir structures
- Move echoes controller to echo mod
- Remove diagnostics
- Echo docs and minor code changes
- Use kube-rs finalizers for handling reconcile events
- Replace `match` with `ok_or_else` and add explicit rustfmt config
- Add features to workspace and integration-tests package
- Add feature for integration tests and add e2e tests to makefile
- Simplify e2e targets in Makefile
- Rename tests to test
- Rename echo resource to kanidm
- Format json definitions
- Use relative imports and split oauth2 reconcile
- Move kanidm to its own module inside operator
- Remove namespace parameter from reconciles
- Make e2e-tests configurable for any kubernetes version

### Testing

- ci: Disable integration test for arm64
- ci: Limit e2e concurrence to 4
- ci: Add pre-commit workflow and deprecate commitlint
- group: Fix `group_lifecycle` race condition on Posix attributes
- kanidm: Ensure replication is correctly configured in e2e checks
- kanidm: Fix naming resolution for external Kanidm pods
- kanidm: Change wait for replication time var in `kanidm_external_replication_node`
- Add unittests for Helm charts
- Add reconcile unittests
- Increase timeout to 30s
- Remove integration-tests
- Force `image.tag` in e2e to be a string
- Clean metadata fields before applying patch
- Change backoff by backon crate in e2e
- Remove person objects in clean-e2e removing finalizer
- Get Kanidm version from `Cargo.lock` instead of `Cargo.toml`
- Add wait to event list in `person_attributes_collision`
- Ensure events are waited with `check_event_with_timeout`
- Show kaniop logs when e2e tests fail
- Add debug commands to e2e tests
- Upgrade kind to 0.27.0
- Fast fail e2e if kaniop does not start
- Ignore examples dir from yamllint
- Show container logs when e2e pod fails to start

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
