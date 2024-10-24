use crate::crd::{Kanidm, KanidmStatus};

use kaniop_operator::controller::Context;
use kaniop_operator::error::{Error, Result};

use std::sync::Arc;

use chrono::Utc;
use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetStatus};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::reflector::ObjectRef;
use kube::ResourceExt;
use serde_json::json;
use tracing::{debug, trace};

/// At least one replica has been ready for `minReadySeconds`.
const TYPE_AVAILABLE: &str = "Available";
/// Any StatefulSet is progressing
const TYPE_PROGRESSING: &str = "Progressing";
/// Secrets exist
#[allow(dead_code)]
const TYPE_INITIALIZED: &str = "Initialized";
/// Indicates whether the StatefulSet has failed to create or delete replicas.
const TYPE_REPLICA_FAILURE: &str = "ReplicaFailure";

const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

const REASON_AVAILABLE: &str = "ReplicaReady";
const REASON_NOT_AVAILABLE: &str = "NoReplicaReady";
const REASON_PROGRESSING: &str = "StatefulSetProgressing";
const REASON_NOT_PROGRESSING: &str = "StatefulSetNotProgressing";
const REASON_REPLICA_FAILURE: &str = "ReplicaCreationFailure";
const REASON_NO_REPLICA_FAILURE: &str = "NoReplicaFailure";

const MESSAGE_AVAILABLE: &str = "At least one replica is ready.";
const MESSAGE_NOT_AVAILABLE: &str = "No replicas are ready.";
const MESSAGE_PROGRESSING: &str = "StatefulSet is progressing.";
const MESSAGE_NOT_PROGRESSING: &str = "StatefulSet is not progressing.";
const MESSAGE_REPLICA_FAILURE: &str = "Failed to create or delete replicas.";
const MESSAGE_NO_REPLICA_FAILURE: &str = "No replica creation or deletion failures.";

pub trait StatusExt {
    async fn update_status(&self, ctx: Arc<Context>) -> Result<()>;
}

impl StatusExt for Kanidm {
    async fn update_status(&self, ctx: Arc<Context>) -> Result<()> {
        let sts_store = ctx
            .stores
            .stateful_set_store
            .as_ref()
            // safe unwrap because we know the statefulset store is defined
            .unwrap();

        let name = &self.name_any();
        let namespace = &self.get_namespace();
        let sts_status = self
            .iter_statefulset_names()
            .map(|sts_name| {
                let sts_ref = ObjectRef::<StatefulSet>::new_with(&sts_name, ()).within(namespace);
                sts_store
                    .get(&sts_ref)
                    .as_ref()
                    .and_then(|sts| sts.status.clone())
            })
            .collect::<Vec<Option<StatefulSetStatus>>>();

        let new_status = generate_status(
            self.status
                .as_ref()
                .cloned()
                .unwrap_or_default()
                .conditions
                .unwrap_or_default(),
            &sts_status,
            self.metadata.generation,
        );

        let new_status_patch = Patch::Apply(json!({
            "apiVersion": "kaniop.rs/v1",
            "kind": "Kanidm",
            "status": new_status
        }));
        debug!(msg = "updating Kanidm status");
        trace!(msg = format!("new status {:?}", new_status_patch));
        let patch = PatchParams::apply("kanidms.kaniop.rs").force();
        let kanidm_api = Api::<Kanidm>::namespaced(ctx.client.clone(), namespace);
        let _o = kanidm_api
            .patch_status(name, &patch, &new_status_patch)
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }
}

fn generate_status(
    previous_conditions: Vec<Condition>,
    statefulset_statuses: &[Option<StatefulSetStatus>],
    kanidm_generation: Option<i64>,
) -> KanidmStatus {
    let new_conditions =
        generate_status_conditions(previous_conditions, statefulset_statuses, kanidm_generation);

    let available_replicas = statefulset_statuses
        .iter()
        .filter_map(|sts| sts.as_ref())
        .map(|sts| sts.available_replicas.unwrap_or(0))
        .sum();

    let replicas = statefulset_statuses
        .iter()
        .filter_map(|sts| sts.as_ref())
        .map(|sts| sts.replicas)
        .sum();

    KanidmStatus {
        conditions: Some(new_conditions),
        available_replicas,
        replicas,
        unavailable_replicas: replicas - available_replicas,
        updated_replicas: statefulset_statuses
            .iter()
            .filter_map(|sts| sts.as_ref())
            .map(|sts| sts.updated_replicas.unwrap_or(0))
            .sum(),
    }
}

/// Generates a list of status conditions for a Kanidm based on its current status and previous conditions.
fn generate_status_conditions(
    previous_conditions: Vec<Condition>,
    statefulset_statuses: &[Option<StatefulSetStatus>],
    // TODO: secrets_exist: bool, for Initialized condition
    kanidm_generation: Option<i64>,
) -> Vec<Condition> {
    let sts_statuses = statefulset_statuses.iter().filter_map(|sts| sts.as_ref());

    let available_condition = match sts_statuses
        .clone()
        .any(|s| s.available_replicas == Some(1))
    {
        true => Condition {
            type_: TYPE_AVAILABLE.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: REASON_AVAILABLE.to_string(),
            message: MESSAGE_AVAILABLE.to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
        false => Condition {
            type_: TYPE_AVAILABLE.to_string(),
            status: CONDITION_FALSE.to_string(),
            reason: REASON_NOT_AVAILABLE.to_string(),
            message: MESSAGE_NOT_AVAILABLE.to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
    };

    let progressing_condition = match sts_statuses.clone().any(|s| {
        s.conditions
            .as_ref()
            .map(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == TYPE_PROGRESSING && c.status == CONDITION_TRUE)
            })
            .unwrap_or(false)
    }) {
        true => Condition {
            type_: TYPE_PROGRESSING.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: REASON_PROGRESSING.to_string(),
            message: MESSAGE_PROGRESSING.to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
        false => Condition {
            type_: TYPE_PROGRESSING.to_string(),
            status: CONDITION_FALSE.to_string(),
            reason: REASON_NOT_PROGRESSING.to_string(),
            message: MESSAGE_NOT_PROGRESSING.to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
    };

    let replicate_failure_condition = match sts_statuses.clone().any(|s| {
        s.conditions
            .as_ref()
            .map(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == TYPE_REPLICA_FAILURE && c.status == CONDITION_TRUE)
            })
            .unwrap_or(false)
    }) {
        true => Condition {
            type_: TYPE_REPLICA_FAILURE.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: REASON_REPLICA_FAILURE.to_string(),
            message: MESSAGE_REPLICA_FAILURE.to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
        false => Condition {
            type_: TYPE_REPLICA_FAILURE.to_string(),
            status: CONDITION_FALSE.to_string(),
            reason: REASON_NO_REPLICA_FAILURE.to_string(),
            message: MESSAGE_NO_REPLICA_FAILURE.to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
    };

    vec![
        available_condition,
        progressing_condition,
        replicate_failure_condition,
    ]
    .into_iter()
    .fold(previous_conditions, |previous_conditions, c| {
        update_conditions(previous_conditions, &c)
    })
}

/// Update conditions based on the current status and previous conditions in the Kanidm
fn update_conditions(
    previous_conditions: Vec<Condition>,
    new_condition: &Condition,
) -> Vec<Condition> {
    if previous_conditions
        .iter()
        .any(|c| c.type_ == *new_condition.type_)
    {
        previous_conditions
            .iter()
            .filter(|c| c.type_ != *new_condition.type_)
            .cloned()
            .chain(std::iter::once(new_condition.clone()))
            .collect()
    } else {
        previous_conditions
            .iter()
            .cloned()
            .chain(std::iter::once(new_condition.clone()))
            .collect()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use chrono::Utc;

    fn create_condition(type_: &str, status: &str) -> Condition {
        Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: None,
        }
    }

    #[test]
    fn test_update_conditions_with_existing_status_type() {
        let previous_conditions = vec![
            create_condition(TYPE_AVAILABLE, CONDITION_TRUE),
            create_condition(TYPE_PROGRESSING, CONDITION_FALSE),
        ];
        let new_condition = create_condition(TYPE_AVAILABLE, CONDITION_FALSE);

        let updated_conditions = update_conditions(previous_conditions.clone(), &new_condition);

        assert_eq!(updated_conditions.len(), 2);
        assert!(updated_conditions
            .iter()
            .any(|c| c.type_ == TYPE_AVAILABLE && c.status == CONDITION_FALSE));
        assert!(updated_conditions
            .iter()
            .any(|c| c.type_ == TYPE_PROGRESSING && c.status == CONDITION_FALSE));
    }

    #[test]
    fn test_update_conditions_without_existing_status_type() {
        let previous_conditions = vec![create_condition(TYPE_PROGRESSING, CONDITION_FALSE)];
        let new_condition = create_condition(TYPE_AVAILABLE, CONDITION_TRUE);

        let updated_conditions = update_conditions(previous_conditions.clone(), &new_condition);

        assert_eq!(updated_conditions.len(), 2);
        assert!(updated_conditions
            .iter()
            .any(|c| c.type_ == TYPE_AVAILABLE && c.status == CONDITION_TRUE));
        assert!(updated_conditions
            .iter()
            .any(|c| c.type_ == TYPE_PROGRESSING && c.status == CONDITION_FALSE));
    }

    #[test]
    fn test_update_conditions_with_empty_previous_conditions() {
        let previous_conditions = vec![];
        let new_condition = create_condition(TYPE_AVAILABLE, CONDITION_TRUE);

        let updated_conditions = update_conditions(previous_conditions.clone(), &new_condition);

        assert_eq!(updated_conditions.len(), 1);
        assert!(updated_conditions
            .iter()
            .any(|c| c.type_ == TYPE_AVAILABLE && c.status == CONDITION_TRUE));
    }
}
