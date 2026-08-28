pub mod backup;
pub mod discovery;
pub mod repository;
pub mod schedule;

pub use backup::CONTROLLER_ID as BACKUP_CONTROLLER_ID;
pub use discovery::CONTROLLER_ID as DISCOVERY_CONTROLLER_ID;
pub use repository::CONTROLLER_ID as REPOSITORY_CONTROLLER_ID;
pub use schedule::CONTROLLER_ID as SCHEDULE_CONTROLLER_ID;

use k8s_openapi::api::core::v1::{
    Capabilities, Pod, PodSecurityContext, ResourceRequirements, SeccompProfile, SecurityContext,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::PropagationPolicy;

pub const RESULT_PATH: &str = "/kaniop-result/result.json";
pub const TERMINATION_MESSAGE_LIMIT: usize = 4096;
pub const BACKUP_JOB_TTL_SECONDS: i32 = 600;

pub fn background_delete_params() -> kube::api::DeleteParams {
    kube::api::DeleteParams {
        propagation_policy: Some(PropagationPolicy::Background),
        ..Default::default()
    }
}

pub fn data_mover_image() -> String {
    std::env::var("DATA_MOVER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/pando85/kaniop-data-mover:latest".to_string())
}

pub fn hardened_security_context() -> SecurityContext {
    SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        }),
        read_only_root_filesystem: Some(true),
        run_as_non_root: Some(true),
        run_as_user: Some(65534),
        ..Default::default()
    }
}

pub fn hardened_pod_security_context() -> PodSecurityContext {
    PodSecurityContext {
        run_as_non_root: Some(true),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub fn default_resource_requirements() -> ResourceRequirements {
    ResourceRequirements {
        requests: Some(
            [
                ("cpu".to_string(), Quantity("50m".to_string())),
                ("memory".to_string(), Quantity("32Mi".to_string())),
            ]
            .into_iter()
            .collect(),
        ),
        limits: Some(
            [
                ("cpu".to_string(), Quantity("100m".to_string())),
                ("memory".to_string(), Quantity("64Mi".to_string())),
            ]
            .into_iter()
            .collect(),
        ),
        ..Default::default()
    }
}

pub fn build_data_mover_wrapper(subcommand: &str) -> String {
    format!(
        r#"set +e
/bin/kaniop-data-mover {subcommand} --operation-doc "$1"
STATUS=$?
RESULT_FILE="{RESULT_PATH}"
if [ -f "$RESULT_FILE" ]; then
  head -c {TERMINATION_MESSAGE_LIMIT} "$RESULT_FILE" > /dev/termination-log
fi
exit $STATUS"#
    )
}

pub fn select_succeeded_pod(pods: &[Pod]) -> Option<&Pod> {
    pods.iter()
        .filter(|p| {
            p.status.as_ref().and_then(|s| s.phase.as_ref()) == Some(&"Succeeded".to_string())
        })
        .max_by_key(|p| {
            p.status
                .as_ref()
                .and_then(|s| s.start_time.as_ref())
                .map(|t| t.0.to_string())
                .unwrap_or_default()
        })
}

pub fn extract_termination_message(pod: &Pod, container_name: &str) -> Option<String> {
    let container_status = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())?
        .iter()
        .find(|cs| cs.name == container_name)?;

    let message = container_status
        .state
        .as_ref()
        .and_then(|state| state.terminated.as_ref())
        .and_then(|t| t.message.as_ref())?;

    if message.is_empty() {
        None
    } else {
        Some(message.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateTerminated, ContainerStatus, PodStatus,
    };
    use kube::ResourceExt;

    #[test]
    fn build_data_mover_wrapper_contains_subcommand() {
        let wrapper = build_data_mover_wrapper("discover");
        assert!(wrapper.contains("/bin/kaniop-data-mover discover"));
        assert!(wrapper.contains("--operation-doc"));
        assert!(wrapper.contains("/dev/termination-log"));
    }

    #[test]
    fn build_data_mover_wrapper_captures_exit_status() {
        let wrapper = build_data_mover_wrapper("discover");
        assert!(wrapper.contains("STATUS=$?"));
        assert!(wrapper.contains("exit $STATUS"));
    }

    #[test]
    fn build_data_mover_wrapper_checks_result_file() {
        let wrapper = build_data_mover_wrapper("download");
        assert!(wrapper.contains("if [ -f \"$RESULT_FILE\" ]"));
        assert!(wrapper.contains(RESULT_PATH));
    }

    #[test]
    fn select_succeeded_pod_returns_succeeded() {
        let pod1 = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("pod1".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some("Failed".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pod2 = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("pod2".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some("Succeeded".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pods = vec![pod1, pod2];
        let selected = select_succeeded_pod(&pods);
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name_any(), "pod2");
    }

    #[test]
    fn select_succeeded_pod_returns_none_when_no_succeeded() {
        let pod1 = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("pod1".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some("Failed".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pods = vec![pod1];
        let selected = select_succeeded_pod(&pods);
        assert!(selected.is_none());
    }

    #[test]
    fn select_succeeded_pod_returns_newest_when_multiple_succeeded() {
        let pod1 = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("pod1".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some("Succeeded".to_string()),
                start_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    k8s_openapi::jiff::Timestamp::new(1704067200, 0).unwrap(),
                )),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pod2 = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("pod2".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some("Succeeded".to_string()),
                start_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    k8s_openapi::jiff::Timestamp::new(1704153600, 0).unwrap(),
                )),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pods = vec![pod1, pod2];
        let selected = select_succeeded_pod(&pods);
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name_any(), "pod2");
    }

    #[test]
    fn extract_termination_message_returns_message() {
        let pod = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("test-pod".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                container_statuses: Some(vec![ContainerStatus {
                    name: "probe".to_string(),
                    state: Some(ContainerState {
                        terminated: Some(ContainerStateTerminated {
                            message: Some("{\"success\":true}".to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = extract_termination_message(&pod, "probe");
        assert!(message.is_some());
        assert_eq!(message.unwrap(), "{\"success\":true}");
    }

    #[test]
    fn extract_termination_message_returns_none_for_wrong_container() {
        let pod = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("test-pod".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                container_statuses: Some(vec![ContainerStatus {
                    name: "other".to_string(),
                    state: Some(ContainerState {
                        terminated: Some(ContainerStateTerminated {
                            message: Some("message".to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = extract_termination_message(&pod, "probe");
        assert!(message.is_none());
    }

    #[test]
    fn extract_termination_message_returns_none_for_empty_message() {
        let pod = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("test-pod".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                container_statuses: Some(vec![ContainerStatus {
                    name: "probe".to_string(),
                    state: Some(ContainerState {
                        terminated: Some(ContainerStateTerminated {
                            message: Some("".to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = extract_termination_message(&pod, "probe");
        assert!(message.is_none());
    }

    #[test]
    fn extract_termination_message_returns_none_when_no_container_statuses() {
        let pod = Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("test-pod".to_string()),
                ..Default::default()
            },
            status: Some(PodStatus {
                container_statuses: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = extract_termination_message(&pod, "probe");
        assert!(message.is_none());
    }

    #[test]
    fn background_delete_params_uses_background_propagation() {
        let dp = background_delete_params();
        assert_eq!(
            dp.propagation_policy,
            Some(kube::api::PropagationPolicy::Background)
        );
    }

    #[test]
    fn backup_job_ttl_is_positive() {
        const { assert!(BACKUP_JOB_TTL_SECONDS > 0) };
    }
}
