// Adapted from: https://github.com/kubernetes/client-go/blob/ca4a13f6dec7cb79cfd85df0ab3d7cfd05c5c5e9/rest/request.go#L526C1-L605C2
pub fn template_path(path: &str, base_path: Option<&str>) -> String {
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut trimmed_base_path = String::new();

    if let Some(base) = base_path {
        if path.starts_with(base) {
            let p = path.trim_start_matches(base);
            trimmed_base_path = base.to_string();
            segments = p.split('/').filter(|s| !s.is_empty()).collect();
        }
    };

    if segments.len() <= 2 {
        // Return as is if not enough segments
        return path.to_string();
    };

    const CORE_GROUP_PREFIX: &str = "api";
    const NAMED_GROUP_PREFIX: &str = "apis";
    let index = match segments[0] {
        CORE_GROUP_PREFIX => 2,
        NAMED_GROUP_PREFIX => 3,
        _ => return "/{prefix}".to_string(),
    };

    match segments.len() - index {
        // resource (with no name) do nothing
        1 => {}
        2 => {
            // /$RESOURCE/$NAME: replace $NAME with {name}
            segments[index + 1] = "{name}";
        }
        3 => {
            if segments[index + 2] == "finalize" || segments[index + 2] == "status" {
                // /$RESOURCE/$NAME/$SUBRESOURCE: replace $NAME with {name}
                segments[index + 1] = "{name}";
            } else {
                // /namespace/$NAMESPACE/$RESOURCE: replace $NAMESPACE with {namespace}
                segments[index + 1] = "{namespace}";
            }
        }
        _ => {
            segments[index + 1] = "{namespace}";
            // /namespace/$NAMESPACE/$RESOURCE/$NAME: replace $NAMESPACE with {namespace},  $NAME with {name}
            if segments[index + 3] != "finalize" && segments[index + 3] != "status" {
                // /$RESOURCE/$NAME/$SUBRESOURCE: replace $NAME with {name}
                segments[index + 3] = "{name}";
            }
        }
    }

    format!(
        "{}/{}",
        trimmed_base_path.trim_end_matches('/'),
        segments.join("/")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_segments() {
        let path = "/";
        let base_path = None;

        let result = template_path(path, base_path);
        assert_eq!(result, "/");
    }

    #[test]
    fn test_core_group_with_name() {
        let path = "/api/v1/pods/mypod";
        let base_path = None;

        let result = template_path(path, base_path);
        assert_eq!(result, "/api/v1/pods/{name}");
    }

    #[test]
    fn test_named_group_with_namespace() {
        let path = "/apis/apps/v1/namespaces/mynamespace/deployments/mydeployment";
        let base_path = None;

        let result = template_path(path, base_path);
        assert_eq!(
            result,
            "/apis/apps/v1/namespaces/{namespace}/deployments/{name}"
        );
    }

    #[test]
    fn test_with_finalize_subresource() {
        let path = "/apis/apps/v1/namespaces/mynamespace/deployments/mydeployment/finalize";
        let base_path = None;

        let result = template_path(path, base_path);
        assert_eq!(
            result,
            "/apis/apps/v1/namespaces/{namespace}/deployments/{name}/finalize"
        );
    }

    #[test]
    fn test_with_status_subresource() {
        let path = "/apis/apps/v1/namespaces/mynamespace/deployments/mydeployment/status";
        let base_path = None;

        let result = template_path(path, base_path);
        assert_eq!(
            result,
            "/apis/apps/v1/namespaces/{namespace}/deployments/{name}/status"
        );
    }

    #[test]
    fn test_prefix_fallback() {
        let path = "/unknown/group/resource";
        let base_path = None;

        let result = template_path(path, base_path);
        assert_eq!(result, "/{prefix}");
    }

    #[test]
    fn test_trimmed_base_path() {
        let path = "/base/path/api/v1/pods/mypod";
        let base_path = Some("/base/path");

        let result = template_path(path, base_path);
        assert_eq!(result, "/base/path/api/v1/pods/{name}");
    }

    #[test]
    fn test_full_path_with_namespace_and_name() {
        let path = "/some/base/url/path/api/v1/namespaces/ns/r1/nm?p0=v0";
        let base_path = Some("/some/base/url/path");

        let result = template_path(path, base_path);
        assert_eq!(
            result,
            "/some/base/url/path/api/v1/namespaces/{namespace}/r1/{name}"
        );
    }

    #[test]
    fn test_full_path_without_namespace_and_name() {
        let path = "/some/base/url/path/api/v1/r1";
        let base_path = Some("/some/base/url/path");

        let result = template_path(path, base_path);
        assert_eq!(result, "/some/base/url/path/api/v1/r1");
    }

    #[test]
    fn test_custom_prefix_in_url() {
        let path = "/some/base/url/path/pre1/v1/namespaces/ns/r1/nm?p0=v0";
        let base_path = Some("/some/base/url/path");

        let result = template_path(path, base_path);
        assert_eq!(result, "/{prefix}");
    }

    #[test]
    fn test_full_path_without_namespace_or_name() {
        let path = "/some/base/path";
        let base_path = Some("/some/base/path");

        let result = template_path(path, base_path);
        assert_eq!(result, "/some/base/path");
    }

    #[test]
    fn test_path_with_invalid_path() {
        let path = "/invalid/path/v1/namespaces/ns/r1/nm?p0=v0";
        let base_path = None;

        let result = template_path(path, base_path);
        assert_eq!(result, "/{prefix}");
    }
}
