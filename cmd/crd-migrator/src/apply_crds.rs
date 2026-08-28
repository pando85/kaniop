use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{
    Api, Client,
    api::{Patch, PatchParams},
};

const CRDS_YAML: &str = include_str!("../../../charts/kaniop/crds/crds.yaml");
const FIELD_MANAGER: &str = "kaniop-helm-crds";
const ESTABLISH_POLL_INTERVAL: Duration = Duration::from_secs(2);

fn strip_unsupported_integer_formats(value: &mut serde_yaml::Value) {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            let format_key = serde_yaml::Value::String("format".to_string());
            if let Some(fmt_val) = mapping.get(&format_key) {
                if let serde_yaml::Value::String(ref s) = fmt_val {
                    if s == "uint32" || s == "uint64" {
                        mapping.remove(&format_key);
                    }
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

fn parse_crds() -> Result<Vec<CustomResourceDefinition>> {
    let mut crds = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(CRDS_YAML) {
        let mut value = serde_yaml::Value::deserialize(doc)
            .context("failed to deserialize CRD YAML document")?;
        strip_unsupported_integer_formats(&mut value);
        let json_value =
            serde_json::to_value(&value).context("failed to convert YAML value to JSON")?;
        let crd: CustomResourceDefinition =
            serde_json::from_value(json_value).context("failed to deserialize CRD from JSON")?;
        crds.push(crd);
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
    let crds = parse_crds().context("failed to parse embedded CRDs")?;

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
    fn parse_embedded_crds_succeeds() {
        let crds = parse_crds().unwrap();
        assert!(!crds.is_empty(), "embedded CRDs should not be empty");
    }

    #[test]
    fn all_embedded_crds_have_names() {
        let crds = parse_crds().unwrap();
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
    fn embedded_crds_count_matches_expected() {
        let crds = parse_crds().unwrap();
        assert_eq!(crds.len(), 9, "expected 9 CRDs embedded");
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
