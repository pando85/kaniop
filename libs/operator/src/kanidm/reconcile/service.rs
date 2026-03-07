use std::collections::BTreeMap;

use super::statefulset::{
    CONTAINER_REPLICATION_PORT, CONTAINER_REPLICATION_PORT_NAME, REPLICA_GROUP_LABEL,
    REPLICA_LABEL, StatefulSetExt,
};

use crate::kanidm::crd::{Kanidm, ReplicaGroup};

use k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::ResourceExt;
use kube::api::{ObjectMeta, Resource};

pub trait ServiceExt {
    fn service_name(&self) -> String;
    fn create_service(&self) -> Service;
    fn replica_group_service_name(&self, rg_name: &str, i: i32) -> String;
    fn create_replica_group_service(&self, rg: &ReplicaGroup, i: i32) -> Service;
}

impl ServiceExt for Kanidm {
    #[inline]
    fn service_name(&self) -> String {
        self.name_any()
    }

    fn create_service(&self) -> Service {
        let labels = self
            .generate_labels()
            .into_iter()
            .chain(
                self.spec
                    .service
                    .as_ref()
                    .and_then(|s| s.additional_labels.clone())
                    .unwrap_or_default(),
            )
            .collect();

        let ports = std::iter::once(ServicePort {
            name: Some(self.spec.port_name.clone()),
            port: 8443,
            target_port: Some(IntOrString::String(self.spec.port_name.clone())),
            ..ServicePort::default()
        })
        .chain(
            self.spec
                .ldap_port_name
                .clone()
                .into_iter()
                .map(|port_name| ServicePort {
                    name: Some(port_name.clone()),
                    port: 3636,
                    target_port: Some(IntOrString::String(port_name)),
                    ..ServicePort::default()
                }),
        )
        .collect();

        Service {
            metadata: ObjectMeta {
                name: Some(self.service_name()),
                namespace: Some(self.get_namespace()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                annotations: self
                    .spec
                    .service
                    .as_ref()
                    .and_then(|s| s.annotations.clone()),
                labels: Some(labels),
                ..ObjectMeta::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(self.generate_resource_labels()),
                ports: Some(ports),
                type_: self.spec.service.as_ref().and_then(|s| s.type_.clone()),
                ..ServiceSpec::default()
            }),
            ..Service::default()
        }
    }

    #[inline]
    fn replica_group_service_name(&self, rg_name: &str, i: i32) -> String {
        self.pod_name(rg_name, i)
    }

    fn create_replica_group_service(&self, rg: &ReplicaGroup, i: i32) -> Service {
        Service {
            metadata: ObjectMeta {
                name: Some(self.replica_group_service_name(&rg.name, i)),
                namespace: Some(self.get_namespace()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                annotations: {
                    rg.services.as_ref().and_then(|s| {
                        s.annotations_template.clone().and_then(|a| {
                            let annotations = a
                                .into_iter()
                                .map(|(k, t)| {
                                    let pod_name = self.pod_name(&rg.name, i);
                                    (
                                        k,
                                        t.replace("{pod_name}", &pod_name)
                                            .replace("{replica_index}", &i.to_string())
                                            .replace("{domain}", &self.spec.domain),
                                    )
                                })
                                .collect::<BTreeMap<_, _>>();
                            (!annotations.is_empty()).then_some(annotations)
                        })
                    })
                },
                labels: Some(
                    self.generate_resource_labels()
                        .clone()
                        .into_iter()
                        .chain(self.labels().clone())
                        .chain([
                            (REPLICA_GROUP_LABEL.to_string(), rg.name.to_string()),
                            (REPLICA_LABEL.to_string(), self.pod_name(&rg.name, i)),
                        ])
                        .chain(
                            rg.services
                                .as_ref()
                                .and_then(|s| s.additional_labels.clone())
                                .unwrap_or_default(),
                        )
                        .collect(),
                ),
                ..ObjectMeta::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(
                    self.generate_resource_labels()
                        .into_iter()
                        .chain([
                            (REPLICA_GROUP_LABEL.to_string(), rg.name.to_string()),
                            ("apps.kubernetes.io/pod-index".to_string(), i.to_string()),
                        ])
                        .collect(),
                ),
                ports: Some(vec![ServicePort {
                    name: Some(self.spec.port_name.clone()),
                    port: CONTAINER_REPLICATION_PORT,
                    target_port: Some(IntOrString::String(
                        CONTAINER_REPLICATION_PORT_NAME.to_string(),
                    )),
                    ..ServicePort::default()
                }]),
                type_: Some("LoadBalancer".to_string()),
                ..ServiceSpec::default()
            }),
            ..Service::default()
        }
    }
}
