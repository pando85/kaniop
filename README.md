<img src="https://raw.githubusercontent.com/pando85/kaniop/master/artwork/logo.png" width="20%" height="auto" />

[![GitHub release](https://img.shields.io/github/release/pando85/kaniop/all.svg)](https://github.com/pando85/kaniop/releases)
[![Container](https://img.shields.io/badge/ghcr.io-pando85/kaniop-blue?logo=github)](https://github.com/pando85/kaniop/pkgs/container/kaniop)

# What is Kaniop?

Kaniop is a Kubernetes operator for managing [Kanidm](https://kanidm.com).

[Kanidm](https://kanidm.com) is a modern, secure identity management system that provides
authentication and authorization services with support for POSIX accounts, OAuth2, and more.

Kaniop treats Kubernetes as the orchestration control plane for Kanidm deployments. It continuously
reconciles desired cluster topology, workload lifecycle, storage, configuration, upgrades, and
identity resources through standard Kubernetes APIs, while Kanidm remains responsible for identity,
persistence, and replication semantics.

Kaniop automates deployment and management of Kanidm clusters to provide declaratively managed,
automatically reconciled, and observable identity services. The operator builds on Kubernetes
resources to deploy, configure, provision, scale, upgrade, and monitor Kanidm clusters.

When an operation depends on database or replication correctness, Kaniop prefers explicit,
machine-readable server state and conservative workflows. Pod readiness, StatefulSet rollout state,
log messages, and timestamps are useful orchestration signals, but they are not treated as proofs of
data safety.

The operator enables **declarative identity management** through GitOps workflows, allowing you to
manage users, groups, OAuth2 clients, and other identity resources using familiar Kubernetes
manifests.

Key capabilities include:

- **Kanidm Cluster Management**: Deploy and manage Kanidm clusters, including high-availability
  topologies and replication configuration
- **Identity Resources**: Declaratively manage persons, groups, OAuth2 clients, and service accounts
- **GitOps Ready**: Full integration with Git-based workflows for infrastructure-as-code
- **Kubernetes Native**: Built using Custom Resources and standard Kubernetes reconciliation
  patterns
- **Operational Safety**: Conservative stateful workflows with testing, monitoring, and
  observability support

For the design principles behind this separation of responsibilities, see the
[Architecture](Documentation/src/architecture.md) documentation.

## Getting Started and Documentation

For installation, deployment, and administration, see our
[Documentation](https://pando85.github.io/) and
[Quickstart Guide](https://pando85.github.io/docs/kaniop/latest/quickstart.html).

## Contributing

We welcome contributions. See [Contributing](Documentation/src/contributing.md) to get started.

## Report a Bug

For filing bugs, suggesting improvements, or requesting new features, please open an
[issue](https://github.com/pando85/kaniop/issues).

## Contact

Please use the following to reach members of the community:

- GitHub: Start a [discussion](https://github.com/pando85/kaniop/discussions) or open an
  [issue](https://github.com/pando85/kaniop/issues)
- Documentation: [pando85.github.io](https://pando85.github.io/)

## Official Releases

Official releases of Kaniop can be found on the
[releases page](https://github.com/pando85/kaniop/releases). Please note that it is **strongly
recommended** that you use [official releases](https://github.com/pando85/kaniop/releases) of
Kaniop, as unreleased versions from the master branch are subject to changes and incompatibilities
that will not be supported in the official releases. Builds from the master branch can have
functionality changed and even removed at any time without compatibility support and without prior
notice.
