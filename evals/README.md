# Agent-system evaluations

These evals test the complete coding system: model, harness, `AGENTS.md`,
skills, subagents, permissions, tools, and review policy. Product tests continue
to test Kaniop itself.

## Corpus

Build the corpus from real accepted work. Add a case when:

- a production or CI defect escapes;
- a reviewer makes a material correction;
- an agent repeatedly misunderstands a repository rule;
- a model, skill, or harness change needs representative coverage.

Each case contains an immutable task prompt plus machine-checkable commands and
human-review criteria. Prefer tests and structural assertions over an LLM judge.

## Configuration change workflow

1. State the hypothesis in the PR changing agent configuration.
2. Run the same cases against the merge-base configuration and the candidate.
3. Use the same repository snapshot, task, tools, budget, and model.
4. Repeat stochastic cases when one sample is not reliable.
5. Publish a result matching `schema/result.schema.json`.
6. Investigate candidate regressions before merging.
7. Start with a small canary of eligible real tasks when offline results improve.

Do not commit complete transcripts or chain of thought. Keep prompts, observable
outputs, test evidence, review findings, timing, and cost. A Git commit identifies
the repository-local configuration used for a run.

The initial repository framework validates case structure but deliberately does
not call a model. Add headless execution only after selecting the production
harness, credential boundary, budget, and initial historical corpus.
