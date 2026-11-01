# Durable change artifacts

Use one directory per non-trivial or high-risk work item:

```text
intent/<issue-or-work-id>/
├── intent.md
├── spec.md
└── plan.md
```

Copy the files from `intent/_template/`. The issue remains the collaboration
record; these files are the repository-local, versioned decisions that coding
agents and reviewers consume.

## Gates

| Artifact | Answers | Human gate |
|---|---|---|
| `intent.md` | What outcome is wanted and why? | Product/issue owner accepts intent |
| `spec.md` | What behavior and design satisfy it? | Product and policy owners accept design |
| `plan.md` | How will this repository implement and prove it? | Engineer or technical owner accepts plan |

A trivial documentation or dependency change does not need three files. Use the
full chain when ambiguity, cross-component impact, API compatibility, migration,
security, data safety, or difficult rollback makes the decisions worth keeping.

Update an artifact when the corresponding decision changes. Do not preserve a
known-false plan merely to make the history look linear.
