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

static STATUS_READY: &str = "Ready";
static STATUS_PROGRESSING: &str = "Progressing";

pub trait StatusExt {
    async fn update_status(&self, ctx: Arc<Context<StatefulSet>>) -> Result<()>;
    fn generate_status(
        &self,
        statefulset_status: &StatefulSetStatus,
        statefulset_metadata_generation: Option<i64>,
    ) -> Result<KanidmStatus, Error>;
    fn update_conditions(&self, new_condition: &Condition, status_type: &str) -> Vec<Condition>;
    fn determine_status_type(statefulset_status: &StatefulSetStatus) -> &str;
}

impl StatusExt for Kanidm {
    async fn update_status(&self, ctx: Arc<Context<StatefulSet>>) -> Result<()> {
        let namespace = &self.get_namespace();
        let statefulset_ref =
            ObjectRef::<StatefulSet>::new_with(&self.name_any(), ()).within(namespace);
        debug!(msg = "getting statefulset");
        let statefulset = ctx
            .stores
            .get("statefulset")
            // safe unwrap: statefulset store should exists
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

        // Create a new condition with the current status
        let new_condition = Condition {
            type_: status_type.to_string(),
            status: "True".to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: statefulset_metadata_generation,
        };

        let conditions = self.update_conditions(&new_condition, status_type);

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
            Some(ready_replicas) if ready_replicas == statefulset_status.replicas => STATUS_READY,
            _ => STATUS_PROGRESSING,
        }
    }

    // TODO: Generate Available and Reconciled status
    /// Update conditions based on the current status and previous conditions in the Kanidm
    fn update_conditions(&self, new_condition: &Condition, status_type: &str) -> Vec<Condition> {
        match self.status.as_ref().and_then(|s| s.conditions.as_ref()) {
            // Remove the 'Ready' condition if we are 'Progressing'
            Some(previous_conditions) if status_type == STATUS_PROGRESSING => previous_conditions
                .iter()
                .filter(|c| c.type_ != STATUS_READY)
                .cloned()
                .chain(std::iter::once(new_condition.clone()))
                .collect(),

            // Add the new condition if it's not already present
            Some(previous_conditions)
                if !previous_conditions.iter().any(|c| c.type_ == *status_type) =>
            {
                previous_conditions
                    .iter()
                    .cloned()
                    .chain(std::iter::once(new_condition.clone()))
                    .collect()
            }

            // Otherwise, keep the existing conditions unchanged
            Some(previous_conditions) => previous_conditions.clone(),

            // No previous conditions; start fresh with the new condition
            None => vec![new_condition.clone()],
        }
    }
}

#[cfg(test)]
mod test {
    use super::{StatusExt, STATUS_PROGRESSING, STATUS_READY};

    use crate::crd::{Kanidm, KanidmStatus};

    use chrono::Utc;
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
        let kanidm = Kanidm::test(None);

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        assert_eq!(result.available_replicas, 3);
        assert_eq!(result.unavailable_replicas, 0);
        assert_eq!(result.replicas, 3);
        assert_eq!(result.updated_replicas, 3);

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, STATUS_READY);
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
        let kanidm = Kanidm::test(None);

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        assert_eq!(result.available_replicas, 2);
        assert_eq!(result.unavailable_replicas, 1);
        assert_eq!(result.replicas, 3);
        assert_eq!(result.updated_replicas, 2);

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, STATUS_PROGRESSING);
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
            type_: STATUS_PROGRESSING.to_string(),
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

        let kanidm = Kanidm::test(Some(kanidm_status));

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 2);
        assert!(conditions.iter().any(|c| c.type_ == STATUS_READY));
        assert!(conditions.iter().any(|c| c.type_ == STATUS_PROGRESSING));
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
            type_: STATUS_READY.to_string(),
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

        let kanidm = Kanidm::test(Some(kanidm_status));

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert!(conditions.iter().all(|c| c.type_ == STATUS_PROGRESSING));
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
        let kanidm = Kanidm::test(None);

        let result = kanidm
            .generate_status(&statefulset_status, statefulset_metadata_generation)
            .unwrap();

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, STATUS_PROGRESSING);
    }
}
