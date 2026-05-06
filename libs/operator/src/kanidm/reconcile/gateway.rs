use crate::kanidm::crd::Kanidm;

use gateway_api::apis::standard::httproutes::{
    HTTPRoute, HttpRouteParentRefs, HttpRouteRules, HttpRouteRulesBackendRefs, HttpRouteSpec,
};
use kube::api::ObjectMeta;
use kube::{Resource, ResourceExt};

pub trait GatewayExt {
    fn create_http_route(&self) -> Option<HTTPRoute>;
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
                    group: Some("gateway.networking.k8s.io".to_string()),
                    kind: Some("Gateway".to_string()),
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
}
