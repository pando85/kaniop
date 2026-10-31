use std::time::Duration;

use anyhow::{Context, Result, bail};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kaniop_backup::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule};
use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::KanidmRestore;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_radius::crd::KanidmRadiusServer;
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
/// CRD name used by Kaniop v0.10.2 and older. Its migration was retired after v0.20.
const LEGACY_PERSON_CRD_NAME: &str = "kanidmpersonsaccounts.kaniop.rs";
const UPGRADE_PATH_HINT: &str = "Kaniop no longer migrates the v0.10 KanidmPersonAccount CRD \
     plural; upgrade to a v0.11.0 to v0.19.3 release first, then upgrade to this release \
     (see the v0.11 CRD Plural Migration upgrade guide)";

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
        KanidmRadiusServer::crd(),
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

fn legacy_person_crd_error() -> String {
    format!("found legacy CRD {LEGACY_PERSON_CRD_NAME}. {UPGRADE_PATH_HINT}")
}

fn naming_conflict_error(name: &str, reason: &str) -> String {
    format!(
        "CRD {name} names were not accepted by the API server: {reason}. Kaniop resources of \
         this kind cannot be served until the conflicting CRD is removed. If you are upgrading \
         from Kaniop v0.10.2 or older: {UPGRADE_PATH_HINT}"
    )
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

    let legacy_person_crd = timeout(API_CALL_TIMEOUT, crd_api.get_opt(LEGACY_PERSON_CRD_NAME))
        .await
        .context("timed out checking for the legacy person CRD")?
        .context("failed to check for the legacy person CRD")?;
    if legacy_person_crd.is_some() {
        bail!(legacy_person_crd_error());
    }

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
                bail!(naming_conflict_error(name, &reason));
            }
            tracing::debug!(crd = name, "waiting for CRD to become Established");
            tokio::time::sleep(ESTABLISH_POLL_INTERVAL).await;
        }
    }

    tracing::info!(count = crds.len(), "all CRDs applied and Established");
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
        assert_eq!(crds.len(), 10, "expected 10 CRDs generated");
    }

    #[test]
    fn generated_crds_include_radius_server() {
        let crds = generate_crds().unwrap();
        assert!(
            crds.iter()
                .any(|crd| crd_name(crd) == "kanidmradiusservers.kaniop.rs"),
            "upgrades must install the KanidmRadiusServer CRD"
        );
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
    fn legacy_person_crd_error_points_to_supported_upgrade_path() {
        let message = legacy_person_crd_error();
        assert!(message.contains(LEGACY_PERSON_CRD_NAME));
        assert!(message.contains("v0.11.0"));
    }

    #[test]
    fn naming_conflict_error_names_crd_reason_and_upgrade_path() {
        let message = naming_conflict_error(
            "kanidmpersonaccounts.kaniop.rs",
            "\"KanidmPersonAccountList\" is already in use",
        );
        assert!(message.contains("kanidmpersonaccounts.kaniop.rs"));
        assert!(message.contains("KanidmPersonAccountList"));
        assert!(message.contains("v0.11.0"));
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
