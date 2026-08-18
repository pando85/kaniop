from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        raise RuntimeError(f"missing observability anchor: {label}")
    return text.replace(old, new, 1)


path = Path("libs/operator/src/kanidm/restore.rs")
text = path.read_text()

text = replace_once(
    text,
    '''use futures::StreamExt;
use k8s_openapi::api::apps::v1::StatefulSet;
''',
    '''use futures::StreamExt;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
''',
    "metrics and condition imports",
)
text = replace_once(
    text,
    '''use kube::runtime::controller::{Action, Controller};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use kube::runtime::watcher;
''',
    '''use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use kube::runtime::watcher;
''',
    "event imports",
)

text = replace_once(
    text,
    '''const REQUEUE: Duration = Duration::from_secs(2);
''',
    '''const REQUEUE: Duration = Duration::from_secs(2);
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";
const CONDITION_PROGRESSING: &str = "Progressing";
const CONDITION_READY: &str = "Ready";
const CONDITION_FAILED: &str = "Failed";
''',
    "condition constants",
)

text = replace_once(
    text,
    '''    #[serde(default)]
    pub database_mutation_started: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
''',
    '''    #[serde(default)]
    pub database_mutation_started: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
''',
    "restore conditions status field",
)

text = replace_once(
    text,
    '''#[derive(Clone)]
struct RestoreContext {
    client: Client,
}

pub async fn run(client: Client) {
    let api = Api::<KanidmRestore>::all(client.clone());
    let ctx = Arc::new(RestoreContext { client });
''',
    '''#[derive(Clone)]
struct RestoreMetrics {
    attempts: Counter<u64>,
    outcomes: Counter<u64>,
    duration_seconds: Histogram<f64>,
}

impl RestoreMetrics {
    fn new() -> Self {
        let meter = global::meter("kaniop");
        Self {
            attempts: meter
                .u64_counter("kanidm_restore_attempts")
                .with_description("Number of Kanidm restore attempts started")
                .build(),
            outcomes: meter
                .u64_counter("kanidm_restore_outcomes")
                .with_description("Number of terminal Kanidm restore outcomes")
                .build(),
            duration_seconds: meter
                .f64_histogram("kanidm_restore_duration_seconds")
                .with_description("Kanidm restore duration from object creation to terminal phase")
                .with_unit("s")
                .build(),
        }
    }
}

#[derive(Clone)]
struct RestoreContext {
    client: Client,
    recorder: Recorder,
    metrics: RestoreMetrics,
}

pub async fn run(client: Client) {
    let api = Api::<KanidmRestore>::all(client.clone());
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: CONTROLLER_ID.into(),
            instance: None,
        },
    );
    let ctx = Arc::new(RestoreContext {
        client,
        recorder,
        metrics: RestoreMetrics::new(),
    });
''',
    "restore context observability",
)

old_patch = '''async fn patch_status(restore: &KanidmRestore, ctx: &RestoreContext, mut status: KanidmRestoreStatus) -> Result<()> {
    let ns = restore.namespace().unwrap();
    status.observed_generation = restore.metadata.generation;
    Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .patch_status(
            &restore.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"status": status})),
        )
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("patch status", "KanidmRestore", &ns, restore.name_any(), e))
}
'''
new_patch = '''async fn patch_status(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    mut status: KanidmRestoreStatus,
) -> Result<()> {
    let ns = restore.namespace().unwrap();
    let previous_phase = restore.status.as_ref().map(|current| current.phase);
    let next_phase = status.phase;
    status.observed_generation = restore.metadata.generation;
    update_restore_conditions(&mut status, restore.metadata.generation);

    Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .patch_status(
            &restore.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"status": &status})),
        )
        .await
        .map_err(|e| {
            Error::kube_error("patch status", "KanidmRestore", &ns, restore.name_any(), e)
        })?;

    if previous_phase != Some(next_phase) {
        record_restore_transition(restore, ctx, previous_phase, next_phase, status.message.as_deref())
            .await;
    }
    Ok(())
}

fn update_restore_conditions(status: &mut KanidmRestoreStatus, generation: Option<i64>) {
    let previous = status.conditions.clone();
    let phase = status.phase;
    let phase_reason = format!("{phase:?}");
    let message = status
        .message
        .clone()
        .unwrap_or_else(|| format!("Kanidm restore is in phase {phase_reason}."));
    let terminal_success = phase == KanidmRestorePhase::Completed;
    let terminal_failure = phase == KanidmRestorePhase::Failed;
    let progressing = !terminal_success && !terminal_failure;

    status.conditions = vec![
        restore_condition(
            &previous,
            CONDITION_PROGRESSING,
            if progressing { CONDITION_TRUE } else { CONDITION_FALSE },
            &phase_reason,
            &message,
            generation,
        ),
        restore_condition(
            &previous,
            CONDITION_READY,
            if terminal_success { CONDITION_TRUE } else { CONDITION_FALSE },
            if terminal_success { "RestoreCompleted" } else { "RestoreNotCompleted" },
            if terminal_success {
                "Kanidm restore completed successfully."
            } else {
                "Kanidm restore has not completed successfully."
            },
            generation,
        ),
        restore_condition(
            &previous,
            CONDITION_FAILED,
            if terminal_failure { CONDITION_TRUE } else { CONDITION_FALSE },
            if terminal_failure { "RestoreFailed" } else { "NoRestoreFailure" },
            if terminal_failure {
                status.message.as_deref().unwrap_or("Kanidm restore failed.")
            } else {
                "No terminal restore failure has been recorded."
            },
            generation,
        ),
    ];
}

fn restore_condition(
    previous: &[Condition],
    condition_type: &str,
    condition_status: &str,
    reason: &str,
    message: &str,
    generation: Option<i64>,
) -> Condition {
    let last_transition_time = previous
        .iter()
        .find(|condition| {
            condition.type_ == condition_type
                && condition.status == condition_status
                && condition.reason == reason
        })
        .map(|condition| condition.last_transition_time.clone())
        .unwrap_or_else(|| Time(Timestamp::now()));
    Condition {
        type_: condition_type.to_string(),
        status: condition_status.to_string(),
        reason: reason.to_string(),
        message: message.to_string(),
        last_transition_time,
        observed_generation: generation,
    }
}

async fn record_restore_transition(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    previous_phase: Option<KanidmRestorePhase>,
    phase: KanidmRestorePhase,
    message: Option<&str>,
) {
    if phase == KanidmRestorePhase::Validating {
        ctx.metrics.attempts.add(1, &[]);
    }

    let result = match phase {
        KanidmRestorePhase::Completed => Some("success"),
        KanidmRestorePhase::Failed => Some("failure"),
        _ => None,
    };
    if let Some(result) = result {
        let attributes = [KeyValue::new("result", result)];
        ctx.metrics.outcomes.add(1, &attributes);
        if let Some(created) = restore.metadata.creation_timestamp.as_ref() {
            let elapsed = (Timestamp::now().as_second() - created.0.as_second()).max(0) as f64;
            ctx.metrics.duration_seconds.record(elapsed, &attributes);
        }
    }

    let reason = if phase == KanidmRestorePhase::Failed {
        "RestoreFailed"
    } else {
        "RestorePhaseChanged"
    };
    let note = message.map(str::to_string).or_else(|| {
        Some(format!(
            "Kanidm restore phase changed from {} to {phase:?}.",
            previous_phase
                .map(|previous| format!("{previous:?}"))
                .unwrap_or_else(|| "None".to_string())
        ))
    });
    if let Err(error) = ctx
        .recorder
        .publish(
            &Event {
                type_: if phase == KanidmRestorePhase::Failed {
                    EventType::Warning
                } else {
                    EventType::Normal
                },
                reason: reason.to_string(),
                note,
                action: "Restore".to_string(),
                secondary: None,
            },
            &restore.object_ref(&()),
        )
        .await
    {
        warn!(restore = %restore.name_any(), %error, "failed to publish restore event");
    }
}
'''
text = replace_once(text, old_patch, new_patch, "central status observability")

# Extend the existing focused unit tests with terminal Condition coverage.
text = replace_once(
    text,
    '''mod tests {
    use super::{KanidmRestoreStatus, mutable_image, safe_basename};
''',
    '''mod tests {
    use super::{
        CONDITION_FAILED, CONDITION_READY, CONDITION_TRUE, KanidmRestorePhase,
        KanidmRestoreStatus, mutable_image, safe_basename, update_restore_conditions,
    };
''',
    "observability test imports",
)
text = replace_once(
    text,
    '''    fn mutation_boundary_defaults_to_fail_open_before_restore_starts() {
        assert!(!KanidmRestoreStatus::default().database_mutation_started);
    }
''',
    '''    fn mutation_boundary_defaults_to_fail_open_before_restore_starts() {
        assert!(!KanidmRestoreStatus::default().database_mutation_started);
    }

    #[test]
    fn completed_restore_sets_ready_condition() {
        let mut status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::Completed,
            ..Default::default()
        };
        update_restore_conditions(&mut status, Some(7));
        assert!(status.conditions.iter().any(|condition| {
            condition.type_ == CONDITION_READY && condition.status == CONDITION_TRUE
        }));
        assert!(!status.conditions.iter().any(|condition| {
            condition.type_ == CONDITION_FAILED && condition.status == CONDITION_TRUE
        }));
    }

    #[test]
    fn failed_restore_sets_failed_condition() {
        let mut status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::Failed,
            message: Some("verification failed".to_string()),
            ..Default::default()
        };
        update_restore_conditions(&mut status, Some(8));
        assert!(status.conditions.iter().any(|condition| {
            condition.type_ == CONDITION_FAILED && condition.status == CONDITION_TRUE
        }));
    }
''',
    "terminal condition tests",
)

path.write_text(text)
