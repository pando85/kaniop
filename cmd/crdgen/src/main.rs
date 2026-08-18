use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::KanidmRestore;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;

use kube::CustomResourceExt;
use serde_yaml::Value;

fn strip_unsupported_integer_formats(value: &mut Value) {
    match value {
        Value::Mapping(mapping) => {
            let format_key = Value::String("format".to_string());
            let remove_format = matches!(
                mapping.get(&format_key),
                Some(Value::String(format)) if matches!(format.as_str(), "uint32" | "uint64")
            );
            if remove_format {
                mapping.remove(&format_key);
            }

            for value in mapping.values_mut() {
                strip_unsupported_integer_formats(value);
            }
        }
        Value::Sequence(sequence) => {
            for value in sequence {
                strip_unsupported_integer_formats(value);
            }
        }
        _ => {}
    }
}

fn serialize_crd<T: serde::Serialize>(crd: &T) -> String {
    // safe unwrap: we know CRDs are serializable
    let mut value = serde_yaml::to_value(crd).unwrap();
    strip_unsupported_integer_formats(&mut value);
    serde_yaml::to_string(&value).unwrap()
}

fn main() {
    for crd in [
        Kanidm::crd(),
        KanidmRestore::crd(),
        KanidmGroup::crd(),
        KanidmOAuth2Client::crd(),
        KanidmPersonAccount::crd(),
        KanidmServiceAccount::crd(),
    ] {
        print!("---\n{}\n", serialize_crd(&crd));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_crds_do_not_use_unsupported_unsigned_formats() {
        for crd in [
            Kanidm::crd(),
            KanidmRestore::crd(),
            KanidmGroup::crd(),
            KanidmOAuth2Client::crd(),
            KanidmPersonAccount::crd(),
            KanidmServiceAccount::crd(),
        ] {
            let yaml = serialize_crd(&crd);
            assert!(!yaml.contains("format: uint32"));
            assert!(!yaml.contains("format: uint64"));
        }
    }

    #[test]
    fn strips_only_unsupported_unsigned_formats() {
        let mut value: Value = serde_yaml::from_str(
            r#"
properties:
  unsigned32:
    type: integer
    format: uint32
    minimum: 0
  unsigned64:
    type: integer
    format: uint64
    minimum: 0
  signed32:
    type: integer
    format: int32
"#,
        )
        .unwrap();

        strip_unsupported_integer_formats(&mut value);
        let yaml = serde_yaml::to_string(&value).unwrap();

        assert!(!yaml.contains("format: uint32"));
        assert!(!yaml.contains("format: uint64"));
        assert!(yaml.contains("format: int32"));
        assert!(yaml.contains("minimum: 0"));
    }
}
