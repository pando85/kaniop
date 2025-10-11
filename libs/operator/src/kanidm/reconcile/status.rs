use super::KANIDM_OPERATOR_NAME;
use super::secret::SecretExt;
use super::statefulset::StatefulSetExt;

use crate::error::{Error, Result};
use crate::kanidm::controller::context::Context;
use crate::kanidm::crd::{Kanidm, KanidmReplicaState, KanidmReplicaStatus, KanidmStatus};

use std::sync::Arc;

use chrono::Utc;
use futures::future::join_all;
use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetStatus};
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::reflector::ObjectRef;
use tracing::{debug, trace};

/// At least one replica has been ready for `minReadySeconds`.
const TYPE_AVAILABLE: &str = "Available";
/// Any StatefulSet is progressing
const TYPE_PROGRESSING: &str = "Progressing";
/// Admin secret exists
const TYPE_INITIALIZED: &str = "Initialized";
/// Indicates whether the StatefulSet has failed to create or delete replicas.
const TYPE_REPLICA_FAILURE: &str = "ReplicaFailure";

const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

#[allow(async_fn_in_trait)]
pub trait StatusExt {
    async fn update_status(&self, ctx: Arc<Context>) -> Result<KanidmStatus>;
}

impl StatusExt for Kanidm {
    async fn update_status(&self, ctx: Arc<Context>) -> Result<KanidmStatus> {
        let name = &self.name_any();
        let namespace = &self.get_namespace();
        let statefulsets = self
            .spec
            .replica_groups
            .iter()
            .filter_map(|rg| {
                let sts_name = self.statefulset_name(&rg.name);
                let sts_ref = ObjectRef::<StatefulSet>::new_with(&sts_name, ()).within(namespace);
                ctx.stores.stateful_set_store.get(&sts_ref)
            })
            .collect::<Vec<Arc<StatefulSet>>>();

        let sts_status = statefulsets
            .iter()
            .map(|sts| sts.status.clone())
            .collect::<Vec<Option<StatefulSetStatus>>>();

        let secret_ref = ObjectRef::<Secret>::new_with(&self.admins_secret_name(), ())
            .within(&self.get_namespace());
        let admin_secret = ctx
            .stores
            .secret_store
            .get(&secret_ref)
            .map(|s| s.name_any());

        let replica_infos_futures: Vec<_> = statefulsets
            .iter()
            .flat_map(|sts| {
                let replicas = sts.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
                let secret_store = ctx.stores.secret_store.clone();
                let ctx_clone = ctx.clone();
                let namespace = namespace.to_string();

                (0..replicas).map(move |i| {
                    let sts_name = sts.name_any();
                    let secret_store = secret_store.clone();
                    let ctx = ctx_clone.clone();
                    let namespace = namespace.clone();
                    let pod_name = format!("{sts_name}-{i}");
                    let secret_name = self.replica_secret_name(&pod_name);
                    let secret_ref =
                        ObjectRef::<Secret>::new_with(&secret_name, ()).within(&namespace);
                    async move {
                        let is_certificate_expiring =
                            ctx.get_repl_cert_exp(&secret_ref).await.map(|exp| {
                                let now = Utc::now().timestamp();
                                // 1 month in seconds
                                let threshold = 30 * 24 * 60 * 60;
                                trace!(msg = format!("replica cert expiration {exp}, now {now}, threshold {threshold}"));
                                exp - now < threshold
                            });
                        ReplicaInformation {
                            pod_name,
                            statefulset_name: sts_name.clone(),
                            replica_secret_exists: secret_store.get(&secret_ref).is_some(),
                            is_certificate_expiring,
                        }
                    }
                })
            })
            .collect();
        let replica_infos = join_all(replica_infos_futures).await;

        let new_status = generate_status(
            self.status
                .as_ref()
                .cloned()
                .unwrap_or_default()
                .conditions
                .unwrap_or_default(),
            &sts_status,
            admin_secret,
            replica_infos,
            self.is_replication_enabled(),
            self.metadata.generation,
        );

        let new_status_patch = Patch::Apply(Kanidm {
            status: Some(new_status.clone()),
            ..Kanidm::default()
        });
        debug!(msg = "updating Kanidm status");
        trace!(msg = format!("new status {:?}", new_status_patch));
        let patch = PatchParams::apply(KANIDM_OPERATOR_NAME).force();
        let kanidm_api = Api::<Kanidm>::namespaced(ctx.kaniop_ctx.client.clone(), namespace);
        let _o = kanidm_api
            .patch_status(name, &patch, &new_status_patch)
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to patch Kanidm/status {namespace}/{name}"),
                    Box::new(e),
                )
            })?;
        Ok(new_status)
    }
}

struct ReplicaInformation {
    pod_name: String,
    statefulset_name: String,
    replica_secret_exists: bool,
    is_certificate_expiring: Option<bool>,
}

pub fn is_kanidm_available(status: KanidmStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == TYPE_AVAILABLE && c.status == CONDITION_TRUE)
}

pub fn is_kanidm_initialized(status: KanidmStatus) -> bool {
    status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == TYPE_INITIALIZED && c.status == CONDITION_TRUE)
}

fn generate_status(
    previous_conditions: Vec<Condition>,
    statefulset_statuses: &[Option<StatefulSetStatus>],
    secret_name: Option<String>,
    replica_infos: Vec<ReplicaInformation>,
    is_replication_enabled: bool,
    kanidm_generation: Option<i64>,
) -> KanidmStatus {
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

    let replica_statuses = replica_infos
        .iter()
        .map(|ri| KanidmReplicaStatus {
            pod_name: ri.pod_name.clone(),
            statefulset_name: ri.statefulset_name.clone(),
            state: if ri.replica_secret_exists || !is_replication_enabled {
                if Some(true) == ri.is_certificate_expiring {
                    debug!(msg = format!("replica cert is expiring for pod {}", ri.pod_name));
                    KanidmReplicaState::CertificateExpiring
                } else {
                    KanidmReplicaState::Initialized
                }
            } else {
                KanidmReplicaState::Pending
            },
        })
        .collect::<Vec<KanidmReplicaStatus>>();

    let new_conditions = generate_status_conditions(
        previous_conditions,
        statefulset_statuses,
        secret_name.is_some(),
        &replica_statuses,
        kanidm_generation,
    );

    let replica_column = format!("{available_replicas}/{replicas}");
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
        replica_statuses,
        replica_column,
        secret_name,
    }
}

/// Generates a list of status conditions for a Kanidm based on its current status and previous conditions.
fn generate_status_conditions(
    previous_conditions: Vec<Condition>,
    statefulset_statuses: &[Option<StatefulSetStatus>],
    secret_exists: bool,
    replica_statuses: &[KanidmReplicaStatus],
    kanidm_generation: Option<i64>,
) -> Vec<Condition> {
    let sts_statuses = statefulset_statuses.iter().filter_map(|sts| sts.as_ref());

    let available_condition = match sts_statuses
        .clone()
        .any(|s| s.available_replicas >= Some(1))
    {
        true => Condition {
            type_: TYPE_AVAILABLE.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: "ReplicaReady".to_string(),
            message: "At least one replica is ready.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
        false => Condition {
            type_: TYPE_AVAILABLE.to_string(),
            status: CONDITION_FALSE.to_string(),
            reason: "NoReplicaReady".to_string(),
            message: "No replicas are ready.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
    };

    let initialized_condition = match secret_exists {
        true => Condition {
            type_: TYPE_INITIALIZED.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: "AdminSecretExists".to_string(),
            message: "admin and idm_admin passwords have been generated.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
        false => Condition {
            type_: TYPE_INITIALIZED.to_string(),
            status: CONDITION_FALSE.to_string(),
            reason: "AdminSecretNotExists".to_string(),
            message: "admin and idm_admin passwords have not been generated.".to_string(),
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
            reason: "ReplicaCreationFailure".to_string(),
            message: "Failed to create or delete replicas.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
        false => Condition {
            type_: TYPE_REPLICA_FAILURE.to_string(),
            status: CONDITION_FALSE.to_string(),
            reason: "NoReplicaFailure".to_string(),
            message: "No replica creation or deletion failures.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        },
    };

    let progressing_condition = if sts_statuses.clone().any(|s| {
        s.conditions
            .as_ref()
            .map(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == TYPE_PROGRESSING && c.status == CONDITION_TRUE)
            })
            .unwrap_or(false)
    }) {
        Condition {
            type_: TYPE_PROGRESSING.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: "Progressing".to_string(),
            message: "StatefulSet is progressing.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        }
    } else if replica_statuses
        .iter()
        .any(|rs| rs.state == KanidmReplicaState::Pending)
    {
        Condition {
            type_: TYPE_PROGRESSING.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: "ReplicaStatusPending".to_string(),
            message: "At least one replica is pending.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        }
    } else if sts_statuses
        .clone()
        .any(|s| s.replicas > s.available_replicas.unwrap_or(0))
    {
        Condition {
            type_: TYPE_PROGRESSING.to_string(),
            status: CONDITION_TRUE.to_string(),
            reason: "ReplicaCreation".to_string(),
            message: "Replicas are being created.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        }
    } else {
        Condition {
            type_: TYPE_PROGRESSING.to_string(),
            status: CONDITION_FALSE.to_string(),
            reason: "NotProgressing".to_string(),
            message: "StatefulSet is not progressing.".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: kanidm_generation,
        }
    };

    [
        available_condition,
        progressing_condition,
        initialized_condition,
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
        assert!(
            updated_conditions
                .iter()
                .any(|c| c.type_ == TYPE_AVAILABLE && c.status == CONDITION_FALSE)
        );
        assert!(
            updated_conditions
                .iter()
                .any(|c| c.type_ == TYPE_PROGRESSING && c.status == CONDITION_FALSE)
        );
    }

    #[test]
    fn test_update_conditions_without_existing_status_type() {
        let previous_conditions = vec![create_condition(TYPE_PROGRESSING, CONDITION_FALSE)];
        let new_condition = create_condition(TYPE_AVAILABLE, CONDITION_TRUE);

        let updated_conditions = update_conditions(previous_conditions.clone(), &new_condition);

        assert_eq!(updated_conditions.len(), 2);
        assert!(
            updated_conditions
                .iter()
                .any(|c| c.type_ == TYPE_AVAILABLE && c.status == CONDITION_TRUE)
        );
        assert!(
            updated_conditions
                .iter()
                .any(|c| c.type_ == TYPE_PROGRESSING && c.status == CONDITION_FALSE)
        );
    }

    #[test]
    fn test_update_conditions_with_empty_previous_conditions() {
        let previous_conditions = vec![];
        let new_condition = create_condition(TYPE_AVAILABLE, CONDITION_TRUE);

        let updated_conditions = update_conditions(previous_conditions.clone(), &new_condition);

        assert_eq!(updated_conditions.len(), 1);
        assert!(
            updated_conditions
                .iter()
                .any(|c| c.type_ == TYPE_AVAILABLE && c.status == CONDITION_TRUE)
        );
    }
}
