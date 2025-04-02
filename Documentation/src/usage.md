---
title: Usage
weight: 030000
---

# Usage

This section provides detailed guidance on managing identity resources with Kaniop. Learn how to
configure persons, groups, and OAuth2 clients for your organization's needs.

## Overview

Kaniop manages identity resources through Kubernetes Custom Resources, enabling you to:

- **Declaratively manage** user accounts, groups, and OAuth2 clients
- **Version control** your identity infrastructure alongside your applications
- **Automate** identity provisioning through GitOps workflows
- **Scale** identity management across multiple environments

## Resource Management

Each identity resource type has specific use cases and configuration options:

### [OAuth2 Integration](usage/oauth2.md)

Set up OAuth2/OpenID Connect clients for application authentication, including scope mappings and
custom claims.

### [Managing Persons](usage/person.md)

Learn how to create and manage individual user accounts with attributes like display names, email
addresses, and POSIX settings.

### [Managing Groups](usage/group.md)

Organize users into groups for authorization and policy management. Configure group memberships and
email aliases.
