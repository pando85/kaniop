---
title: LLM Operations Guide
weight: 90
---

# LLM Operations Guide

Kaniop publishes a machine-oriented operations guide for LLMs at:

```text
https://pando85.github.io/llm.txt
```

Use that file as the stable entrypoint for automated agents. It explains the operation order, source precedence, common commands, troubleshooting, and known field-name pitfalls.

The guide intentionally points agents to generated or schema-backed sources instead of duplicating every CRD field:

- `charts/kaniop/crds/crds.yaml` for exact CRD schema and validation.
- `examples/*.yaml` for generated manifest examples.
- `charts/kaniop/values.yaml` and `charts/kaniop/values.schema.json` for Helm configuration.
- `kubectl explain` for the schema installed in a target cluster.

If this page, `llm.txt`, generated examples, and CRD schema disagree, prefer the installed CRDs or generated CRD schema for exact fields and validation.
