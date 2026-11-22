use kaniop_backup::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule};
use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::KanidmRestore;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_radius::crd::KanidmRadiusServer;
use kaniop_service_account::crd::KanidmServiceAccount;

use kube::CustomResourceExt;
use serde_yaml::Value;

mod yaml;

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

fn crd_value<T: serde::Serialize>(crd: &T) -> Value {
    // safe unwrap: we know CRDs are serializable
    let mut value = serde_yaml::to_value(crd).unwrap();
    strip_unsupported_integer_formats(&mut value);
    value
}

fn serialize_crd<T: serde::Serialize>(crd: &T) -> String {
    yaml::to_yaml_string(&crd_value(crd))
}

fn main() {
    for crd in [
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
            KanidmBackupRepository::crd(),
            KanidmBackupSchedule::crd(),
            KanidmBackup::crd(),
            KanidmGroup::crd(),
            KanidmOAuth2Client::crd(),
            KanidmPersonAccount::crd(),
            KanidmServiceAccount::crd(),
            KanidmRadiusServer::crd(),
        ] {
            let yaml = serialize_crd(&crd);
            assert!(!yaml.contains("format: uint32"));
            assert!(!yaml.contains("format: uint64"));
        }
    }

    #[test]
    fn generated_crds_round_trip_without_changing_scalar_types() {
        for crd in [
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
        ] {
            let value = crd_value(&crd);
            let yaml = serialize_crd(&crd);
            // The written document has to parse back as the same schema it was generated from.
            let written: Value = match serde_yaml::from_str(&yaml) {
                Ok(value) => value,
                Err(error) => panic!("the generated YAML does not parse: {error}\n{yaml}"),
            };
            assert_eq!(written, value, "the generated YAML must round trip");
            // A YAML 1.1 parser, the one `kubectl`, Helm and Argo CD use, resolves plain `off`,
            // `On`, `y` or `1_000` to a boolean or a number, which changes the CRD schema. Every
            // string that would be affected must be written as a quoted scalar.
            for text in ambiguous_strings(&value) {
                for plain in [format!("- {text}\n"), format!(": {text}\n")] {
                    assert!(
                        !yaml.contains(&plain),
                        "{text:?} must not be written as a plain scalar:\n{yaml}"
                    );
                }
            }
        }
    }

    /// The strings of a value that a YAML parser reads as something else when written unquoted.
    fn ambiguous_strings(value: &Value) -> Vec<String> {
        let mut out = Vec::new();
        collect_ambiguous_strings(value, &mut out);
        out
    }

    fn collect_ambiguous_strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                if yaml::is_ambiguous_plain_scalar(text) {
                    out.push(text.clone());
                }
            }
            Value::Mapping(mapping) => {
                for (key, val) in mapping {
                    collect_ambiguous_strings(key, out);
                    collect_ambiguous_strings(val, out);
                }
            }
            Value::Sequence(sequence) => {
                for val in sequence {
                    collect_ambiguous_strings(val, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn backup_retention_durations_have_schema_validation() {
        let schedule_yaml = serialize_crd(&KanidmBackupSchedule::crd());
        assert!(
            schedule_yaml.contains("self.minAge.matches('^[0-9]+[hd]?
        let yaml = serialize_crd(&Kanidm::crd());
        assert!(
            yaml.contains("- 'off'\n"),
            "the mail sender log level enum must quote `off`:\n{yaml}"
        );
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
        let yaml = yaml::to_yaml_string(&value);

        assert!(!yaml.contains("format: uint32"));
        assert!(!yaml.contains("format: uint64"));
        assert!(yaml.contains("format: int32"));
        assert!(yaml.contains("minimum: 0"));
    }

    #[test]
    fn person_ssh_public_keys_has_atomic_map_type() {
        let yaml = serialize_crd(&KanidmPersonAccount::crd());
        assert!(
            yaml.contains("x-kubernetes-map-type: atomic"),
            "Person sshPublicKeys should have x-kubernetes-map-type: atomic"
        );
    }

    #[test]
    fn conditions_fields_have_list_map_annotations() {
        for crd in [
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
        ] {
            let yaml = serialize_crd(&crd);
            assert!(
                yaml.contains("x-kubernetes-list-type: map"),
                "CRD should contain x-kubernetes-list-type: map"
            );
            assert!(
                yaml.contains("x-kubernetes-list-map-keys:"),
                "CRD should contain x-kubernetes-list-map-keys"
            );
            assert!(
                yaml.contains("- type"),
                "CRD should have 'type' as a list map key"
            );
        }
    }
}
)"),
            "Schedule retention.minAge must have a CEL validation"
        );

        let repository_yaml = serialize_crd(&KanidmBackupRepository::crd());
        assert!(
            repository_yaml.contains("self.safetyBackupMinRetention.matches('^[0-9]+[hd]?
        let yaml = serialize_crd(&Kanidm::crd());
        assert!(
            yaml.contains("- 'off'\n"),
            "the mail sender log level enum must quote `off`:\n{yaml}"
        );
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
        let yaml = yaml::to_yaml_string(&value);

        assert!(!yaml.contains("format: uint32"));
        assert!(!yaml.contains("format: uint64"));
        assert!(yaml.contains("format: int32"));
        assert!(yaml.contains("minimum: 0"));
    }

    #[test]
    fn person_ssh_public_keys_has_atomic_map_type() {
        let yaml = serialize_crd(&KanidmPersonAccount::crd());
        assert!(
            yaml.contains("x-kubernetes-map-type: atomic"),
            "Person sshPublicKeys should have x-kubernetes-map-type: atomic"
        );
    }

    #[test]
    fn conditions_fields_have_list_map_annotations() {
        for crd in [
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
        ] {
            let yaml = serialize_crd(&crd);
            assert!(
                yaml.contains("x-kubernetes-list-type: map"),
                "CRD should contain x-kubernetes-list-type: map"
            );
            assert!(
                yaml.contains("x-kubernetes-list-map-keys:"),
                "CRD should contain x-kubernetes-list-map-keys"
            );
            assert!(
                yaml.contains("- type"),
                "CRD should have 'type' as a list map key"
            );
        }
    }
}
)"),
            "Repository limits.safetyBackupMinRetention must have a CEL validation"
        );
    }

    #[test]
    fn mail_sender_log_level_off_is_quoted() {
        let yaml = serialize_crd(&Kanidm::crd());
        assert!(
            yaml.contains("- 'off'\n"),
            "the mail sender log level enum must quote `off`:\n{yaml}"
        );
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
        let yaml = yaml::to_yaml_string(&value);

        assert!(!yaml.contains("format: uint32"));
        assert!(!yaml.contains("format: uint64"));
        assert!(yaml.contains("format: int32"));
        assert!(yaml.contains("minimum: 0"));
    }

    #[test]
    fn person_ssh_public_keys_has_atomic_map_type() {
        let yaml = serialize_crd(&KanidmPersonAccount::crd());
        assert!(
            yaml.contains("x-kubernetes-map-type: atomic"),
            "Person sshPublicKeys should have x-kubernetes-map-type: atomic"
        );
    }

    #[test]
    fn conditions_fields_have_list_map_annotations() {
        for crd in [
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
        ] {
            let yaml = serialize_crd(&crd);
            assert!(
                yaml.contains("x-kubernetes-list-type: map"),
                "CRD should contain x-kubernetes-list-type: map"
            );
            assert!(
                yaml.contains("x-kubernetes-list-map-keys:"),
                "CRD should contain x-kubernetes-list-map-keys"
            );
            assert!(
                yaml.contains("- type"),
                "CRD should have 'type' as a list map key"
            );
        }
    }
}
