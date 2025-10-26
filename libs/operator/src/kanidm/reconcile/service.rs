use crate::kanidm::crd::Kanidm;

use k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::ResourceExt;
use kube::api::{ObjectMeta, Resource};

pub trait ServiceExt {
    fn service_name(&self) -> String;
    fn create_service(&self) -> Service;
}

impl ServiceExt for Kanidm {
    #[inline]
    fn service_name(&self) -> String {
        self.name_any()
    }

    fn create_service(&self) -> Service {
        let labels = self.generate_labels();

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
                selector: Some(self.generate_resource_labels()),
                ports: Some(ports),
                type_: self.spec.service.as_ref().and_then(|s| s.type_.clone()),
                ..ServiceSpec::default()
            }),
            ..Service::default()
        }
    }
}
