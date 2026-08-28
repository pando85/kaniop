use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
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

const FIELD_MANAGER: &str = "kaniop-helm-crds";
const ESTABLISH_POLL_INTERVAL: Duration = Duration::from_secs(2);

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

pub async fn apply_crds(timeout: Duration) -> Result<()> {
    let client = Client::try_default()
        .await
        .context("failed to create Kubernetes client")?;

    let crd_api: Api<CustomResourceDefinition> = Api::all(client);
    let crds = generate_crds().context("failed to generate CRDs")?;

    tracing::info!(count = crds.len(), "applying CRDs via server-side apply");

    let pp = PatchParams::apply(FIELD_MANAGER).force();

    for crd in &crds {
        let name = crd_name(crd);
        tracing::info!(crd = name, "server-side applying CRD");
        crd_api
            .patch(name, &pp, &Patch::Apply(crd))
            .await
            .with_context(|| format!("failed to server-side apply CRD {name}"))?;
    }

    let deadline = Instant::now() + timeout;

    for crd in &crds {
        let name = crd_name(crd);
        loop {
            let current = crd_api
                .get(name)
                .await
                .with_context(|| format!("failed to get CRD {name} after apply"))?;
            if is_established(&current) {
                tracing::info!(crd = name, "CRD is Established");
                break;
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for CRD {name} to become Established");
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
