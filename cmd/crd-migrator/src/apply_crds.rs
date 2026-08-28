use std::time::Duration;

use anyhow::{Context, Result};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kaniop_backup::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule};
use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::KanidmRestore;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;
use kube::{
    Api, Client, CustomResourceExt,
    api::{Patch, PatchParams},
};
use tokio::time::timeout;

const FIELD_MANAGER: &str = "kaniop-helm-crds";
const ESTABLISH_POLL_INTERVAL: Duration = Duration::from_secs(2);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
const API_CALL_TIMEOUT: Duration = Duration::from_secs(60);

fn strip_unsupported_integer_formats(value: &mut serde_yaml::Value) {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            let format_key = serde_yaml::Value::String("format".to_string());
            if let Some(serde_yaml::Value::String(s)) = mapping.get(&format_key) {
                if s == "uint32" || s == "uint64" {
                    mapping.remove(&format_key);
                }
            }
            for value in mapping.values_mut() {
                strip_unsupported_integer_formats(value);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for value in seq.iter_mut() {
                strip_unsupported_integer_formats(value);
            }
        }
        _ => {}
    }
}

fn generate_crds() -> Result<Vec<CustomResourceDefinition>> {
    let raw_crds: Vec<CustomResourceDefinition> = vec![
        Kanidm::crd(),
        KanidmRestore::crd(),
        KanidmBackupRepository::crd(),
        KanidmBackupSchedule::crd(),
        KanidmBackup::crd(),
        KanidmGroup::crd(),
        KanidmOAuth2Client::crd(),
        KanidmPersonAccount::crd(),
        KanidmServiceAccount::crd(),
    ];

    let mut crds = Vec::new();
    for crd in raw_crds {
        let mut value =
            serde_yaml::to_value(&crd).context("failed to serialize CRD to YAML value")?;
        strip_unsupported_integer_formats(&mut value);
        let json_value =
            serde_json::to_value(&value).context("failed to convert YAML value to JSON")?;
        let cleaned_crd: CustomResourceDefinition =
            serde_json::from_value(json_value).context("failed to deserialize CRD from JSON")?;
        crds.push(cleaned_crd);
    }
    Ok(crds)
}

fn crd_name(crd: &CustomResourceDefinition) -> &str {
    crd.metadata.name.as_deref().unwrap_or("<unknown>")
}

fn is_established(crd: &CustomResourceDefinition) -> bool {
    crd.status.as_ref().is_some_and(|s| {
        s.conditions.as_ref().is_some_and(|conds| {
            conds
                .iter()
                .any(|c| c.type_ == "Established" && c.status == "True")
        })
    })
}

fn has_naming_conflict(crd: &CustomResourceDefinition) -> Option<String> {
    crd.status.as_ref().and_then(|s| {
        s.conditions.as_ref().and_then(|conds| {
            conds.iter().find_map(|c| {
                if c.type_ == "NamesAccepted" && c.status == "False" {
                    Some(
                        c.message
                            .clone()
                            .unwrap_or_else(|| "names not accepted".to_string()),
                    )
                } else {
                    None
                }
            })
        })
    })
}

pub async fn apply_crds(global_timeout: Duration) -> Result<()> {
    timeout(global_timeout, apply_crds_inner())
        .await
        .context("timed out applying CRDs")?
}

async fn apply_crds_inner() -> Result<()> {
    let client = timeout(CLIENT_TIMEOUT, Client::try_default())
        .await
        .context("timed out creating Kubernetes client")?
        .context("failed to create Kubernetes client")?;

    let crd_api: Api<CustomResourceDefinition> = Api::all(client);
    let crds = generate_crds().context("failed to generate CRDs")?;

    tracing::info!(count = crds.len(), "applying CRDs via server-side apply");

    let pp = PatchParams::apply(FIELD_MANAGER).force();

    for crd in &crds {
        let name = crd_name(crd);
        tracing::info!(crd = name, "server-side applying CRD");
        timeout(
            API_CALL_TIMEOUT,
            crd_api.patch(name, &pp, &Patch::Apply(crd)),
        )
        .await
        .context("timed out applying CRD")?
        .with_context(|| format!("failed to server-side apply CRD {name}"))?;
    }

    let mut skipped = Vec::new();

    for crd in &crds {
        let name = crd_name(crd);
        loop {
            let current = timeout(API_CALL_TIMEOUT, crd_api.get(name))
                .await
                .context("timed out getting CRD status")?
                .with_context(|| format!("failed to get CRD {name} after apply"))?;
            if is_established(&current) {
                tracing::info!(crd = name, "CRD is Established");
                break;
            }
            if let Some(reason) = has_naming_conflict(&current) {
                tracing::warn!(
                    crd = name,
                    reason = %reason,
                    "CRD has naming conflict with existing CRD; skipping Established wait \
                     (will be resolved by migration hook)"
                );
                skipped.push(name.to_string());
                break;
            }
            tracing::debug!(crd = name, "waiting for CRD to become Established");
            tokio::time::sleep(ESTABLISH_POLL_INTERVAL).await;
        }
    }

    let established_count = crds.len() - skipped.len();
    if skipped.is_empty() {
        tracing::info!(count = crds.len(), "all CRDs applied and Established");
    } else {
        tracing::info!(
            total = crds.len(),
            established = established_count,
            skipped = ?skipped,
            "CRDs applied; some skipped due to naming conflicts"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_crds_succeeds() {
        let crds = generate_crds().unwrap();
        assert!(!crds.is_empty(), "generated CRDs should not be empty");
    }

    #[test]
    fn all_generated_crds_have_names() {
        let crds = generate_crds().unwrap();
        for crd in &crds {
            let name = crd.metadata.name.as_deref();
            assert!(name.is_some(), "every CRD must have a metadata.name");
            assert!(
                name.unwrap().ends_with(".kaniop.rs"),
                "CRD name should end with .kaniop.rs, got: {}",
                name.unwrap()
            );
        }
    }

    #[test]
    fn generated_crds_count_matches_expected() {
        let crds = generate_crds().unwrap();
        assert_eq!(crds.len(), 9, "expected 9 CRDs generated");
    }

    #[test]
    fn strip_unsupported_integer_formats_removes_uint32() {
        let mut value: serde_yaml::Value = serde_yaml::from_str(
            r#"
properties:
  field:
    type: integer
    format: uint32
"#,
        )
        .unwrap();
        strip_unsupported_integer_formats(&mut value);
        let yaml = serde_yaml::to_string(&value).unwrap();
        assert!(!yaml.contains("uint32"));
    }

    #[test]
    fn has_naming_conflict_returns_none_for_established_crd() {
        let crd = Kanidm::crd();
        assert!(has_naming_conflict(&crd).is_none());
    }

    #[test]
    fn has_naming_conflict_detects_false_names_accepted() {
        use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinitionCondition;
        let mut crd = Kanidm::crd();
        crd.status = Some(
            k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinitionStatus {
                conditions: Some(vec![CustomResourceDefinitionCondition {
                    type_: "NamesAccepted".to_string(),
                    status: "False".to_string(),
                    message: Some("\"KanidmPersonAccountList\" is already in use".to_string()),
                    reason: Some("ListKindConflict".to_string()),
                    last_transition_time: None,
                }]),
                accepted_names: None,
                stored_versions: None,
            },
        );
        let result = has_naming_conflict(&crd);
        assert!(result.is_some());
        assert!(result.unwrap().contains("KanidmPersonAccountList"));
    }

    #[test]
    fn strip_unsupported_integer_formats_preserves_int32() {
        let mut value: serde_yaml::Value = serde_yaml::from_str(
            r#"
properties:
  field:
    type: integer
    format: int32
"#,
        )
        .unwrap();
        strip_unsupported_integer_formats(&mut value);
        let yaml = serde_yaml::to_string(&value).unwrap();
        assert!(yaml.contains("int32"));
    }
}
