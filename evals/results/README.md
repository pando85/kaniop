# Evaluation results

Commit only baseline-versus-candidate evidence used to review a configuration
change. Store one result per case so runs remain diffable and failed runs do not
destroy completed evidence. Use a unique directory such as:

```text
evals/results/2026-09-02-<candidate-commit>/
├── baseline/
│   └── <case-id>.json
├── candidate/
│   └── <case-id>.json
└── summary.md
```

Copy `_template/result.json` for each result. Generate the scorecard with:

```bash
make agent-eval-summary \
  BASELINE=evals/results/<run>/baseline \
  CANDIDATE=evals/results/<run>/candidate
```

Do not commit raw transcripts, temporary worktrees, build output, or duplicate
Git diffs.
