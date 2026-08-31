# Implementation Plans

This directory contains internal, actionable implementation plans for Kaniop.

## Relationship to ADRs

| Directory | Purpose | Answers |
|---|---|---|
| `docs/adr/` | Architecture Decision Records | What did we decide and why? |
| `docs/plans/` | Implementation Plans | How do we execute the work? |

Not every plan requires an ADR. When a plan implements an ADR, link it near the
start of the plan.

## Current plans

- [Production Kanidm backup and restore](production-kanidm-backup-and-restore.md)
- [Argo CD migration E2E CI](argocd-migration-e2e-ci.md)
- [Rolling Kanidm database maintenance](maintenance-operations-design.md)

## File naming

Use an unnumbered kebab-case description:

```text
docs/plans/example-implementation.md
```

## Template

```markdown
# Title

## Goal

One-paragraph summary of the outcome.

## Context

Links to ADRs, issues and prior work.

## Current state

What exists and what is missing?

## Phase N: Title

### Problem

### Approach

### Files

### Verify

## Sequencing and dependencies

## Effort and impact summary

## Completion criteria
```
