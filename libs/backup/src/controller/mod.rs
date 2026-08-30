pub mod backup;
pub mod discovery;
pub mod repository;
pub mod schedule;

pub use backup::CONTROLLER_ID as BACKUP_CONTROLLER_ID;
pub use discovery::CONTROLLER_ID as DISCOVERY_CONTROLLER_ID;
pub use repository::CONTROLLER_ID as REPOSITORY_CONTROLLER_ID;
pub use schedule::CONTROLLER_ID as SCHEDULE_CONTROLLER_ID;

use k8s_openapi::api::core::v1::Pod;
use kube::api::PropagationPolicy;

pub use kaniop_backup_core::image::data_mover_image;
pub use kaniop_backup_core::pod_defaults::{
    default_resource_requirements, hardened_pod_security_context, hardened_security_context,
};

pub const RESULT_PATH: &str = "/kaniop-result/result.json";
pub const TERMINATION_MESSAGE_LIMIT: usize = 4096;
pub const BACKUP_JOB_TTL_SECONDS: i32 = 600;
pub const RESULT_FRAME_BEGIN: &str = "---BEGIN-KANIOP-RESULT---";
pub const RESULT_FRAME_END: &str = "---END-KANIOP-RESULT---";
pub const DISCOVER_LOG_LIMIT_BYTES: i64 = 1024 * 1024;

pub fn background_delete_params() -> kube::api::DeleteParams {
    kube::api::DeleteParams {
        propagation_policy: Some(PropagationPolicy::Background),
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

pub fn build_discover_data_mover_wrapper() -> String {
    format!(
        r#"set +e
/bin/kaniop-data-mover discover --operation-doc "$1"
STATUS=$?
RESULT_FILE="{RESULT_PATH}"
if [ -f "$RESULT_FILE" ]; then
  head -c {TERMINATION_MESSAGE_LIMIT} "$RESULT_FILE" > /dev/termination-log
  printf '%s\n' "{RESULT_FRAME_BEGIN}"
  cat "$RESULT_FILE"
  printf '\n{RESULT_FRAME_END}\n'
fi
exit $STATUS"#
    )
}

pub fn extract_framed_result(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let mut frame_begin: Option<usize> = None;
    let mut frame_end: Option<usize> = None;
    let mut found_complete = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == RESULT_FRAME_BEGIN {
            if frame_begin.is_some() && frame_end.is_none() {
                return None;
            }
            if found_complete {
                return None;
            }
            frame_begin = Some(i);
            frame_end = None;
        } else if trimmed == RESULT_FRAME_END && frame_begin.is_some() && frame_end.is_none() {
            frame_end = Some(i);
            found_complete = true;
        }
    }

    if !found_complete {
        return None;
    }

    let begin = frame_begin?;
    let end = frame_end?;

    if end <= begin + 1 {
        return None;
    }

    let payload = lines[begin + 1..end].join("\n");
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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

    #[test]
    fn build_discover_wrapper_uses_discover_subcommand() {
        let wrapper = build_discover_data_mover_wrapper();
        assert!(wrapper.contains("/bin/kaniop-data-mover discover"));
        assert!(wrapper.contains("--operation-doc"));
    }

    #[test]
    fn build_discover_wrapper_emits_frame_markers() {
        let wrapper = build_discover_data_mover_wrapper();
        assert!(wrapper.contains(RESULT_FRAME_BEGIN));
        assert!(wrapper.contains(RESULT_FRAME_END));
    }

    #[test]
    fn build_discover_wrapper_preserves_exit_status() {
        let wrapper = build_discover_data_mover_wrapper();
        assert!(wrapper.contains("STATUS=$?"));
        assert!(wrapper.contains("exit $STATUS"));
    }

    #[test]
    fn build_discover_wrapper_writes_termination_log() {
        let wrapper = build_discover_data_mover_wrapper();
        assert!(wrapper.contains("/dev/termination-log"));
        assert!(wrapper.contains(&TERMINATION_MESSAGE_LIMIT.to_string()));
    }

    #[test]
    fn extract_framed_result_extracts_payload() {
        let log = format!(
            "some log line\n{RESULT_FRAME_BEGIN}\n{{\"success\":true}}\n{RESULT_FRAME_END}\nmore logs"
        );
        let result = extract_framed_result(&log);
        assert_eq!(result, Some("{\"success\":true}".to_string()));
    }

    #[test]
    fn extract_framed_result_with_surrounding_logs() {
        let log = format!(
            "2024-01-01 INFO starting discover\n\
             2024-01-01 INFO connecting to S3\n\
             {RESULT_FRAME_BEGIN}\n\
             {{\"apiVersion\":\"backup.kaniop.rs/v1alpha1\",\"kind\":\"ResultDocument\"}}\n\
             {RESULT_FRAME_END}\n\
             2024-01-01 INFO done"
        );
        let result = extract_framed_result(&log);
        assert!(result.is_some());
        let payload = result.unwrap();
        assert!(payload.contains("\"apiVersion\""));
        assert!(payload.contains("ResultDocument"));
    }

    #[test]
    fn extract_framed_result_multiline_json() {
        let json = serde_json::json!({
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "discover",
            "success": true,
            "exitCode": "success",
            "discovery": {
                "manifestKeys": [
                    "prod/v1/backups/b1/manifest.json",
                    "prod/v1/backups/b2/manifest.json",
                    "prod/v1/backups/b3/manifest.json"
                ],
                "totalFound": 3,
                "truncated": false
            }
        });
        let pretty = serde_json::to_string_pretty(&json).unwrap();
        let log =
            format!("log line 1\n{RESULT_FRAME_BEGIN}\n{pretty}\n{RESULT_FRAME_END}\nlog line 2");
        let result = extract_framed_result(&log);
        assert!(result.is_some());
        let extracted = result.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&extracted).unwrap();
        assert_eq!(parsed["operation"], "discover");
        assert_eq!(parsed["discovery"]["totalFound"], 3);
    }

    #[test]
    fn extract_framed_result_missing_begin_marker() {
        let log = format!("some logs\n{{\"success\":true}}\n{RESULT_FRAME_END}\nmore logs");
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_missing_end_marker() {
        let log = format!("some logs\n{RESULT_FRAME_BEGIN}\n{{\"success\":true}}\nmore logs");
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_no_markers() {
        let log = "just some normal log lines\nnothing special here";
        let result = extract_framed_result(log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_empty_payload() {
        let log = format!("{RESULT_FRAME_BEGIN}\n\n{RESULT_FRAME_END}");
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_whitespace_only_payload() {
        let log = format!("{RESULT_FRAME_BEGIN}\n   \n  \n{RESULT_FRAME_END}");
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_empty_log() {
        let result = extract_framed_result("");
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_markers_reversed() {
        let log = format!("{RESULT_FRAME_END}\n{RESULT_FRAME_BEGIN}\n{{\"ok\":true}}");
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_rejects_inline_begin_marker() {
        let log = format!("INFO {RESULT_FRAME_BEGIN}\n{{\"ok\":true}}\n{RESULT_FRAME_END}");
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_rejects_inline_end_marker() {
        let log = format!("{RESULT_FRAME_BEGIN}\n{{\"ok\":true}}\n{RESULT_FRAME_END} INFO");
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_rejects_multiple_complete_frames() {
        let log = format!(
            "{RESULT_FRAME_BEGIN}\n{{\"first\":true}}\n{RESULT_FRAME_END}\n\
             {RESULT_FRAME_BEGIN}\n{{\"second\":true}}\n{RESULT_FRAME_END}"
        );
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_rejects_nested_begin() {
        let log = format!(
            "{RESULT_FRAME_BEGIN}\n{RESULT_FRAME_BEGIN}\n{{\"ok\":true}}\n{RESULT_FRAME_END}"
        );
        let result = extract_framed_result(&log);
        assert!(result.is_none());
    }

    #[test]
    fn extract_framed_result_oversized_payload_detected_by_parse() {
        let large_keys: Vec<String> = (0..2000)
            .map(|i| {
                format!(
                    "prod/v1/tenants/default-ns/clusters/kaniop/backups/{:032x}-f423-7a12-8f41-2bea7588a303/manifest.json",
                    i
                )
            })
            .collect();
        let doc = serde_json::json!({
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "discover",
            "success": true,
            "exitCode": "success",
            "discovery": {
                "manifestKeys": large_keys,
                "totalFound": 2000,
                "truncated": true
            }
        });
        let pretty = serde_json::to_string_pretty(&doc).unwrap();
        let log = format!("{RESULT_FRAME_BEGIN}\n{pretty}\n{RESULT_FRAME_END}");
        let extracted = extract_framed_result(&log);
        assert!(extracted.is_some());
        let parse_result = kaniop_backup_core::result::parse_result_document(&extracted.unwrap());
        if pretty.len() > kaniop_backup_core::result::MAX_RESULT_DOC_SIZE {
            assert!(parse_result.is_err());
        }
    }

    #[test]
    fn discover_log_limit_bytes_exceeds_max_result_doc_size() {
        assert!(DISCOVER_LOG_LIMIT_BYTES > kaniop_backup_core::result::MAX_RESULT_DOC_SIZE as i64);
    }
}
