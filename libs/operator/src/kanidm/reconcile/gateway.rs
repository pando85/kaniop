use crate::kanidm::crd::Kanidm;

use gateway_api::apis::standard::backendtlspolicies::{
    BackendTLSPolicy, BackendTlsPolicySpec, BackendTlsPolicyTargetRefs, BackendTlsPolicyValidation,
    BackendTlsPolicyValidationCaCertificateRefs,
};
use gateway_api::apis::standard::httproutes::{
    HTTPRoute, HttpRouteParentRefs, HttpRouteRules, HttpRouteRulesBackendRefs, HttpRouteSpec,
};
use kube::api::ObjectMeta;
use kube::{Resource, ResourceExt};

pub trait GatewayExt {
    fn create_http_route(&self) -> Option<HTTPRoute>;
    fn create_backend_tls_policy(&self) -> Option<BackendTLSPolicy>;
}

impl GatewayExt for Kanidm {
    fn create_http_route(&self) -> Option<HTTPRoute> {
        self.spec.gateway.as_ref().map(|gateway| {
            let labels = self.generate_labels();
            let hostnames = gateway
                .hostnames
                .clone()
                .or_else(|| Some(vec![self.spec.domain.clone()]));

            let parent_refs: Vec<HttpRouteParentRefs> = gateway
                .parent_refs
                .iter()
                .map(|parent_ref| HttpRouteParentRefs {
                    group: parent_ref
                        .group
                        .clone()
                        .or_else(|| Some("gateway.networking.k8s.io".to_string())),
                    kind: parent_ref
                        .kind
                        .clone()
                        .or_else(|| Some("Gateway".to_string())),
                    name: parent_ref.name.clone(),
                    namespace: parent_ref.namespace.clone(),
                    port: parent_ref.port.map(|p| p as i32),
                    section_name: parent_ref.section_name.clone(),
                })
                .collect();

            let rules = gateway.rules.clone().or_else(|| {
                Some(vec![HttpRouteRules {
                    backend_refs: Some(vec![HttpRouteRulesBackendRefs {
                        group: Some("".to_string()),
                        kind: Some("Service".to_string()),
                        name: self.name_any(),
                        namespace: None,
                        port: Some(8443),
                        weight: Some(1),
                        filters: None,
                    }]),
                    ..HttpRouteRules::default()
                }])
            });

            HTTPRoute {
                metadata: ObjectMeta {
                    name: Some(self.name_any()),
                    namespace: Some(self.namespace().unwrap()),
                    labels: Some(labels),
                    annotations: gateway.annotations.clone(),
                    owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                    ..ObjectMeta::default()
                },
                spec: HttpRouteSpec {
                    hostnames,
                    parent_refs: Some(parent_refs),
                    rules,
                },
                status: None,
            }
        })
    }

    fn create_backend_tls_policy(&self) -> Option<BackendTLSPolicy> {
        self.spec.gateway.as_ref().and_then(|gateway| {
            gateway.backend_tls_policy.as_ref().map(|tls_policy| {
                let labels = self.generate_labels();
                let hostname = tls_policy
                    .validation
                    .hostname
                    .clone()
                    .unwrap_or_else(|| self.spec.domain.clone());

                let ca_certificate_refs =
                    tls_policy
                        .validation
                        .ca_certificate_refs
                        .as_ref()
                        .map(|refs| {
                            refs.iter()
                                .map(|ref_| BackendTlsPolicyValidationCaCertificateRefs {
                                    group: ref_.group.clone().unwrap_or_else(|| "".to_string()),
                                    kind: ref_.kind.clone().unwrap_or_else(|| "Secret".to_string()),
                                    name: ref_.name.clone(),
                                })
                                .collect()
                        });

                BackendTLSPolicy {
                    metadata: ObjectMeta {
                        name: Some(self.name_any()),
                        namespace: Some(self.namespace().unwrap()),
                        labels: Some(labels),
                        annotations: tls_policy.annotations.clone(),
                        owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                        ..ObjectMeta::default()
                    },
                    spec: BackendTlsPolicySpec {
                        options: None,
                        target_refs: vec![BackendTlsPolicyTargetRefs {
                            group: "".to_string(),
                            kind: "Service".to_string(),
                            name: self.name_any(),
                            section_name: Some("https".to_string()),
                        }],
                        validation: BackendTlsPolicyValidation {
                            hostname,
                            well_known_ca_certificates: tls_policy
                                .validation
                                .well_known_ca_certificates
                                .clone(),
                            ca_certificate_refs,
                            subject_alt_names: None,
                        },
                    },
                    status: None,
                }
            })
        })
    }
}
