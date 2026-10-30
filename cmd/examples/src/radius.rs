use kaniop_operator::{
    crd::KanidmRef,
    kanidm::{
        crd::Kanidm,
        radius_crd::{
            KanidmRadiusServer, KanidmRadiusServerSpec, KanidmTlsSpec, RadiusAuthorizationSpec,
            RadiusCaBundleRef,
            RadiusClientSpec, RadiusGroupVlan, RadiusSecretKeyRef, RadiusSecretRef,
            RadiusServiceSpec, RadiusTlsSpec, RadiusTokenRotationSpec,
        },
    },
};
use kube::{ResourceExt, api::ObjectMeta};

pub fn example(kanidm: &Kanidm) -> KanidmRadiusServer {
    KanidmRadiusServer {
        metadata: ObjectMeta {
            name: Some("campus-radius".into()),
            namespace: kanidm.namespace(),
            ..Default::default()
        },
        spec: KanidmRadiusServerSpec {
            kanidm_ref: KanidmRef {
                name: kanidm.name_any(),
                namespace: kanidm.namespace(),
            },
            image: Some("kanidm/radius:1.11.2".into()),
            replicas: 2,
            authorization: RadiusAuthorizationSpec {
                required_groups: vec![format!("radius_access_allowed@{}", kanidm.spec.domain)],
                default_vlan: 10,
                group_vlans: vec![RadiusGroupVlan {
                    group: format!("staff@{}", kanidm.spec.domain),
                    vlan: 20,
                }],
            },
            clients: vec![RadiusClientSpec {
                name: "mikrotik".into(),
                ipaddr: "192.0.2.10/32".into(),
                shared_secret_ref: RadiusSecretKeyRef {
                    name: "nas-radius-secrets".into(),
                    key: "mikrotik".into(),
                },
            }],
            radius_tls: RadiusTlsSpec {
                secret_ref: RadiusSecretRef {
                    name: "radius-eap-tls".into(),
                },
                cert_key: "tls.crt".into(),
                private_key_key: "tls.key".into(),
                ca_key: Some("ca.crt".into()),
            },
            kanidm_tls: Some(KanidmTlsSpec {
                ca_bundle_ref: Some(RadiusCaBundleRef {
                    kind: Default::default(),
                    name: "kanidm-ca".into(),
                    key: "ca.crt".into(),
                }),
            }),
            service: RadiusServiceSpec::default(),
            token_rotation: RadiusTokenRotationSpec {
                enabled: false,
                period_days: 30,
            },
            resources: None,
            node_selector: None,
            affinity: None,
            tolerations: None,
            topology_spread_constraints: None,
        },
        status: Default::default(),
    }
}
