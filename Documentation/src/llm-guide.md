---
title: LLM Operations Guide
weight: 90
---

# LLM Operations Guide

Kaniop publishes a compact machine-oriented documentation index at:

```text
https://pando85.github.io/docs/kaniop/latest/llms.txt
```

Use `llms.txt` as the stable discovery entrypoint for automated agents. It stays intentionally
small and links to the Markdown documentation, generated schemas, generated examples, Helm
configuration, repository instructions, and the detailed operations guide.

For deeper operational guidance, source precedence, common commands, troubleshooting, and known
field-name pitfalls, follow the linked `llms-full.txt` in the same versioned documentation
directory.

Each rendered documentation page advertises:

- `rel="describedby"` pointing to the `llms.txt` file that covers the page.
- `rel="alternate" type="text/markdown"` pointing to the page's Markdown representation.

The documentation index intentionally points agents to generated or schema-backed sources instead of
duplicating every CRD field:

- `charts/kaniop/crds/crds.yaml` for exact CRD schema and validation.
- `examples/*.yaml` for generated manifest examples.
- `charts/kaniop/values.yaml` and `charts/kaniop/values.schema.json` for Helm configuration.
- `kubectl explain` for the schema installed in a target cluster.

If narrative documentation, generated examples, and CRD schema disagree, prefer the installed CRDs
or generated CRD schema for exact fields and validation.
