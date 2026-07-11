use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{Api, CustomResourceExt};

use kaniop_person::crd::KanidmPersonAccount;

use crate::{API_GROUP, API_VERSION, CORRECTED_PLURAL, KIND, MigrationError, Result};

pub fn extract_corrected_person_crd() -> Result<CustomResourceDefinition> {
    Ok(KanidmPersonAccount::crd())
}

pub async fn get_crd(
    api: &Api<CustomResourceDefinition>,
    name: &str,
) -> Result<Option<CustomResourceDefinition>> {
    match api.get(name).await {
        Ok(crd) => Ok(Some(crd)),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(MigrationError::Kube(format!("get CRD {name}"), Box::new(e))),
    }
}

pub async fn delete_crd(api: &Api<CustomResourceDefinition>, name: &str) -> Result<()> {
    match api.delete(name, &Default::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(()),
        Err(e) => Err(MigrationError::Kube(
            format!("delete CRD {name}"),
            Box::new(e),
        )),
    }
}

pub fn validate_corrected_crd(existing: &CustomResourceDefinition) -> Result<()> {
    let spec = &existing.spec;

    if spec.group != API_GROUP {
        return Err(MigrationError::Crd(format!(
            "corrected CRD has wrong group: {}, expected {}",
            spec.group, API_GROUP
        )));
    }

    if spec.names.kind != KIND {
        return Err(MigrationError::Crd(format!(
            "corrected CRD has wrong kind: {}, expected {}",
            spec.names.kind, KIND
        )));
    }

    if spec.names.plural != CORRECTED_PLURAL {
        return Err(MigrationError::Crd(format!(
            "corrected CRD has wrong plural: {}, expected {}",
            spec.names.plural, CORRECTED_PLURAL
        )));
    }

    if spec.scope != "Namespaced" {
        return Err(MigrationError::Crd(format!(
            "corrected CRD has wrong scope: {}, expected Namespaced",
            spec.scope
        )));
    }

    let v1beta1_version = spec
        .versions
        .iter()
        .find(|v| v.name == API_VERSION)
        .ok_or_else(|| {
            MigrationError::Crd(format!(
                "corrected CRD does not serve version {}",
                API_VERSION
            ))
        })?;

    if !v1beta1_version.served {
        return Err(MigrationError::Crd(format!(
            "corrected CRD version {} is not served",
            API_VERSION
        )));
    }

    if !v1beta1_version.storage {
        return Err(MigrationError::Crd(format!(
            "corrected CRD version {} is not the storage version",
            API_VERSION
        )));
    }

    Ok(())
}

pub fn validate_corrected_crd_schema(existing: &CustomResourceDefinition) -> Result<()> {
    validate_corrected_crd(existing)?;

    let expected_crd = extract_corrected_person_crd()?;
    let expected_spec = &expected_crd.spec;

    let expected_version = expected_spec
        .versions
        .iter()
        .find(|v| v.name == API_VERSION)
        .ok_or_else(|| {
            MigrationError::Crd(format!(
                "embedded CRD does not have version {}",
                API_VERSION
            ))
        })?;

    let existing_version = existing
        .spec
        .versions
        .iter()
        .find(|v| v.name == API_VERSION)
        .ok_or_else(|| {
            MigrationError::Crd(format!(
                "existing CRD does not have version {}",
                API_VERSION
            ))
        })?;

    let expected_schema = expected_version
        .schema
        .as_ref()
        .and_then(|s| s.open_api_v3_schema.as_ref());

    let existing_schema = existing_version
        .schema
        .as_ref()
        .and_then(|s| s.open_api_v3_schema.as_ref());

    match (expected_schema, existing_schema) {
        (Some(expected), Some(existing)) => {
            let expected_json = serde_json::to_value(expected).map_err(|e| {
                MigrationError::Serialization("serialize expected schema".to_string(), e)
            })?;
            let existing_json = serde_json::to_value(existing).map_err(|e| {
                MigrationError::Serialization("serialize existing schema".to_string(), e)
            })?;

            if expected_json != existing_json {
                return Err(MigrationError::Crd(
                    "corrected CRD schema does not match embedded generated CRD".to_string(),
                ));
            }
        }
        (None, None) => {}
        (Some(_), None) => {
            return Err(MigrationError::Crd(
                "corrected CRD is missing schema but embedded CRD has one".to_string(),
            ));
        }
        (None, Some(_)) => {
            return Err(MigrationError::Crd(
                "corrected CRD has schema but embedded CRD does not".to_string(),
            ));
        }
    }

    Ok(())
}

pub fn is_crd_established(crd: &CustomResourceDefinition) -> bool {
    crd.status.as_ref().is_some_and(|status| {
        status.conditions.as_ref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|c| c.type_ == "Established" && c.status == "True")
        })
    })
}

pub fn is_crd_names_accepted(crd: &CustomResourceDefinition) -> bool {
    crd.status.as_ref().is_some_and(|status| {
        status.conditions.as_ref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|c| c.type_ == "NamesAccepted" && c.status == "True")
        })
    })
}

pub fn is_crd_ready(crd: &CustomResourceDefinition) -> bool {
    is_crd_established(crd) && is_crd_names_accepted(crd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_corrected_person_crd() {
        let crd = extract_corrected_person_crd().unwrap();
        assert_eq!(crd.spec.group, API_GROUP);
        assert_eq!(crd.spec.names.kind, KIND);
        assert_eq!(crd.spec.names.plural, CORRECTED_PLURAL);
        assert_eq!(crd.spec.scope, "Namespaced");
    }

    #[test]
    fn test_validate_corrected_crd_accepts_valid() {
        let crd = extract_corrected_person_crd().unwrap();
        assert!(validate_corrected_crd(&crd).is_ok());
    }
}
