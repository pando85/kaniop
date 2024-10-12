use crate::crd::Kanidm;

use k8s_openapi::api::networking::v1::{
    HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
    IngressServiceBackend, IngressSpec, IngressTLS, ServiceBackendPort,
};
use kube::api::{ObjectMeta, Resource};
use kube::ResourceExt;

pub trait IngressExt {
    // TODO: clean
    #[allow(dead_code)]
    fn get_ingress(&self) -> Option<Ingress>;
}

impl IngressExt for Kanidm {
    fn get_ingress(&self) -> Option<Ingress> {
        self.spec.ingress.clone().map(|ingress| {
            let labels = self
                .get_labels()
                .clone()
                .into_iter()
                .chain(self.labels().clone())
                .collect();

            let hosts = std::iter::once(self.spec.domain.clone());
            Ingress {
                metadata: ObjectMeta {
                    name: Some(self.name_any()),
                    namespace: Some(self.namespace().unwrap()),
                    labels: Some(labels),
                    annotations: ingress.annotations.clone(),
                    owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                    ..ObjectMeta::default()
                },
                spec: Some(IngressSpec {
                    ingress_class_name: ingress.ingress_class_name.clone(),
                    rules: Some(
                        hosts
                            .clone()
                            .map(|host| IngressRule {
                                host: Some(host.clone()),
                                http: Some(HTTPIngressRuleValue {
                                    paths: vec![HTTPIngressPath {
                                        backend: IngressBackend {
                                            service: Some(IngressServiceBackend {
                                                name: self.name_any(),
                                                port: Some(ServiceBackendPort {
                                                    name: Some(self.spec.port_name.clone()),
                                                    ..ServiceBackendPort::default()
                                                }),
                                            }),
                                            ..IngressBackend::default()
                                        },
                                        path: Some("/".to_string()),
                                        ..HTTPIngressPath::default()
                                    }],
                                }),
                            })
                            .collect(),
                    ),
                    tls: Some(vec![IngressTLS {
                        hosts: Some(hosts.collect()),
                        secret_name: Some(
                            ingress
                                .tls_secret_name
                                .unwrap_or_else(|| format!("{}-tls", self.name_any())),
                        ),
                    }]),
                    ..IngressSpec::default()
                }),
                ..Ingress::default()
            }
        })
    }
}
