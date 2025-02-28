use crate::kanidm::crd::Kanidm;

use k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::ResourceExt;
use kube::api::{ObjectMeta, Resource};

use super::statefulset::{CONTAINER_REPLICATION_PORT, CONTAINER_REPLICATION_PORT_NAME};

pub trait ServiceExt {
    fn service_name(&self) -> String;
    fn create_service(&self) -> Service;
    fn create_pod_service(&self, name: &str) -> Service;
}

trait ServiceExtPrivate {
    fn create_service_internal(
        &self,
        name: String,
        resource_labels: std::collections::BTreeMap<String, String>,
        ports: Vec<ServicePort>,
    ) -> Service;
}

impl ServiceExt for Kanidm {
    #[inline]
    fn service_name(&self) -> String {
        self.name_any()
    }

    fn create_service(&self) -> Service {
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
        self.create_service_internal(self.service_name(), self.generate_resource_labels(), ports)
    }

    fn create_pod_service(&self, name: &str) -> Service {
        let resource_labels = self
            .generate_resource_labels()
            .into_iter()
            .chain(std::iter::once((
                "statefulset.kubernetes.io/pod-name".to_string(),
                name.to_string(),
            )))
            .collect();
        let ports = [ServicePort {
            name: Some(CONTAINER_REPLICATION_PORT_NAME.to_string()),
            port: CONTAINER_REPLICATION_PORT,
            target_port: Some(IntOrString::String(
                CONTAINER_REPLICATION_PORT_NAME.to_string(),
            )),
            ..ServicePort::default()
        }];
        self.create_service_internal(name.to_string(), resource_labels, ports.to_vec())
    }
}

impl ServiceExtPrivate for Kanidm {
    fn create_service_internal(
        &self,
        name: String,
        resource_labels: std::collections::BTreeMap<String, String>,
        ports: Vec<ServicePort>,
    ) -> Service {
        let labels = self
            .generate_resource_labels()
            .clone()
            .into_iter()
            .chain(self.labels().clone())
            .chain(resource_labels.clone())
            .collect();

        Service {
            metadata: ObjectMeta {
                name: Some(name),
                namespace: Some(self.namespace().unwrap()),
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
                selector: Some(resource_labels),
                ports: Some(ports),
                type_: self.spec.service.as_ref().and_then(|s| s.type_.clone()),
                ..ServiceSpec::default()
            }),
            ..Service::default()
        }
    }
}
