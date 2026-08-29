# Project Analysis

This section records how Kaniop evolves as a project, not only as a codebase. It tracks technical capability, Kanidm compatibility, release engineering, testing maturity, community participation, production-derived knowledge, and AI-assisted development.

The analysis is intentionally split into two layers:

- **[Current report](current.md)**: a living view of the project. Update it when material facts change.
- **[Historical snapshots](history/index.md)**: immutable, dated reports that preserve what was known and how the project was assessed at a specific point in time.

The [methodology](methodology.md) defines how metrics are collected and interpreted so reports remain comparable over time.

## Why both a living report and snapshots?

A single living document is useful for answering "where is Kaniop now?", but it destroys historical context when numbers, dependencies, support claims, and maturity assessments change. Dated snapshots solve that problem.

Conversely, snapshots alone make it difficult to answer a current question without reconstructing the project from many historical documents. The living report provides that current entry point.

The intended workflow is therefore:

1. Update `current.md` from primary sources.
2. When a meaningful milestone is reached, copy the current state into a new dated snapshot.
3. Never silently rewrite an old snapshot to match the present. Correct factual mistakes explicitly and record the correction.

Good snapshot triggers include a new operator capability level, a new Kanidm minor-version transition, a major migration or disaster-recovery milestone, a substantial change in community composition, or roughly six months since the previous snapshot.

## Scope

The reports should answer questions such as:

- Which Kanidm versions did Kaniop actually build and test against?
- How quickly does Kaniop adopt new Kanidm releases?
- Which Operator Framework capability levels are implemented and which are production-qualified?
- How did the workspace, controllers, CRDs, CI, and e2e coverage grow?
- How much development is maintainer-authored, externally authored, dependency automation, or AI/agent implementation?
- How much product knowledge comes from external users through issues, reproductions, metrics, logs, and production experience?
- How fast are production reports converted into fixes or capabilities?
- How are stars, forks, and active contributors evolving?
- What are the important maturity gaps and risks?

The goal is not to maximize flattering metrics. The goal is to make Kaniop's development history reproducible and useful for technical and project decisions.
