# Logging conventions

Kaniop uses `tracing` for application logs. Log events are part of the operator's observability API: they are consumed by humans as well as log aggregation systems, so their shape must remain predictable.

## Output formats

Kaniop supports `text` and `json` log formats.

### JSON

JSON logs **must use newline-delimited JSON (NDJSON)**:

- one tracing event is serialized as exactly one JSON object;
- one event occupies exactly one physical output line;
- pretty-printed or multiline JSON is not permitted;
- tracing metadata such as `timestamp`, `level`, and `target` remains at the top level;
- application event fields remain nested under `fields`.

Example:

```json
{"timestamp":"2026-08-27T17:55:44.468890Z","level":"INFO","fields":{"message":"starting controller loop","controller":"backup-discovery","interval_secs":300},"target":"kaniop_backup::controller::discovery"}
```

Keeping event fields nested avoids collisions between application fields and tracing metadata.

### Text

Text logs use the compact human-readable formatter and are intended primarily for interactive use.

## Event authoring

The central rule is:

> Messages describe what happened; fields describe what it happened to.

Use the trailing tracing message syntax for the human-readable event message:

```rust
info!(
    controller = CONTROLLER_ID,
    interval_secs = interval.as_secs(),
    "starting controller loop"
);
```

Do not create `msg` or explicit `message` fields:

```rust
// Do not do this.
info!(msg = format!("starting {CONTROLLER_ID} controller"));
info!(message = "starting controller");
```

`tracing` creates the canonical `message` field from the trailing message automatically. This keeps JSON output consistent across Kaniop and upstream libraries.

## Structured fields

- Field names use `snake_case`.
- Prefer stable, semantic names such as `controller`, `namespace`, `kanidm`, `repository`, `schedule`, and `job`.
- Put dynamic identifiers and values in fields instead of interpolating them into the message when practical.
- Errors use the `error` field, normally `error = %error` or `error = %err`.
- Never log credentials, access tokens, private keys, secret values, or other sensitive data.

Prefer:

```rust
warn!(
    controller = CONTROLLER_ID,
    namespace,
    schedule = schedule.name_any(),
    error = %error,
    "discovery processing failed for schedule"
);
```

Instead of:

```rust
warn!("discovery processing failed for schedule {namespace}/{}: {error}", schedule.name_any());
```

## Levels

- `ERROR`: an operation failed and needs investigation or the current operation cannot continue.
- `WARN`: recoverable abnormal state, retry, degraded behavior, or an intentional skip that operators should notice.
- `INFO`: lifecycle events and meaningful state transitions.
- `DEBUG`: routine reconciliation details and diagnostic context.
- `TRACE`: very high-volume implementation detail.

## Enforcement

`.ci/check-logging.py` rejects `msg = ...` and explicit `message = ...` fields inside `trace!`, `debug!`, `info!`, `warn!`, and `error!` invocations. The check runs through pre-commit and therefore in CI.

The telemetry unit tests additionally enforce the runtime JSON contract: every emitted JSON event must be independently parseable from a single output line and use the canonical `fields.message` field.
