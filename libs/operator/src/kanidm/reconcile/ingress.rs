use crate::kanidm::crd::Kanidm;

use k8s_openapi::api::networking::v1::{
    HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
    IngressServiceBackend, IngressSpec, IngressTLS, ServiceBackendPort,
};
use kube::ResourceExt;
use kube::api::{ObjectMeta, Resource};

pub trait IngressExt {
    fn create_ingress(&self) -> Option<Ingress>;
    fn create_region_ingress(&self) -> Option<Ingress>;
    fn generate_region_ingress_name(&self) -> Option<String>;
}

impl IngressExt for Kanidm {
    fn create_ingress(&self) -> Option<Ingress> {
        self.spec.ingress.as_ref().map(|ingress| {
            let labels = self.generate_labels();
            let tls_hosts: Vec<String> = std::iter::once(self.spec.domain.clone())
                .chain(ingress.extra_tls_hosts.clone().unwrap_or_default())
                .collect();
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
                    rules: Some(vec![self.generate_ingress_rule(self.spec.domain.clone())]),
                    tls: Some(vec![IngressTLS {
                        hosts: Some(tls_hosts),
                        secret_name: Some(
                            ingress
                                .tls_secret_name
                                .clone()
                                .unwrap_or_else(|| self.get_tls_secret_name()),
                        ),
                    }]),
                    ..IngressSpec::default()
                }),
                ..Ingress::default()
            }
        })
    }
    fn create_region_ingress(&self) -> Option<Ingress> {
        self.spec.region_ingress.as_ref().map(|ingress| {
            let labels = self.generate_labels();
            let host = format!("{}.{}", ingress.region, self.spec.domain);
            let tls_hosts: Vec<String> = std::iter::once(self.spec.domain.clone())
                .chain(std::iter::once(host.clone()))
                .collect();
            Ingress {
                metadata: ObjectMeta {
                    name: self.generate_region_ingress_name(),
                    namespace: Some(self.namespace().unwrap()),
                    labels: Some(labels),
                    annotations: ingress.annotations.clone(),
                    owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                    ..ObjectMeta::default()
                },
                spec: Some(IngressSpec {
                    ingress_class_name: ingress.ingress_class_name.clone(),
                    rules: Some(vec![self.generate_ingress_rule(host.clone())]),
                    tls: Some(vec![IngressTLS {
                        hosts: Some(tls_hosts),
                        secret_name: Some(
                            ingress
                                .tls_secret_name
                                .clone()
                                .unwrap_or_else(|| format!("{}-region-tls", self.name_any())),
                        ),
                    }]),
                    ..IngressSpec::default()
                }),
                ..Ingress::default()
            }
        })
    }

    #[inline]
    fn generate_region_ingress_name(&self) -> Option<String> {
        self.spec
            .region_ingress
            .as_ref()
            .map(|ingress| format!("{}-{}", self.name_any(), ingress.region))
    }
}

impl Kanidm {
    fn generate_ingress_rule(&self, host: String) -> IngressRule {
        IngressRule {
            host: Some(host),
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
                    path_type: "Prefix".to_string(),
                }],
            }),
        }
    }
}
