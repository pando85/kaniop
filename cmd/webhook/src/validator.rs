use crate::state::WebhookState;

use kaniop_operator::controller::kanidm::{KanidmClients, KanidmKey, KanidmUser};
use kaniop_operator::crd::KanidmRef;
use kaniop_operator::kanidm::crd::Kanidm;

use kube::runtime::reflector::Store;
use kube::{Api, Resource, ResourceExt};

#[derive(Clone, Copy, Debug)]
pub enum KanidmEntityKind {
    Group,
    Person,
    OAuth2Client,
    ServiceAccount,
}

/// Normalize KanidmRef by filling in missing namespace with object's namespace
pub fn normalize_kanidm_ref(kanidm_ref: &KanidmRef, object_namespace: &str) -> (String, String) {
    let ref_name = kanidm_ref.name.clone();
    let ref_namespace = kanidm_ref
        .namespace
        .clone()
        .unwrap_or_else(|| object_namespace.to_string());
    (ref_name, ref_namespace)
}

/// Generic duplicate checker for any resource type
/// Checks for duplicates based on kanidmRef AND the effective Kanidm entity name (kanidmName or K8s name)
pub fn check_duplicate<T>(object: &T, object_name: &str, store: &Store<T>) -> Result<(), String>
where
    T: Resource + ResourceExt + Clone + HasKanidmRef,
    <T as Resource>::DynamicType: Eq + std::hash::Hash + Clone,
{
    let obj_namespace = object.namespace().unwrap_or_else(|| "default".to_string());
    let (ref_name, ref_namespace) = normalize_kanidm_ref(object.kanidm_ref_spec(), &obj_namespace);
    let obj_kanidm_entity_name = object.kanidm_entity_name();

    store
        .state()
        .into_iter()
        .find(|resource| {
            resource.meta().uid != object.meta().uid && {
                let res_namespace = resource
                    .namespace()
                    .unwrap_or_else(|| "default".to_string());
                let (res_ref_name, res_ref_namespace) =
                    normalize_kanidm_ref(resource.kanidm_ref_spec(), &res_namespace);
                ref_name == res_ref_name
                    && ref_namespace == res_ref_namespace
                    && resource.kanidm_entity_name() == obj_kanidm_entity_name
            }
        })
        .map(|r| {
            Err(format!(
                "{} with same kanidmRef and kanidmName already exists: {}/{}",
                object_name,
                r.namespace().unwrap_or_else(|| "default".to_string()),
                r.name_any()
            ))
        })
        .unwrap_or(Ok(()))
}

/// Reject creation when an entity with the same effective Kanidm name already exists in a
/// Kanidm cluster configured for external replication.
///
/// This deliberately performs an exact live lookup instead of treating a periodically refreshed
/// cache miss as proof that an entity does not exist. If the referenced Kanidm resource has not
/// been created yet, validation keeps the existing apply-order behavior and lets the request
/// through. Once external replication is known to be enabled, lookup/client errors fail closed.
pub async fn check_external_duplicate<T>(
    state: &WebhookState,
    object: &T,
    object_name: &str,
    entity_kind: KanidmEntityKind,
) -> Result<(), String>
where
    T: Resource + ResourceExt + HasKanidmRef,
{
    let object_namespace = object.namespace().unwrap_or_else(|| "default".to_string());
    let (kanidm_name, kanidm_namespace) =
        normalize_kanidm_ref(object.kanidm_ref_spec(), &object_namespace);

    let kanidm_api = Api::<Kanidm>::namespaced(state.kube_client.clone(), &kanidm_namespace);
    let kanidm = match kanidm_api.get(&kanidm_name).await {
        Ok(kanidm) => kanidm,
        // Preserve the existing ability to apply dependent CRs before the Kanidm CR itself.
        Err(kube::Error::Api(response)) if response.code == 404 => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to verify {object_name} against Kanidm {kanidm_namespace}/{kanidm_name}: {error}"
            ));
        }
    };

    if kanidm.spec.external_replication_nodes.is_empty() {
        return Ok(());
    }

    let key = KanidmKey {
        namespace: kanidm_namespace.clone(),
        name: kanidm_name.clone(),
    };

    let cached_client = state.kanidm_clients.read().await.get(&key).cloned();
    let client = match cached_client {
        Some(client) if client.auth_valid().await.is_ok() => client,
        _ => {
            let client = KanidmClients::create_client(
                &kanidm_namespace,
                &kanidm_name,
                KanidmUser::IdmAdmin,
                state.kube_client.clone(),
            )
            .await
            .map_err(|error| {
                format!(
                    "failed to connect to externally replicated Kanidm {kanidm_namespace}/{kanidm_name} while validating {object_name}: {error:?}"
                )
            })?;

            state
                .kanidm_clients
                .write()
                .await
                .insert(key, client.clone());
            client
        }
    };

    let entity_name = object.kanidm_entity_name();
    let entity_exists = match entity_kind {
        KanidmEntityKind::Group => client
            .idm_group_get(&entity_name)
            .await
            .map(|entry| entry.is_some()),
        KanidmEntityKind::Person => client
            .idm_person_account_get(&entity_name)
            .await
            .map(|entry| entry.is_some()),
        KanidmEntityKind::OAuth2Client => client
            .idm_oauth2_rs_get(&entity_name)
            .await
            .map(|entry| entry.is_some()),
        KanidmEntityKind::ServiceAccount => client
            .idm_service_account_get(&entity_name)
            .await
            .map(|entry| entry.is_some()),
    }
    .map_err(|error| {
        format!(
            "failed to verify whether Kanidm entity '{entity_name}' exists in externally replicated Kanidm {kanidm_namespace}/{kanidm_name}: {error:?}"
        )
    })?;

    if entity_exists {
        Err(format!(
            "{object_name} cannot manage Kanidm entity '{entity_name}' because it already exists in externally replicated Kanidm {kanidm_namespace}/{kanidm_name}"
        ))
    } else {
        Ok(())
    }
}

/// Trait to abstract access to kanidm_ref and kanidm_entity_name across different resource types
pub trait HasKanidmRef {
    fn kanidm_ref_spec(&self) -> &KanidmRef;
    fn kanidm_entity_name(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_operator::crd::KanidmRef;

    #[test]
    fn test_normalize_kanidm_ref_with_namespace() {
        let ref_obj = KanidmRef {
            name: "my-kanidm".to_string(),
            namespace: Some("other-ns".to_string()),
        };
        let (name, ns) = normalize_kanidm_ref(&ref_obj, "current-ns");
        assert_eq!(name, "my-kanidm");
        assert_eq!(ns, "other-ns");
    }

    #[test]
    fn test_normalize_kanidm_ref_without_namespace() {
        let ref_obj = KanidmRef {
            name: "my-kanidm".to_string(),
            namespace: None,
        };
        let (name, ns) = normalize_kanidm_ref(&ref_obj, "current-ns");
        assert_eq!(name, "my-kanidm");
        assert_eq!(ns, "current-ns");
    }
}
