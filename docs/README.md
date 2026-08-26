# Kaniop Internal Documentation

This directory contains maintainer and contributor documentation that is not
part of the public mdBook under `Documentation/src/`.

## Directory structure

- [`adr/`](adr/): architecture decision records describing what Kaniop decided
  and why.
- [`plans/`](plans/): detailed implementation plans describing how approved or
  proposed work should be executed.

Public user documentation, installation instructions, usage guides and
troubleshooting remain under `Documentation/src/` and are published by mdBook.
Internal ADRs and plans are reviewed in the repository but are not published as
product documentation.

## Conventions

- ADR files use `NNNN-kebab-case-title.md` and are indexed in
  `docs/adr/README.md`.
- Plan files use `kebab-case-title.md` and link to their associated ADR when one
  exists.
- ADRs capture decisions and consequences, not a file-by-file patch sequence.
- Plans contain phases, files, dependencies, verification and completion
  criteria.
- Superseded ADRs remain in the repository with their status updated and a link
  to the replacing ADR.
