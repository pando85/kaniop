---
title: Usage
weight: 030000
---

# Usage

This section provides detailed guidance on managing identity resources with Kaniop. Learn how to
configure Kanidm clusters, persons, groups, and OAuth2 clients for your organization's needs.

## Overview

Kaniop manages identity resources through Kubernetes Custom Resources, enabling you to:

- **Declaratively manage** Kanidm clusters, user accounts, groups, and OAuth2 clients
- **Version control** your identity infrastructure alongside your applications
- **Automate** identity provisioning through GitOps workflows
- **Scale** identity management across multiple environments

## Resource Management

Each identity resource type has specific use cases and configuration options:

### Managing Kanidm Clusters

Deploy and operate Kanidm identity management clusters on Kubernetes using the `Kanidm` Custom
Resource Definition (CRD). Kaniop provides a cloud-native abstraction for managing the complete
Kanidm lifecycle—from initial deployment through scaling and certificate rotation.

#### Key Capabilities

- **Declarative deployment**: Define cluster topology in YAML; Kaniop handles StatefulSets,
  Services, and certificates.
- **High availability**: Multi-replica configurations with different groups of replica roles and
  topologies.
- **Zero-touch certificate management**: Automated generation, distribution, and renewal of
  replication TLS certificates.
- **External node federation**: Seamlessly integrate external Kanidm instances into your Kaniop
  cluster.

#### Basic Kanidm Cluster

Here's a simple Kanidm cluster configuration. You can reference the complete example at
[`examples/kanidm.yaml`](https://github.com/pando85/kaniop/blob/master/examples/kanidm.yaml):

### [OAuth2 Integration](usage/oauth2.md)

Set up OAuth2/OpenID Connect clients for application authentication, including scope mappings and
custom claims.

### [Managing Persons](usage/person.md)

Learn how to create and manage individual user accounts with attributes like display names, email
addresses, and POSIX settings.

### [Managing Groups](usage/group.md)

Organize users into groups for authorization and policy management. Configure group memberships and
email aliases.
