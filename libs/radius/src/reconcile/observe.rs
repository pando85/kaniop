//! Observe the world the token planner decides on.
//!
//! [`observe`] reads Kanidm (tokens on the dedicated account) and the
//! Kubernetes API (Deployment template, ReplicaSets, pods, owned config
//! Secrets) with live list calls, never the controller cache, and returns a
//! [`TokenWorld`]. Everything that interprets objects is a pure function over
//! `k8s_openapi` values so it can be tested from fixtures.
//!
//! The in-use scan is the safety net for revocation: missing a consumer
//! revokes a live token. It therefore errs toward counting:
//!
//! - Pods are scanned across the namespace, not by label. A pod counts when
//!   it is not Succeeded/Failed (Terminating pods count) and carries the
//!   token-id annotation, carries our UID label, or mounts a config Secret we
//!   identify as ours.
//! - A mounted Secret is ours when it is an owned Secret, when its name has
//!   the content-addressed shape `<cr>-<10 hex>` (so a Secret that was
//!   already deleted is still recognised), or when a pod with our UID label
//!   projects its `radius.toml` key. Every such name is reported in
//!   [`Consumer::config_secrets`] even if the Secret no longer exists, so the
//!   planner sees an unresolved reference instead of nothing.
//! - ReplicaSets count while they have replicas, found through the
//!   Deployment owner reference or our UID label.
//! - Annotation values are passed to the planner raw.

use std::collections::BTreeSet;

use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::{Container, Pod, PodSpec, Secret};
use kanidm_client::KanidmClient;
use kanidm_proto::internal::ApiToken;
use kaniop_operator::metrics::ControllerMetrics;
use kube::api::ListParams;
use kube::{Api, Client, Resource, ResourceExt};
use serde::de::DeserializeOwned;
use std::fmt::Debug;

use crate::crd::KanidmRadiusServer;
use crate::reconcile::apply::{check_not_foreign, is_controlled_by};
use crate::reconcile::error::{ExecError, ExecResult, kube_error};
use crate::reconcile::identity::RadiusIdentity;
use crate::reconcile::kanidm_token::{Bearer, list_tokens};
use crate::reconcile::meta::{
    ANNOTATION_ROTATE_TOKEN, ANNOTATION_ROTATION_REQUEST, ANNOTATION_TOKEN_ID, LABEL_UID,
    SECRET_KEY_CONFIG, SECRET_KEY_TOKEN,
};
use crate::reconcile::token::{
    Consumer, ConsumerKind, ObservedSecret, ObservedToken, TemplateRef, TokenWorld,
};
use crate::reconcile::workload::CONFIG_HASH_LEN;

/// List page size for namespace-wide lists.
const LIST_PAGE_SIZE: u32 = 500;

/// The planner input plus the bearer the current template runs with.
///
/// The bearer is needed to re-render the config for a config-only change
/// without minting; it is wrapped in [`Bearer`] so this struct cannot leak it
/// through `Debug`.
#[derive(Debug)]
pub struct Observation {
    pub world: TokenWorld,
    /// Bearer from the template's config Secret, if readable.
    pub template_bearer: Option<Bearer>,
}

/// Names of config Secrets that are ours.
#[derive(Clone, Debug, Default)]
pub struct SecretScope {
    cr_name: String,
    owned: BTreeSet<String>,
}

impl SecretScope {
    pub fn new(cr_name: &str, owned: impl IntoIterator<Item = String>) -> Self {
        Self {
            cr_name: cr_name.to_string(),
            owned: owned.into_iter().collect(),
        }
    }

    /// An owned Secret, or a name shaped like our content-addressed config
    /// Secret (`<cr>-<CONFIG_HASH_LEN lowercase hex>`), which also catches a
    /// Secret that has been deleted since a pod mounted it.
    pub fn is_ours(&self, name: &str) -> bool {
        if self.owned.contains(name) {
            return true;
        }
        name.strip_prefix(self.cr_name.as_str())
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|hash| {
                hash.len() == CONFIG_HASH_LEN
                    && hash
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
    }
}

/// Tokens on the account as planner input.
pub fn observed_tokens(tokens: &[ApiToken]) -> Vec<ObservedToken> {
    tokens
        .iter()
        .map(|t| ObservedToken {
            id: t.token_id,
            issued_at: t.issued_at,
            expiry: t.expiry,
        })
        .collect()
}

/// Keep only Secrets controlled by this CR. The UID label alone is not
/// ownership: a foreign Secret carrying our label is ignored.
pub fn owned_secrets(
    secrets: Vec<Secret>,
    owner: &k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> Vec<Secret> {
    secrets
        .into_iter()
        .filter(|s| is_controlled_by(&s.metadata, owner))
        .collect()
}

fn annotation(
    meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    key: &str,
) -> Option<String> {
    meta.annotations.as_ref().and_then(|a| a.get(key)).cloned()
}

/// The bearer stored in a config Secret, if present and non-empty UTF-8.
pub fn secret_bearer(secret: &Secret) -> Option<Bearer> {
    let bytes = &secret.data.as_ref()?.get(SECRET_KEY_TOKEN)?.0;
    let text = std::str::from_utf8(bytes).ok()?.trim();
    (!text.is_empty()).then(|| Bearer::new(text.to_string()))
}

/// An owned config Secret as planner input.
pub fn observed_secret(secret: &Secret) -> ObservedSecret {
    ObservedSecret {
        name: secret.name_any(),
        token_id: annotation(&secret.metadata, ANNOTATION_TOKEN_ID),
        rotation_request: annotation(&secret.metadata, ANNOTATION_ROTATION_REQUEST),
        bearer_readable: secret_bearer(secret).is_some(),
    }
}

/// Secret names a pod spec refers to, with whether the reference is a
/// projection of the config key (`radius.toml`).
fn mounted_secrets(spec: &PodSpec) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for volume in spec.volumes.iter().flatten() {
        if let Some(secret) = &volume.secret {
            if let Some(name) = &secret.secret_name {
                let has_config = secret
                    .items
                    .iter()
                    .flatten()
                    .any(|item| item.key == SECRET_KEY_CONFIG);
                found.push((name.clone(), has_config));
            }
        }
        for source in volume
            .projected
            .iter()
            .flat_map(|p| p.sources.iter().flatten())
        {
            if let Some(secret) = &source.secret {
                let has_config = secret
                    .items
                    .iter()
                    .flatten()
                    .any(|item| item.key == SECRET_KEY_CONFIG);
                found.push((secret.name.clone(), has_config));
            }
        }
    }
    let containers = spec
        .containers
        .iter()
        .chain(spec.init_containers.iter().flatten());
    for container in containers {
        found.extend(env_secrets(container));
    }
    found
}

fn env_secrets(container: &Container) -> Vec<(String, bool)> {
    let from_env = container
        .env
        .iter()
        .flatten()
        .filter_map(|e| e.value_from.as_ref()?.secret_key_ref.as_ref())
        .map(|r| (r.name.clone(), r.key == SECRET_KEY_CONFIG));
    let from_env_from = container
        .env_from
        .iter()
        .flatten()
        .filter_map(|e| e.secret_ref.as_ref())
        .map(|r| (r.name.clone(), false));
    from_env.chain(from_env_from).collect()
}

/// Config Secrets of ours that `spec` mounts, sorted and unique.
///
/// With `trust_layout` (objects carrying our UID label or the Deployment
/// template), any Secret whose `radius.toml` key is projected also counts.
pub fn config_secrets(
    spec: Option<&PodSpec>,
    scope: &SecretScope,
    trust_layout: bool,
) -> Vec<String> {
    let names: BTreeSet<String> = spec
        .map(mounted_secrets)
        .into_iter()
        .flatten()
        .filter(|(name, has_config)| scope.is_ours(name) || (trust_layout && *has_config))
        .map(|(name, _)| name)
        .collect();
    names.into_iter().collect()
}

/// What the Deployment pod template points at.
///
/// The template's config Secret is the one it mounts; if the template names
/// several or none, `config_secret` is `None` and the planner mints.
pub fn template_ref(deployment: &Deployment, scope: &SecretScope) -> TemplateRef {
    let template = deployment.spec.as_ref().map(|s| &s.template);
    let mut secrets = config_secrets(template.and_then(|t| t.spec.as_ref()), scope, true);
    TemplateRef {
        token_id: template
            .and_then(|t| t.metadata.as_ref())
            .and_then(|m| annotation(m, ANNOTATION_TOKEN_ID)),
        config_secret: if secrets.len() == 1 {
            secrets.pop()
        } else {
            None
        },
    }
}

fn has_our_uid_label(
    meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    uid: &str,
) -> bool {
    meta.labels
        .as_ref()
        .and_then(|l| l.get(LABEL_UID))
        .is_some_and(|v| v == uid)
}

/// Is this ReplicaSet ours: controlled by our Deployment, or labelled with
/// our UID (the Deployment template label, so it also finds ReplicaSets whose
/// Deployment is already gone)?
pub fn is_our_replicaset(rs: &ReplicaSet, deployment_uid: Option<&str>, cr_uid: &str) -> bool {
    let by_owner = deployment_uid.is_some_and(|uid| {
        rs.metadata
            .owner_references
            .iter()
            .flatten()
            .any(|r| r.controller == Some(true) && r.kind == "Deployment" && r.uid == uid)
    });
    by_owner || has_our_uid_label(&rs.metadata, cr_uid)
}

/// A ReplicaSet counts only while it wants or has replicas.
pub fn replicaset_has_replicas(rs: &ReplicaSet) -> bool {
    let wanted = rs.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
    let current = rs.status.as_ref().map_or(0, |s| s.replicas);
    wanted > 0 || current > 0
}

/// Consumer for one of our ReplicaSets, or `None` when it has no replicas.
pub fn replicaset_consumer(rs: &ReplicaSet, scope: &SecretScope) -> Option<Consumer> {
    if !replicaset_has_replicas(rs) {
        return None;
    }
    let template = rs.spec.as_ref().and_then(|s| s.template.as_ref());
    Some(Consumer {
        kind: ConsumerKind::ReplicaSet,
        name: rs.name_any(),
        token_id: template
            .and_then(|t| t.metadata.as_ref())
            .and_then(|m| annotation(m, ANNOTATION_TOKEN_ID)),
        config_secrets: config_secrets(template.and_then(|t| t.spec.as_ref()), scope, true),
    })
}

/// Consumer for a pod, or `None` when the pod is finished (Succeeded or
/// Failed) or has nothing to do with us. Terminating pods count: their
/// containers may still authenticate until they exit.
pub fn pod_consumer(pod: &Pod, cr_uid: &str, scope: &SecretScope) -> Option<Consumer> {
    let phase = pod.status.as_ref().and_then(|s| s.phase.as_deref());
    if matches!(phase, Some("Succeeded" | "Failed")) {
        return None;
    }
    let ours = has_our_uid_label(&pod.metadata, cr_uid);
    let token_id = annotation(&pod.metadata, ANNOTATION_TOKEN_ID);
    let secrets = config_secrets(pod.spec.as_ref(), scope, ours);
    if !ours && token_id.is_none() && secrets.is_empty() {
        return None;
    }
    Some(Consumer {
        kind: ConsumerKind::Pod,
        name: pod.name_any(),
        token_id,
        config_secrets: secrets,
    })
}

/// Everything read from the API and Kanidm for one observation.
pub struct ObservedObjects<'a> {
    pub cr_uid: &'a str,
    pub cr_name: &'a str,
    /// Raw `kaniop.rs/radius-rotate-token` value on the CR.
    pub rotate_request: Option<String>,
    pub tokens: &'a [ApiToken],
    /// Config Secrets controlled by this CR (see [`owned_secrets`]).
    pub secrets: &'a [Secret],
    pub deployment: Option<&'a Deployment>,
    pub replicasets: &'a [ReplicaSet],
    pub pods: &'a [Pod],
}

/// Assemble the planner input from already-read objects. Pure.
pub fn build_world(objects: &ObservedObjects<'_>) -> Observation {
    let scope = SecretScope::new(
        objects.cr_name,
        objects.secrets.iter().map(ResourceExt::name_any),
    );
    let template = objects.deployment.map(|d| template_ref(d, &scope));
    let deployment_uid = objects.deployment.and_then(|d| d.metadata.uid.as_deref());

    let replicasets = objects
        .replicasets
        .iter()
        .filter(|rs| is_our_replicaset(rs, deployment_uid, objects.cr_uid))
        .filter_map(|rs| replicaset_consumer(rs, &scope));
    let pods = objects
        .pods
        .iter()
        .filter_map(|pod| pod_consumer(pod, objects.cr_uid, &scope));

    let template_bearer = template
        .as_ref()
        .and_then(|t| t.config_secret.as_deref())
        .and_then(|name| objects.secrets.iter().find(|s| s.name_any() == name))
        .and_then(secret_bearer);

    Observation {
        world: TokenWorld {
            tokens: observed_tokens(objects.tokens),
            secrets: objects.secrets.iter().map(observed_secret).collect(),
            template,
            consumers: replicasets.chain(pods).collect(),
            rotate_request: objects.rotate_request.clone(),
        },
        template_bearer,
    }
}

async fn list_all<K>(
    api: &Api<K>,
    params: ListParams,
    kind: &str,
    namespace: &str,
) -> ExecResult<Vec<K>>
where
    K: Resource + Clone + DeserializeOwned + Debug,
{
    let mut items = Vec::new();
    let mut params = params.limit(LIST_PAGE_SIZE);
    loop {
        let page = api
            .list(&params)
            .await
            .map_err(|e| kube_error("list", kind, namespace, "*", e))?;
        items.extend(page.items);
        match page.metadata.continue_.filter(|token| !token.is_empty()) {
            Some(token) => params = params.continue_token(&token),
            None => return Ok(items),
        }
    }
}

/// Observe Kanidm and Kubernetes for one reconcile.
///
/// Fails with [`ExecError::ForeignObject`] if the Deployment name is held by
/// an object we do not control, before anything is minted. The account must
/// already exist (call `ensure_identity` first).
pub async fn observe(
    client: &KanidmClient,
    metrics: &ControllerMetrics,
    kube: Client,
    radius: &KanidmRadiusServer,
    identity: &RadiusIdentity,
) -> ExecResult<Observation> {
    let namespace = radius.namespace().ok_or(ExecError::MissingMetadata {
        kind: "KanidmRadiusServer",
        field: "namespace",
    })?;
    let owner = radius
        .controller_owner_ref(&())
        .ok_or(ExecError::MissingMetadata {
            kind: "KanidmRadiusServer",
            field: "uid",
        })?;
    let name = radius.name_any();

    let deployments: Api<Deployment> = Api::namespaced(kube.clone(), &namespace);
    let deployment = check_not_foreign(&deployments, &namespace, &name, &owner).await?;

    let tokens = list_tokens(client, metrics, &identity.account_name).await?;

    let secret_api: Api<Secret> = Api::namespaced(kube.clone(), &namespace);
    let selector = ListParams::default().labels(&format!("{LABEL_UID}={}", owner.uid));
    let secrets = owned_secrets(
        list_all(&secret_api, selector, "Secret", &namespace).await?,
        &owner,
    );

    // ReplicaSets before pods: a pod listed after its ReplicaSet cannot be a
    // consumer of a ReplicaSet we missed.
    let rs_api: Api<ReplicaSet> = Api::namespaced(kube.clone(), &namespace);
    let replicasets = list_all(&rs_api, ListParams::default(), "ReplicaSet", &namespace).await?;
    let pod_api: Api<Pod> = Api::namespaced(kube, &namespace);
    let pods = list_all(&pod_api, ListParams::default(), "Pod", &namespace).await?;

    Ok(build_world(&ObservedObjects {
        cr_uid: &owner.uid,
        cr_name: &name,
        rotate_request: annotation(&radius.metadata, ANNOTATION_ROTATE_TOKEN),
        tokens: &tokens,
        secrets: &secrets,
        deployment: deployment.as_ref(),
        replicasets: &replicasets,
        pods: &pods,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::*;
    use crate::reconcile::token::{CurrentToken, TokenPolicy, plan_tokens};

    const CR: &str = "campus";
    const CR_UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";
    const TOKEN_A: &str = "9ad74e68-147f-49e8-96db-dc21c920ff3b";
    const TOKEN_B: &str = "31b30b65-037a-418f-b7ca-0c376da2db28";
    // Content-addressed shapes: `<cr>-<10 hex>`.
    const SECRET_A: &str = "campus-0a1b2c3d4e";
    const SECRET_B: &str = "campus-f0e1d2c3b4";

    fn scope() -> SecretScope {
        SecretScope::new(CR, [SECRET_A.to_string(), SECRET_B.to_string()])
    }

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    fn projected_volume(secret: &str) -> Value {
        json!({"name": "radius-data", "projected": {"sources": [
            {"secret": {"name": secret, "items": [{"key": "radius.toml", "path": "radius.toml"}]}},
            {"secret": {"name": "radius-tls", "items": [{"key": "tls.crt", "path": "cert.pem"}]}}
        ]}})
    }

    fn pod_json(
        name: &str,
        phase: Option<&str>,
        volumes: Vec<Value>,
        annotations: Value,
        labels: Value,
    ) -> Value {
        let mut status = json!({});
        if let Some(phase) = phase {
            status = json!({"phase": phase});
        }
        json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": "network", "annotations": annotations, "labels": labels},
            "spec": {"containers": [{"name": "kanidm-radius"}], "volumes": volumes},
            "status": status
        })
    }

    fn pod(value: Value) -> Pod {
        serde_json::from_value(value).unwrap()
    }

    fn our_pod(name: &str, secret: &str, token: &str) -> Pod {
        pod(pod_json(
            name,
            Some("Running"),
            vec![projected_volume(secret)],
            json!({"kaniop.rs/radius-token-id": token}),
            json!({"kaniop.rs/radius-uid": CR_UID}),
        ))
    }

    #[test]
    fn pod_matches_by_projected_volume_and_annotation() {
        let c = pod_consumer(&our_pod("p1", SECRET_A, TOKEN_A), CR_UID, &scope()).unwrap();
        assert_eq!(c.kind, ConsumerKind::Pod);
        assert_eq!(c.name, "p1");
        assert_eq!(c.token_id.as_deref(), Some(TOKEN_A));
        // Only our config Secret; the user's TLS Secret is not ours.
        assert_eq!(c.config_secrets, vec![SECRET_A.to_string()]);
    }

    #[test]
    fn pod_matches_by_plain_secret_volume_without_annotation() {
        let p = pod(pod_json(
            "p2",
            Some("Running"),
            vec![json!({"name": "v", "secret": {"secretName": SECRET_B}})],
            json!({}),
            json!({}),
        ));
        let c = pod_consumer(&p, CR_UID, &scope()).unwrap();
        assert_eq!(c.config_secrets, vec![SECRET_B.to_string()]);
        assert_eq!(c.token_id, None);
    }

    #[test]
    fn pod_matches_by_env_secret_reference() {
        let p = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "p-env", "namespace": "network"},
            "spec": {"containers": [{"name": "c", "envFrom": [{"secretRef": {"name": SECRET_A}}]}]},
            "status": {"phase": "Running"}
        }));
        let c = pod_consumer(&p, CR_UID, &scope()).unwrap();
        assert_eq!(c.config_secrets, vec![SECRET_A.to_string()]);
    }

    #[test]
    fn pod_matches_by_annotation_alone_and_passes_it_raw() {
        let p = pod(pod_json(
            "p3",
            Some("Pending"),
            vec![],
            json!({"kaniop.rs/radius-token-id": "not-a-uuid"}),
            json!({}),
        ));
        let c = pod_consumer(&p, CR_UID, &scope()).unwrap();
        assert_eq!(c.token_id.as_deref(), Some("not-a-uuid"));
        assert!(c.config_secrets.is_empty());
    }

    #[test]
    fn terminating_pod_is_counted() {
        let mut value = pod_json(
            "p4",
            Some("Running"),
            vec![projected_volume(SECRET_A)],
            json!({"kaniop.rs/radius-token-id": TOKEN_A}),
            json!({"kaniop.rs/radius-uid": CR_UID}),
        );
        value["metadata"]["deletionTimestamp"] = json!("2026-11-01T10:00:00Z");
        let c = pod_consumer(&pod(value), CR_UID, &scope());
        assert_eq!(c.unwrap().token_id.as_deref(), Some(TOKEN_A));
    }

    #[test]
    fn succeeded_and_failed_pods_are_ignored() {
        for phase in ["Succeeded", "Failed"] {
            let p = pod(pod_json(
                "done",
                Some(phase),
                vec![projected_volume(SECRET_A)],
                json!({"kaniop.rs/radius-token-id": TOKEN_A}),
                json!({"kaniop.rs/radius-uid": CR_UID}),
            ));
            assert_eq!(pod_consumer(&p, CR_UID, &scope()), None, "{phase}");
        }
        // Unknown and missing phases are not finished.
        for phase in [Some("Unknown"), None] {
            let p = pod(pod_json(
                "live",
                phase,
                vec![],
                json!({"kaniop.rs/radius-token-id": TOKEN_A}),
                json!({}),
            ));
            assert!(pod_consumer(&p, CR_UID, &scope()).is_some(), "{phase:?}");
        }
    }

    #[test]
    fn unrelated_pod_is_ignored() {
        let p = pod(pod_json(
            "web",
            Some("Running"),
            vec![json!({"name": "v", "secret": {"secretName": "radius-tls"}})],
            json!({}),
            json!({"app": "web"}),
        ));
        assert_eq!(pod_consumer(&p, CR_UID, &scope()), None);
    }

    #[test]
    fn pod_mounting_a_deleted_owned_secret_is_still_reported() {
        // The Secret is not in the owned set (it was deleted), but its name
        // has our content-addressed shape.
        let scope = SecretScope::new(CR, [SECRET_B.to_string()]);
        let c = pod_consumer(&our_pod("p5", SECRET_A, TOKEN_A), CR_UID, &scope).unwrap();
        assert_eq!(c.config_secrets, vec![SECRET_A.to_string()]);
    }

    #[test]
    fn labelled_pod_reports_its_projected_config_secret_whatever_the_name() {
        let c = pod_consumer(
            &our_pod("p6", "legacy-generation-name", TOKEN_A),
            CR_UID,
            &scope(),
        )
        .unwrap();
        assert_eq!(c.config_secrets, vec!["legacy-generation-name".to_string()]);
        // An unlabelled pod doing the same is not claimed.
        let p = pod(pod_json(
            "stranger",
            Some("Running"),
            vec![projected_volume("legacy-generation-name")],
            json!({}),
            json!({}),
        ));
        assert_eq!(pod_consumer(&p, CR_UID, &scope()), None);
    }

    #[test]
    fn labelled_pod_without_any_reference_is_reported_empty() {
        let p = pod(pod_json(
            "p7",
            Some("Running"),
            vec![],
            json!({}),
            json!({"kaniop.rs/radius-uid": CR_UID}),
        ));
        let c = pod_consumer(&p, CR_UID, &scope()).unwrap();
        assert_eq!(c.token_id, None);
        assert!(c.config_secrets.is_empty());
    }

    #[test]
    fn secret_scope_recognises_only_content_addressed_names() {
        let scope = SecretScope::new(CR, []);
        assert!(scope.is_ours("campus-0a1b2c3d4e"));
        assert!(!scope.is_ours("campus-0A1B2C3D4E"), "uppercase");
        assert!(!scope.is_ours("campus-0a1b2c3d4"), "short");
        assert!(!scope.is_ours("campus-0a1b2c3d4ef"), "long");
        assert!(!scope.is_ours("campus-tls"));
        assert!(!scope.is_ours("other-0a1b2c3d4e"));
    }

    fn rs_json(
        name: &str,
        spec_replicas: Option<i32>,
        status_replicas: i32,
        owner_uid: Option<&str>,
        labelled: bool,
    ) -> Value {
        let mut spec = json!({
            "selector": {"matchLabels": {"a": "b"}},
            "template": {
                "metadata": {"annotations": {"kaniop.rs/radius-token-id": TOKEN_A}},
                "spec": {"containers": [{"name": "c"}], "volumes": [projected_volume(SECRET_A)]}
            }
        });
        if let Some(r) = spec_replicas {
            spec["replicas"] = json!(r);
        }
        let mut meta = json!({"name": name, "namespace": "network"});
        if let Some(uid) = owner_uid {
            meta["ownerReferences"] = json!([{
                "apiVersion": "apps/v1", "kind": "Deployment", "name": CR,
                "uid": uid, "controller": true
            }]);
        }
        if labelled {
            meta["labels"] = json!({"kaniop.rs/radius-uid": CR_UID});
        }
        json!({"apiVersion": "apps/v1", "kind": "ReplicaSet", "metadata": meta, "spec": spec,
               "status": {"replicas": status_replicas}})
    }

    fn rs(value: Value) -> ReplicaSet {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn replicaset_with_zero_replicas_is_ignored() {
        let zero = rs(rs_json("old", Some(0), 0, Some("dep-uid"), true));
        assert_eq!(replicaset_consumer(&zero, &scope()), None);
        // Still draining pods: counted until status reaches zero too.
        let draining = rs(rs_json("old", Some(0), 1, Some("dep-uid"), true));
        assert!(replicaset_consumer(&draining, &scope()).is_some());
        let scaled = rs(rs_json("new", Some(2), 2, Some("dep-uid"), true));
        let c = replicaset_consumer(&scaled, &scope()).unwrap();
        assert_eq!(c.kind, ConsumerKind::ReplicaSet);
        assert_eq!(c.token_id.as_deref(), Some(TOKEN_A));
        assert_eq!(c.config_secrets, vec![SECRET_A.to_string()]);
        // replicas omitted defaults to 1.
        let defaulted = rs(rs_json("dflt", None, 0, Some("dep-uid"), true));
        assert!(replicaset_consumer(&defaulted, &scope()).is_some());
    }

    #[test]
    fn replicaset_ownership_by_deployment_uid_or_label() {
        let by_owner = rs(rs_json("a", Some(1), 1, Some("dep-uid"), false));
        assert!(is_our_replicaset(&by_owner, Some("dep-uid"), CR_UID));
        assert!(!is_our_replicaset(&by_owner, Some("other-dep"), CR_UID));
        assert!(!is_our_replicaset(&by_owner, None, CR_UID));
        // Deployment gone: the template label still identifies it.
        let by_label = rs(rs_json("b", Some(1), 1, Some("dep-uid"), true));
        assert!(is_our_replicaset(&by_label, None, CR_UID));
        let stranger = rs(rs_json("c", Some(1), 1, None, false));
        assert!(!is_our_replicaset(&stranger, Some("dep-uid"), CR_UID));
    }

    fn secret_json(
        name: &str,
        token: Option<&str>,
        bearer: Option<&str>,
        owner_uid: Option<&str>,
    ) -> Secret {
        let mut value = json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": name, "namespace": "network",
                         "labels": {"kaniop.rs/radius-uid": CR_UID}},
            "data": {}
        });
        if let Some(t) = token {
            value["metadata"]["annotations"] =
                json!({"kaniop.rs/radius-token-id": t, "kaniop.rs/radius-rotation-request": "r1"});
        }
        if let Some(uid) = owner_uid {
            value["metadata"]["ownerReferences"] = json!([{
                "apiVersion": "kaniop.rs/v1beta1", "kind": "KanidmRadiusServer",
                "name": CR, "uid": uid, "controller": true
            }]);
        }
        let mut secret: Secret = serde_json::from_value(value).unwrap();
        if let Some(b) = bearer {
            secret.data = Some(std::collections::BTreeMap::from([(
                "auth_token".to_string(),
                k8s_openapi::ByteString(b.as_bytes().to_vec()),
            )]));
        }
        secret
    }

    #[test]
    fn only_controlled_secrets_are_ours() {
        let owner = k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
            api_version: "kaniop.rs/v1beta1".into(),
            kind: "KanidmRadiusServer".into(),
            name: CR.into(),
            uid: CR_UID.into(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        };
        let kept = owned_secrets(
            vec![
                secret_json(SECRET_A, Some(TOKEN_A), Some("b"), Some(CR_UID)),
                // Carries our label but is controlled by someone else.
                secret_json("spoof", Some(TOKEN_A), Some("b"), Some("other-uid")),
                secret_json("ownerless", None, None, None),
            ],
            &owner,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name_any(), SECRET_A);
    }

    #[test]
    fn observed_secret_reports_annotations_raw_and_bearer_readability() {
        let s = observed_secret(&secret_json(
            SECRET_A,
            Some("garbage"),
            Some("tok"),
            Some(CR_UID),
        ));
        assert_eq!(s.token_id.as_deref(), Some("garbage"));
        assert_eq!(s.rotation_request.as_deref(), Some("r1"));
        assert!(s.bearer_readable);
        assert!(
            !observed_secret(&secret_json(SECRET_A, Some(TOKEN_A), None, Some(CR_UID)))
                .bearer_readable
        );
        assert!(
            !observed_secret(&secret_json(
                SECRET_A,
                Some(TOKEN_A),
                Some("  "),
                Some(CR_UID)
            ))
            .bearer_readable
        );
    }

    #[test]
    fn bearer_debug_never_shows_the_token() {
        let observation = Observation {
            world: TokenWorld::default(),
            template_bearer: Some(Bearer::new("super-secret-bearer".into())),
        };
        assert!(!format!("{observation:?}").contains("super-secret-bearer"));
    }

    fn deployment_json(token: &str, secret: &str, uid: &str) -> Deployment {
        serde_json::from_value(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": CR, "namespace": "network", "uid": uid},
            "spec": {
                "selector": {"matchLabels": {"a": "b"}},
                "template": {
                    "metadata": {"annotations": {"kaniop.rs/radius-token-id": token}},
                    "spec": {"containers": [{"name": "c"}], "volumes": [projected_volume(secret)]}
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn template_ref_reads_annotation_and_mounted_config_secret() {
        let t = template_ref(&deployment_json(TOKEN_B, SECRET_B, "dep-uid"), &scope());
        assert_eq!(t.token_id.as_deref(), Some(TOKEN_B));
        assert_eq!(t.config_secret.as_deref(), Some(SECRET_B));
    }

    fn api_token(id: &str, age: Duration, now: OffsetDateTime) -> ApiToken {
        serde_json::from_value(json!({
            "account_id": "00000000-0000-0000-0000-000000000001",
            "token_id": id,
            "label": "kaniop-radius-x",
            "expiry": null,
            "issued_at": (now - age).unix_timestamp(),
            "purpose": "readonly"
        }))
        .unwrap()
    }

    /// Rollout in progress: the Deployment already points at token B, one
    /// old pod still runs token A. A must survive until it is gone, then be
    /// revoked, and B stays current throughout.
    #[test]
    fn observe_then_plan_keeps_old_token_during_rollout() {
        let now = OffsetDateTime::now_utc();
        let tokens = [
            api_token(TOKEN_A, Duration::days(10), now),
            api_token(TOKEN_B, Duration::hours(1), now),
        ];
        let secrets = [
            secret_json(SECRET_A, Some(TOKEN_A), Some("bearer-a"), Some(CR_UID)),
            secret_json(SECRET_B, Some(TOKEN_B), Some("bearer-b"), Some(CR_UID)),
        ];
        let deployment = deployment_json(TOKEN_B, SECRET_B, "dep-uid");
        let replicasets = [rs(rs_json("rs-old", Some(1), 1, Some("dep-uid"), true)), {
            let mut new = rs_json("rs-new", Some(1), 1, Some("dep-uid"), true);
            new["spec"]["template"]["metadata"]["annotations"]["kaniop.rs/radius-token-id"] =
                json!(TOKEN_B);
            new["spec"]["template"]["spec"]["volumes"] = json!([projected_volume(SECRET_B)]);
            rs(new)
        }];
        let mut pods = vec![
            our_pod("pod-old", SECRET_A, TOKEN_A),
            our_pod("pod-new", SECRET_B, TOKEN_B),
        ];
        let policy = TokenPolicy::from_spec(None);
        let observe_and_plan = |replicasets: &[ReplicaSet], pods: &[Pod]| {
            let observation = build_world(&ObservedObjects {
                cr_uid: CR_UID,
                cr_name: CR,
                rotate_request: None,
                tokens: &tokens,
                secrets: &secrets,
                deployment: Some(&deployment),
                replicasets,
                pods,
            });
            let plan = plan_tokens(&observation.world, &policy, now);
            (observation, plan)
        };

        let (observation, plan) = observe_and_plan(&replicasets, &pods);
        assert_eq!(plan.current, CurrentToken::Keep(uuid(TOKEN_B)));
        assert!(plan.revoke.is_empty(), "old token still used: {plan:?}");
        assert_eq!(plan.retiring, vec![uuid(TOKEN_A)]);
        assert!(plan.gc_secrets.is_empty());
        assert_eq!(observation.template_bearer.unwrap().expose(), "bearer-b");

        // Old ReplicaSet scaled to zero, but its pod is still Terminating.
        let mut drained = replicasets.clone();
        drained[0] = rs(rs_json("rs-old", Some(0), 0, Some("dep-uid"), true));
        let mut terminating = serde_json::to_value(&pods[0]).unwrap();
        terminating["metadata"]["deletionTimestamp"] = json!("2026-11-01T10:00:00Z");
        pods[0] = pod(terminating);
        let (_, plan) = observe_and_plan(&drained, &pods);
        assert!(plan.revoke.is_empty(), "terminating pod holds A: {plan:?}");

        // Pod gone: A is revoked, its Secret collected, B untouched.
        pods.remove(0);
        let (_, plan) = observe_and_plan(&drained, &pods);
        assert_eq!(plan.current, CurrentToken::Keep(uuid(TOKEN_B)));
        assert_eq!(plan.revoke, vec![uuid(TOKEN_A)]);
        assert_eq!(plan.gc_secrets, vec![SECRET_A.to_string()]);
    }

    /// A pod mounting an owned Secret that has been deleted cannot be
    /// resolved to a token, so nothing is revoked until it is gone.
    #[test]
    fn pod_on_deleted_secret_blocks_revocation() {
        let now = OffsetDateTime::now_utc();
        let tokens = [
            api_token(TOKEN_A, Duration::days(10), now),
            api_token(TOKEN_B, Duration::hours(1), now),
        ];
        // SECRET_A was deleted: only B's Secret exists.
        let secrets = [secret_json(
            SECRET_B,
            Some(TOKEN_B),
            Some("bearer-b"),
            Some(CR_UID),
        )];
        let deployment = deployment_json(TOKEN_B, SECRET_B, "dep-uid");
        // The pod carries no annotation, so only the mount names a token.
        let stranded = pod(pod_json(
            "pod-old",
            Some("Running"),
            vec![projected_volume(SECRET_A)],
            json!({}),
            json!({"kaniop.rs/radius-uid": CR_UID}),
        ));
        let observation = build_world(&ObservedObjects {
            cr_uid: CR_UID,
            cr_name: CR,
            rotate_request: None,
            tokens: &tokens,
            secrets: &secrets,
            deployment: Some(&deployment),
            replicasets: &[],
            pods: std::slice::from_ref(&stranded),
        });
        assert_eq!(
            observation.world.consumers[0].config_secrets,
            vec![SECRET_A.to_string()]
        );
        let plan = plan_tokens(&observation.world, &TokenPolicy::from_spec(None), now);
        assert!(plan.revoke.is_empty(), "{plan:?}");
        assert_eq!(plan.unresolved_consumers, vec!["pod/pod-old".to_string()]);
    }

    #[test]
    fn status_loss_and_no_deployment_mint_initial() {
        let now = OffsetDateTime::now_utc();
        let observation = build_world(&ObservedObjects {
            cr_uid: CR_UID,
            cr_name: CR,
            rotate_request: Some("x".into()),
            tokens: &[],
            secrets: &[],
            deployment: None,
            replicasets: &[],
            pods: &[],
        });
        assert_eq!(observation.world.rotate_request.as_deref(), Some("x"));
        assert!(observation.world.template.is_none());
        let plan = plan_tokens(&observation.world, &TokenPolicy::from_spec(None), now);
        assert!(matches!(plan.current, CurrentToken::Mint(_)));
    }

    fn radius_cr() -> KanidmRadiusServer {
        serde_json::from_value(json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmRadiusServer",
            "metadata": {
                "name": CR, "namespace": "network", "uid": CR_UID,
                "annotations": {"kaniop.rs/radius-rotate-token": "now-please"}
            },
            "spec": {
                "kanidmRef": {"name": "idm"},
                "authorization": {"requiredGroups": ["allowed@idm.example.com"]},
                "clients": [{
                    "name": "nas", "ipaddr": "192.0.2.1",
                    "sharedSecretRef": {"name": "nas-secret", "key": "nas"}
                }],
                "radiusTls": {"secretRef": {"name": "eap-tls"}}
            }
        }))
        .unwrap()
    }

    fn list(kind: &str, items: Vec<Value>, continue_token: Option<&str>) -> (u16, Value) {
        let mut metadata = json!({});
        if let Some(token) = continue_token {
            metadata["continue"] = json!(token);
        }
        (
            200,
            json!({"apiVersion": "v1", "kind": kind, "metadata": metadata, "items": items}),
        )
    }

    #[tokio::test]
    async fn observe_fails_on_a_foreign_deployment_before_touching_kanidm() {
        use crate::reconcile::kanidm_mock;
        use crate::reconcile::kube_mock::{mock_client, to_json};

        let radius = radius_cr();
        let identity = RadiusIdentity::for_resource(&radius).unwrap();
        let mut foreign = deployment_json(TOKEN_A, SECRET_A, "dep-uid");
        foreign.metadata.owner_references = None;
        let (kube, recorder) = mock_client(vec![(200, to_json(&foreign))]);
        let mock = kanidm_mock::start(|_| (500, "{}".into())).await;
        let error = observe(&mock.client, &mock.metrics, kube, &radius, &identity)
            .await
            .unwrap_err();
        assert!(matches!(error, ExecError::ForeignObject { .. }));
        assert!(
            mock.requests().is_empty(),
            "no Kanidm call, so nothing minted"
        );
        assert_eq!(recorder.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn observe_lists_namespace_wide_pages_and_builds_the_world() {
        use crate::reconcile::kanidm_mock;
        use crate::reconcile::kube_mock::{api_status, mock_client, to_json};

        let radius = radius_cr();
        let identity = RadiusIdentity::for_resource(&radius).unwrap();
        let now = OffsetDateTime::now_utc();
        let token_list =
            serde_json::to_string(&[api_token(TOKEN_A, Duration::days(1), now)]).unwrap();
        let mock = kanidm_mock::start(move |_| (200, token_list.clone())).await;

        let mut secret = secret_json(SECRET_A, Some(TOKEN_A), Some("bearer-a"), Some(CR_UID));
        secret.metadata.resource_version = Some("1".into());
        let page2_pod = our_pod("pod-on-page-two", SECRET_A, TOKEN_A);
        let (kube, recorder) = mock_client(vec![
            api_status(404, "NotFound"),
            list("SecretList", vec![to_json(&secret)], None),
            list("ReplicaSetList", vec![], None),
            list(
                "PodList",
                vec![to_json(&our_pod("pod-on-page-one", SECRET_A, TOKEN_A))],
                Some("next-page"),
            ),
            list("PodList", vec![to_json(&page2_pod)], None),
        ]);
        let observation = observe(&mock.client, &mock.metrics, kube, &radius, &identity)
            .await
            .unwrap();

        let world = &observation.world;
        assert_eq!(world.tokens.len(), 1);
        assert_eq!(world.secrets[0].name, SECRET_A);
        assert!(world.template.is_none());
        assert_eq!(world.rotate_request.as_deref(), Some("now-please"));
        let names: Vec<_> = world.consumers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["pod-on-page-one", "pod-on-page-two"]);

        let requests = recorder.await.unwrap();
        let uris: Vec<_> = requests.iter().map(|r| r.uri.as_str()).collect();
        assert!(
            uris[1].contains("/secrets")
                && uris[1].contains("labelSelector=kaniop.rs%2Fradius-uid%3D"),
            "{uris:?}"
        );
        assert!(
            uris[2].contains("/replicasets") && !uris[2].contains("labelSelector"),
            "{uris:?}"
        );
        assert!(
            uris[3].contains("/pods") && !uris[3].contains("labelSelector"),
            "pods are scanned namespace-wide: {uris:?}"
        );
        assert!(uris[3].contains("limit=500"));
        assert!(uris[4].contains("continue=next-page"), "{uris:?}");
    }
}
