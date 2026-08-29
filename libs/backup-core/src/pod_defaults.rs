use k8s_openapi::api::core::v1::{
    Capabilities, PodSecurityContext, ResourceRequirements, SeccompProfile, SecurityContext,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardened_security_context_drops_all_capabilities() {
        let ctx = hardened_security_context();
        assert_eq!(ctx.allow_privilege_escalation, Some(false));
        assert_eq!(ctx.read_only_root_filesystem, Some(true));
        assert_eq!(ctx.run_as_non_root, Some(true));
        assert_eq!(ctx.run_as_user, Some(65534));
        let caps = ctx.capabilities.unwrap();
        assert_eq!(caps.drop.unwrap(), vec!["ALL".to_string()]);
    }

    #[test]
    fn hardened_pod_security_context_uses_runtime_default_seccomp() {
        let ctx = hardened_pod_security_context();
        assert_eq!(ctx.run_as_non_root, Some(true));
        let seccomp = ctx.seccomp_profile.unwrap();
        assert_eq!(seccomp.type_, "RuntimeDefault");
    }

    #[test]
    fn default_resource_requirements_has_expected_values() {
        let reqs = default_resource_requirements();
        let requests = reqs.requests.unwrap();
        assert_eq!(requests.get("cpu").unwrap().0, "50m");
        assert_eq!(requests.get("memory").unwrap().0, "32Mi");
        let limits = reqs.limits.unwrap();
        assert_eq!(limits.get("cpu").unwrap().0, "100m");
        assert_eq!(limits.get("memory").unwrap().0, "64Mi");
    }
}
