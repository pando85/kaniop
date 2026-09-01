---
title: Introduction
weight: 0
---

# Introduction

Kaniop is a Kubernetes operator for managing [Kanidm](https://kanidm.com) clusters. By leveraging
GitOps, it provides a declarative way to manage identity resources—such as persons, groups, OAuth2
integrations, and more—through familiar Kubernetes manifests.

Kaniop treats Kubernetes as the control plane for deployment and lifecycle orchestration. The
operator continuously reconciles declared topology, configuration, workloads, storage, and identity
resources while Kanidm remains responsible for identity, database, and replication semantics.

This separation allows teams to manage identity infrastructure with the same reviewed, versioned,
and observable workflows they use for other Kubernetes workloads without asking Kubernetes state
to stand in for Kanidm's own correctness guarantees.

## What is Kanidm?

Kanidm is a secure identity management platform designed to act as a complete identity provider. It
covers a broad spectrum of authentication and directory requirements, allowing applications and
systems to offload identity and authorization concerns to a dedicated service.

Kaniop manages the Kubernetes lifecycle around Kanidm; it does not replace Kanidm's responsibility
for the safety and correctness of its persisted and replicated identity state.

## Why Kaniop?

Operating a stateful identity service requires both generic infrastructure orchestration and
application-specific knowledge. Kubernetes already provides a mature desired-state and
reconciliation model for workloads, storage, placement, health signals, and extensible APIs. Kaniop
adds the Kanidm-specific policy required to use those facilities safely.

Kaniop therefore favors explicit reconciliation over imperative runbooks and explicit server
semantics over heuristics. Kubernetes readiness, rollout state, timestamps, and logs remain useful
operational signals, but workflows that depend on database or replication safety must use the best
available Kanidm-specific evidence and fall back conservatively when that evidence is unavailable.

See [Architecture](architecture.md) for the responsibility boundary and design principles that guide
new operational features.

## LLM and Automation Entry Point

For LLM agents and automation that need a source-oriented operations map, use the published
[`llm.txt`](https://pando85.github.io/llm.txt). It points to generated CRD schemas, generated
examples, Helm values, and troubleshooting workflows so agents can prefer current authoritative
sources over stale copied snippets.
