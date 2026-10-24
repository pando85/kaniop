# Architecture Decision Records

This directory contains internal Architecture Decision Records for Kaniop.

An ADR records an important technical decision, its context and its
consequences. Public user guidance belongs under `Documentation/src/`, not here.

## ADR index

| Number | Title | Status |
|---|---|---|
| [0001](0001-production-kanidm-backup-and-restore.md) | Production Kanidm backup and restore orchestration | Proposed |
| [0002](0002-rolling-kanidm-database-maintenance.md) | Rolling Kanidm database maintenance with init containers | Proposed |

## File naming

Use a zero-padded sequence followed by a kebab-case title:

```text
docs/adr/0002-example-decision.md
```

Do not reuse an ADR number. When a decision is replaced, keep the original file,
mark it `Superseded`, and link both records.

## Status lifecycle

- `Proposed`: under review and not yet approved.
- `Accepted`: approved as the architecture to implement or preserve.
- `Deprecated`: retained for history but no longer recommended.
- `Superseded`: replaced by another ADR.

## Template

```markdown
# ADR-NNNN: Title

## Status

Proposed

## Date

YYYY-MM-DD

## References

- Related issue or plan

## Context

What problem or constraint motivates the decision?

## Decision

What is being decided and what invariants must implementations preserve?

## Consequences

### Benefits

### Costs

### Risks and mitigations

## Rejected alternatives
```
