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
    fn generate_status(
        &self,
        statefulset_status: &StatefulSetStatus,
        statefulset_metadata_generation: Option<i64>,
    ) -> Result<KanidmStatus, Error>;
    fn determine_status_type(statefulset_status: &StatefulSetStatus) -> &str;
}

impl StatusExt for Kanidm {
    async fn update_status(&self, ctx: Arc<Context>) -> Result<()> {
        let namespace = &self.get_namespace();
        // TODO: handle all statefulsets
        let statefulset_ref =
            ObjectRef::<StatefulSet>::new_with(&format!("{}-0", &self.name_any()), ())
                .within(namespace);
        debug!(msg = "getting statefulset");
        let statefulset = ctx
            .stores
            .stateful_set_store
            .as_ref()
            // safe unwrap because we know the statefulset store is defined
            .unwrap()
            .get(&statefulset_ref)
            .ok_or_else(|| Error::MissingObject("statefulset"))?;

        let owner = statefulset
            .metadata
            .owner_references
            .as_ref()
            .and_then(|refs| refs.iter().find(|r| r.controller == Some(true)))
            .ok_or_else(|| Error::MissingObjectKey("ownerReferences"))?;

        let statefulset_status = statefulset
            .status
            .as_ref()
            .ok_or_else(|| Error::MissingObjectKey("status"))?;

        let new_status =
            self.generate_status(statefulset_status, statefulset.metadata.generation)?;

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
            .patch_status(&owner.name, &patch, &new_status_patch)
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    /// Generate the KanidmStatus based on the statefulset status
    fn generate_status(
        &self,
        statefulset_status: &StatefulSetStatus,
        statefulset_metadata_generation: Option<i64>,
    ) -> Result<KanidmStatus, Error> {
        let status_type = Kanidm::determine_status_type(statefulset_status);

        let new_condition = Condition {
            type_: status_type.to_string(),
            status: "True".to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: statefulset_metadata_generation,
        };

        let conditions = update_conditions(
            self.status
                .as_ref()
                .cloned()
                .unwrap_or_default()
                .conditions
                .unwrap_or_default(),
            &new_condition,
            status_type,
        );

        let available_replicas = statefulset_status
            .available_replicas
            .ok_or_else(|| Error::MissingObjectKey("available_replicas"))?;

        Ok(KanidmStatus {
            available_replicas,
            replicas: statefulset_status.replicas,
            updated_replicas: statefulset_status
                .updated_replicas
                .ok_or_else(|| Error::MissingObjectKey("updated_replicas"))?,
            unavailable_replicas: statefulset_status.replicas - available_replicas,
            conditions: Some(conditions),
        })
    }

    /// Determine the status type based on the statefulset status
    fn determine_status_type(statefulset_status: &StatefulSetStatus) -> &str {
        match statefulset_status.ready_replicas {
            Some(ready_replicas) if ready_replicas == statefulset_status.replicas => TYPE_AVAILABLE,
            _ => TYPE_PROGRESSING,
        }
    }
}

/// Generates a list of status conditions for a Kanidm based on its current status and previous conditions.
fn generate_status_conditions(
    previous_conditions: Vec<Condition>,
    statefulset_statuses: Vec<Option<StatefulSetStatus>>,
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

    use super::{StatusExt, TYPE_AVAILABLE, TYPE_PROGRESSING};

    use crate::crd::{Kanidm, KanidmStatus};

    use k8s_openapi::api::apps::v1::StatefulSetStatus;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

    #[test]
    fn test_generate_status_ready() {
        let statefulset_status = StatefulSetStatus {
            available_replicas: Some(3),
            ready_replicas: Some(3),
            replicas: 3,
            updated_replicas: Some(3),
            ..Default::default()
        };

        let statefulset_metadata_generation = Some(1);
        let kanidm = Kanidm::test();

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        assert_eq!(result.available_replicas, 3);
        assert_eq!(result.unavailable_replicas, 0);
        assert_eq!(result.replicas, 3);
        assert_eq!(result.updated_replicas, 3);

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, TYPE_AVAILABLE);
    }

    #[test]
    fn test_generate_status_progressing() {
        let statefulset_status = StatefulSetStatus {
            available_replicas: Some(2),
            ready_replicas: Some(2),
            replicas: 3,
            updated_replicas: Some(2),
            ..Default::default()
        };

        let statefulset_metadata_generation = Some(2);
        let kanidm = Kanidm::test();

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        assert_eq!(result.available_replicas, 2);
        assert_eq!(result.unavailable_replicas, 1);
        assert_eq!(result.replicas, 3);
        assert_eq!(result.updated_replicas, 2);

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, TYPE_PROGRESSING);
    }

    #[test]
    fn test_generate_status_add_new_condition() {
        let statefulset_status = StatefulSetStatus {
            available_replicas: Some(3),
            ready_replicas: Some(3),
            replicas: 3,
            updated_replicas: Some(3),
            ..Default::default()
        };

        let statefulset_metadata_generation = Some(3);

        // Previous condition with a different type (Progressing)
        let previous_conditions = vec![Condition {
            type_: TYPE_PROGRESSING.to_string(),
            status: "True".to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: Some(1),
        }];

        let kanidm_status = KanidmStatus {
            conditions: Some(previous_conditions),
            ..Default::default()
        };

        let kanidm = Kanidm::test().with_status(kanidm_status);

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 2);
        assert!(conditions.iter().any(|c| c.type_ == TYPE_AVAILABLE));
        assert!(conditions.iter().any(|c| c.type_ == TYPE_PROGRESSING));
    }

    #[test]
    fn test_generate_status_replace_ready_condition() {
        let statefulset_status = StatefulSetStatus {
            available_replicas: Some(2),
            ready_replicas: Some(2),
            replicas: 3,
            updated_replicas: Some(2),
            ..Default::default()
        };

        let statefulset_metadata_generation = Some(4);

        // Previous condition with type Ready
        let previous_conditions = vec![Condition {
            type_: TYPE_AVAILABLE.to_string(),
            status: "True".to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: Some(2),
        }];

        let kanidm_status = KanidmStatus {
            conditions: Some(previous_conditions),
            ..Default::default()
        };

        let kanidm = Kanidm::test().with_status(kanidm_status);

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert!(conditions.iter().all(|c| c.type_ == TYPE_PROGRESSING));
    }

    #[test]
    fn test_generate_status_no_previous_conditions() {
        let statefulset_status = StatefulSetStatus {
            available_replicas: Some(2),
            ready_replicas: Some(2),
            replicas: 3,
            updated_replicas: Some(2),
            ..Default::default()
        };

        let statefulset_metadata_generation = Some(5);
        let kanidm = Kanidm::test();

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, TYPE_PROGRESSING);
    }
}
