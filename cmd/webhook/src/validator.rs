use crate::state::InternalEntityCache;

use kaniop_operator::crd::KanidmRef;

use kube::runtime::reflector::Store;
use kube::{Resource, ResourceExt};

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
pub fn check_duplicate<T>(object: &T, object_name: &str, store: &Store<T>) -> Result<(), String>
where
    T: Resource + ResourceExt + Clone + HasKanidmRef,
    <T as Resource>::DynamicType: Eq + std::hash::Hash + Clone,
{
    let obj_namespace = object.namespace().unwrap_or_else(|| "default".to_string());
    let (ref_name, ref_namespace) = normalize_kanidm_ref(object.kanidm_ref_spec(), &obj_namespace);

    store
        .state()
        .into_iter()
        .find(|resource| {
            resource.meta().uid != object.meta().uid
                && {
                    let res_namespace = resource
                        .namespace()
                        .unwrap_or_else(|| "default".to_string());
                    let (res_ref_name, res_ref_namespace) =
                        normalize_kanidm_ref(resource.kanidm_ref_spec(), &res_namespace);
                    ref_name == res_ref_name && ref_namespace == res_ref_namespace
                }
                && resource.name_any() == object.name_any()
        })
        .map(|r| {
            Err(format!(
                "{} with same kanidmRef already exists: {}/{}",
                object_name,
                r.namespace().unwrap_or_else(|| "default".to_string()),
                r.name_any()
            ))
        })
        .unwrap_or(Ok(()))
}

/// Check for duplicate considering both CRD store and internal cache
pub async fn check_duplicate_with_internal<T>(
    object: &T,
    object_name: &str,
    store: &Store<T>,
    internal_cache: &InternalEntityCache,
    entity_type: EntityType,
) -> Result<(), String>
where
    T: Resource + ResourceExt + Clone + HasKanidmRef,
    <T as Resource>::DynamicType: Eq + std::hash::Hash + Clone,
{
    // First check CRD store
    check_duplicate(object, object_name, store)?;

    // Then check internal cache
    let obj_namespace = object.namespace().unwrap_or_else(|| "default".to_string());
    let (ref_name, ref_namespace) = normalize_kanidm_ref(object.kanidm_ref_spec(), &obj_namespace);
    let entity_name = object.name_any();

    let has_internal = match entity_type {
        EntityType::Group => {
            internal_cache
                .has_group(&entity_name, &ref_name, &ref_namespace)
                .await
        }
        EntityType::Person => {
            internal_cache
                .has_person(&entity_name, &ref_name, &ref_namespace)
                .await
        }
        EntityType::OAuth2Client => {
            internal_cache
                .has_oauth2_client(&entity_name, &ref_name, &ref_namespace)
                .await
        }
        EntityType::ServiceAccount => {
            internal_cache
                .has_service_account(&entity_name, &ref_name, &ref_namespace)
                .await
        }
    };

    if has_internal {
        return Err(format!(
            "{} '{}' already exists in Kanidm cluster {}/{} (synced from external replication)",
            object_name, entity_name, ref_namespace, ref_name
        ));
    }

    Ok(())
}

/// Type of entity being validated
#[derive(Debug, Clone, Copy)]
pub enum EntityType {
    Group,
    Person,
    OAuth2Client,
    ServiceAccount,
}

/// Trait to abstract access to kanidm_ref across different resource types
pub trait HasKanidmRef {
    fn kanidm_ref_spec(&self) -> &KanidmRef;
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
