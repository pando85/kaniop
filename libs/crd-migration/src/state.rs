use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use serde::{Deserialize, Serialize};

use crate::{
    CORRECTED_CRD_NAME, LEGACY_CRD_NAME, MIGRATION_LABEL, MIGRATION_VERSION, MigrationError, Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Phase {
    Discovered,
    BackedUp,
    OperatorStopped,
    FinalizersRemoved,
    LegacyCRDDeleted,
    CorrectedCRDCreated,
    ObjectsRestored,
    Verified,
    Completed,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Phase::Discovered => "Discovered",
            Phase::BackedUp => "BackedUp",
            Phase::OperatorStopped => "OperatorStopped",
            Phase::FinalizersRemoved => "FinalizersRemoved",
            Phase::LegacyCRDDeleted => "LegacyCRDDeleted",
            Phase::CorrectedCRDCreated => "CorrectedCRDCreated",
            Phase::ObjectsRestored => "ObjectsRestored",
            Phase::Verified => "Verified",
            Phase::Completed => "Completed",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for Phase {
    type Err = MigrationError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Discovered" => Ok(Phase::Discovered),
            "BackedUp" => Ok(Phase::BackedUp),
            "OperatorStopped" => Ok(Phase::OperatorStopped),
            "FinalizersRemoved" => Ok(Phase::FinalizersRemoved),
            "LegacyCRDDeleted" => Ok(Phase::LegacyCRDDeleted),
            "CorrectedCRDCreated" => Ok(Phase::CorrectedCRDCreated),
            "ObjectsRestored" => Ok(Phase::ObjectsRestored),
            "Verified" => Ok(Phase::Verified),
            "Completed" => Ok(Phase::Completed),
            _ => Err(MigrationError::State(format!("unknown phase: {s}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationMarker {
    pub migration_version: String,
    pub phase: Phase,
    pub legacy_crd: String,
    pub corrected_crd: String,
    pub original_replicas: Option<i32>,
    pub source_count: usize,
    pub restored_count: usize,
    pub source_set_checksum: String,
    pub started_at: String,
    pub updated_at: String,
}

impl MigrationMarker {
    pub fn new(now: &str) -> Self {
        Self {
            migration_version: MIGRATION_VERSION.to_string(),
            phase: Phase::Discovered,
            legacy_crd: LEGACY_CRD_NAME.to_string(),
            corrected_crd: CORRECTED_CRD_NAME.to_string(),
            original_replicas: None,
            source_count: 0,
            restored_count: 0,
            source_set_checksum: String::new(),
            started_at: now.to_string(),
            updated_at: now.to_string(),
        }
    }
}

pub fn validate_marker(marker: &MigrationMarker) -> Result<()> {
    if marker.migration_version != MIGRATION_VERSION {
        return Err(MigrationError::State(format!(
            "marker has incompatible migration version: {}, expected {}",
            marker.migration_version, MIGRATION_VERSION
        )));
    }

    if marker.legacy_crd != LEGACY_CRD_NAME {
        return Err(MigrationError::State(format!(
            "marker has wrong legacy CRD name: {}, expected {}",
            marker.legacy_crd, LEGACY_CRD_NAME
        )));
    }

    if marker.corrected_crd != CORRECTED_CRD_NAME {
        return Err(MigrationError::State(format!(
            "marker has wrong corrected CRD name: {}, expected {}",
            marker.corrected_crd, CORRECTED_CRD_NAME
        )));
    }

    if marker.phase >= Phase::BackedUp {
        let expected_checksum = crate::checksum::empty_source_set_checksum();
        if marker.source_set_checksum.is_empty() {
            return Err(MigrationError::State(
                "marker is at BackedUp or later but has empty source_set_checksum".to_string(),
            ));
        }
        if marker.source_count == 0 && marker.source_set_checksum != expected_checksum {
            return Err(MigrationError::State(format!(
                "marker has source_count=0 but source_set_checksum {} does not match expected \
                 empty checksum {}",
                marker.source_set_checksum, expected_checksum
            )));
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrdState {
    LegacyPresentNewAbsent,
    LegacyAbsentNewPresent,
    BothAbsent,
    BothPresent,
}

pub fn detect_crd_state(legacy_present: bool, corrected_present: bool) -> CrdState {
    match (legacy_present, corrected_present) {
        (true, false) => CrdState::LegacyPresentNewAbsent,
        (false, true) => CrdState::LegacyAbsentNewPresent,
        (false, false) => CrdState::BothAbsent,
        (true, true) => CrdState::BothPresent,
    }
}

pub fn marker_to_configmap(marker: &MigrationMarker, name: &str, namespace: &str) -> ConfigMap {
    let mut data = BTreeMap::new();
    data.insert(
        "migrationVersion".to_string(),
        marker.migration_version.clone(),
    );
    data.insert("phase".to_string(), marker.phase.to_string());
    data.insert("legacyCRD".to_string(), marker.legacy_crd.clone());
    data.insert("correctedCRD".to_string(), marker.corrected_crd.clone());
    if let Some(replicas) = marker.original_replicas {
        data.insert("originalReplicas".to_string(), replicas.to_string());
    }
    data.insert("sourceCount".to_string(), marker.source_count.to_string());
    data.insert(
        "restoredCount".to_string(),
        marker.restored_count.to_string(),
    );
    data.insert(
        "sourceSetChecksum".to_string(),
        marker.source_set_checksum.clone(),
    );
    data.insert("startedAt".to_string(), marker.started_at.clone());
    data.insert("updatedAt".to_string(), marker.updated_at.clone());

    let mut labels = BTreeMap::new();
    labels.insert(MIGRATION_LABEL.to_string(), MIGRATION_VERSION.to_string());

    ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}

pub fn configmap_to_marker(cm: &ConfigMap) -> Result<MigrationMarker> {
    let data = cm
        .data
        .as_ref()
        .ok_or_else(|| MigrationError::State("marker ConfigMap has no data".to_string()))?;

    let get = |key: &str| -> Result<String> {
        data.get(key)
            .cloned()
            .ok_or_else(|| MigrationError::State(format!("marker missing field: {key}")))
    };

    let phase_str = get("phase")?;
    let phase: Phase = phase_str.parse()?;

    Ok(MigrationMarker {
        migration_version: get("migrationVersion")?,
        phase,
        legacy_crd: get("legacyCRD")?,
        corrected_crd: get("correctedCRD")?,
        original_replicas: data.get("originalReplicas").and_then(|v| v.parse().ok()),
        source_count: get("sourceCount")?
            .parse()
            .map_err(|e| MigrationError::State(format!("invalid sourceCount: {e}")))?,
        restored_count: get("restoredCount")?
            .parse()
            .map_err(|e| MigrationError::State(format!("invalid restoredCount: {e}")))?,
        source_set_checksum: get("sourceSetChecksum")?,
        started_at: get("startedAt")?,
        updated_at: get("updatedAt")?,
    })
}

pub async fn get_marker(api: &Api<ConfigMap>, name: &str) -> Result<Option<MigrationMarker>> {
    match api.get(name).await {
        Ok(cm) => {
            let marker = configmap_to_marker(&cm)?;
            validate_marker(&marker)?;
            Ok(Some(marker))
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(MigrationError::Kube(
            format!("get marker {name}"),
            Box::new(e),
        )),
    }
}

pub async fn create_or_update_marker(
    api: &Api<ConfigMap>,
    marker: &MigrationMarker,
    name: &str,
    namespace: &str,
) -> Result<()> {
    let cm = marker_to_configmap(marker, name, namespace);
    match api.get(name).await {
        Ok(existing) => {
            let mut updated = existing;
            updated.data = cm.data.clone();
            api.replace(name, &Default::default(), &updated)
                .await
                .map_err(|e| MigrationError::Kube(format!("update marker {name}"), Box::new(e)))?;
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            api.create(&Default::default(), &cm)
                .await
                .map_err(|e| MigrationError::Kube(format!("create marker {name}"), Box::new(e)))?;
        }
        Err(e) => {
            return Err(MigrationError::Kube(
                format!("get marker {name}"),
                Box::new(e),
            ));
        }
    }
    Ok(())
}

pub async fn delete_marker(api: &Api<ConfigMap>, name: &str) -> Result<()> {
    match api.delete(name, &Default::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(()),
        Err(e) => Err(MigrationError::Kube(
            format!("delete marker {name}"),
            Box::new(e),
        )),
    }
}

pub fn determine_action(crd_state: CrdState, marker: &Option<MigrationMarker>) -> Result<Action> {
    match (crd_state, marker) {
        (CrdState::LegacyPresentNewAbsent, None) => Ok(Action::StartNew),
        (CrdState::LegacyPresentNewAbsent, Some(m)) => Ok(Action::Resume(m.phase)),

        (CrdState::BothPresent, None) => Err(MigrationError::State(
            "both CRDs present with no marker; ambiguous state requires manual intervention"
                .to_string(),
        )),
        (CrdState::BothPresent, Some(m)) => {
            if m.phase >= Phase::LegacyCRDDeleted {
                Err(MigrationError::State(format!(
                    "marker phase is {} (after LegacyCRDDeleted) but legacy CRD still present; \
                     resume will attempt to delete legacy CRD again",
                    m.phase
                )))
            } else {
                Ok(Action::Resume(m.phase))
            }
        }

        (CrdState::LegacyAbsentNewPresent, None) => Ok(Action::PostSyncVerify),
        (CrdState::LegacyAbsentNewPresent, Some(m)) => {
            if m.phase >= Phase::Verified {
                Ok(Action::PostSyncVerify)
            } else {
                Ok(Action::Resume(m.phase))
            }
        }

        (CrdState::BothAbsent, None) => Ok(Action::FreshInstall),
        (CrdState::BothAbsent, Some(m)) => {
            if m.phase >= Phase::FinalizersRemoved && m.phase < Phase::CorrectedCRDCreated {
                Ok(Action::Resume(m.phase))
            } else if m.phase >= Phase::CorrectedCRDCreated {
                Err(MigrationError::State(format!(
                    "marker phase is {} (after CorrectedCRDCreated) but corrected CRD is absent; \
                     inconsistent state requires manual intervention",
                    m.phase
                )))
            } else {
                Err(MigrationError::State(format!(
                    "marker phase is {} (before FinalizersRemoved) but both CRDs are absent; \
                     cannot reset to FreshInstall without losing migration state",
                    m.phase
                )))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    StartNew,
    Resume(Phase),
    FreshInstall,
    PostSyncVerify,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phase_display_roundtrip() {
        let phases = [
            Phase::Discovered,
            Phase::BackedUp,
            Phase::OperatorStopped,
            Phase::FinalizersRemoved,
            Phase::LegacyCRDDeleted,
            Phase::CorrectedCRDCreated,
            Phase::ObjectsRestored,
            Phase::Verified,
            Phase::Completed,
        ];
        for p in phases {
            let s = p.to_string();
            let parsed: Phase = s.parse().unwrap();
            assert_eq!(p, parsed);
        }
    }

    #[test]
    fn test_phase_ordering() {
        assert!(Phase::Discovered < Phase::BackedUp);
        assert!(Phase::BackedUp < Phase::OperatorStopped);
        assert!(Phase::OperatorStopped < Phase::FinalizersRemoved);
        assert!(Phase::FinalizersRemoved < Phase::LegacyCRDDeleted);
        assert!(Phase::LegacyCRDDeleted < Phase::CorrectedCRDCreated);
        assert!(Phase::CorrectedCRDCreated < Phase::ObjectsRestored);
        assert!(Phase::ObjectsRestored < Phase::Verified);
        assert!(Phase::Verified < Phase::Completed);
    }

    #[test]
    fn test_detect_crd_state() {
        assert_eq!(
            detect_crd_state(true, false),
            CrdState::LegacyPresentNewAbsent
        );
        assert_eq!(
            detect_crd_state(false, true),
            CrdState::LegacyAbsentNewPresent
        );
        assert_eq!(detect_crd_state(false, false), CrdState::BothAbsent);
        assert_eq!(detect_crd_state(true, true), CrdState::BothPresent);
    }

    #[test]
    fn test_determine_action_legacy_only_no_marker() {
        let action = determine_action(CrdState::LegacyPresentNewAbsent, &None).unwrap();
        assert_eq!(action, Action::StartNew);
    }

    #[test]
    fn test_determine_action_legacy_only_with_marker() {
        let marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        let action = determine_action(CrdState::LegacyPresentNewAbsent, &Some(marker)).unwrap();
        assert_eq!(action, Action::Resume(Phase::Discovered));
    }

    #[test]
    fn test_determine_action_corrected_only_no_marker() {
        let action = determine_action(CrdState::LegacyAbsentNewPresent, &None).unwrap();
        assert_eq!(action, Action::PostSyncVerify);
    }

    #[test]
    fn test_determine_action_corrected_only_with_verified_marker() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::Verified;
        let action = determine_action(CrdState::LegacyAbsentNewPresent, &Some(marker)).unwrap();
        assert_eq!(action, Action::PostSyncVerify);
    }

    #[test]
    fn test_determine_action_corrected_only_with_incomplete_marker() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::ObjectsRestored;
        let action = determine_action(CrdState::LegacyAbsentNewPresent, &Some(marker)).unwrap();
        assert_eq!(action, Action::Resume(Phase::ObjectsRestored));
    }

    #[test]
    fn test_determine_action_both_absent_no_marker() {
        let action = determine_action(CrdState::BothAbsent, &None).unwrap();
        assert_eq!(action, Action::FreshInstall);
    }

    #[test]
    fn test_determine_action_both_absent_with_early_marker_errors() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        let result = determine_action(CrdState::BothAbsent, &Some(marker));
        assert!(result.is_err());
    }

    #[test]
    fn test_determine_action_both_absent_crash_after_legacy_deleted() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::LegacyCRDDeleted;
        let action = determine_action(CrdState::BothAbsent, &Some(marker)).unwrap();
        assert_eq!(action, Action::Resume(Phase::LegacyCRDDeleted));
    }

    #[test]
    fn test_determine_action_both_absent_crash_before_corrected_created() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::FinalizersRemoved;
        let action = determine_action(CrdState::BothAbsent, &Some(marker)).unwrap();
        assert_eq!(action, Action::Resume(Phase::FinalizersRemoved));
    }

    #[test]
    fn test_determine_action_both_absent_inconsistent_marker() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::CorrectedCRDCreated;
        let result = determine_action(CrdState::BothAbsent, &Some(marker));
        assert!(result.is_err());
    }

    #[test]
    fn test_determine_action_both_present_with_marker_after_delete_fails() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::LegacyCRDDeleted;
        let result = determine_action(CrdState::BothPresent, &Some(marker));
        assert!(result.is_err());
    }

    #[test]
    fn test_determine_action_both_present_with_marker_before_delete() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        let action = determine_action(CrdState::BothPresent, &Some(marker)).unwrap();
        assert_eq!(action, Action::Resume(Phase::BackedUp));
    }

    #[test]
    fn test_determine_action_both_present_no_marker_fails() {
        let result = determine_action(CrdState::BothPresent, &None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_marker_good() {
        let marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        assert!(validate_marker(&marker).is_ok());
    }

    #[test]
    fn test_validate_marker_wrong_version() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.migration_version = "wrong-version".to_string();
        assert!(validate_marker(&marker).is_err());
    }

    #[test]
    fn test_validate_marker_wrong_legacy_crd() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.legacy_crd = "wrong.kaniop.rs".to_string();
        assert!(validate_marker(&marker).is_err());
    }

    #[test]
    fn test_validate_marker_wrong_corrected_crd() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.corrected_crd = "wrong.kaniop.rs".to_string();
        assert!(validate_marker(&marker).is_err());
    }

    #[test]
    fn test_validate_marker_backed_up_without_checksum() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        marker.source_count = 5;
        marker.source_set_checksum = String::new();
        assert!(validate_marker(&marker).is_err());
    }

    #[test]
    fn test_validate_marker_backed_up_without_source_count() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        marker.source_count = 0;
        marker.source_set_checksum = "abc123".to_string();
        assert!(validate_marker(&marker).is_err());
    }

    #[test]
    fn test_validate_marker_backed_up_valid() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        marker.source_count = 5;
        marker.source_set_checksum = "abc123".to_string();
        assert!(validate_marker(&marker).is_ok());
    }

    #[test]
    fn test_validate_marker_backed_up_zero_person_valid() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        marker.source_count = 0;
        marker.source_set_checksum = crate::checksum::empty_source_set_checksum();
        assert!(validate_marker(&marker).is_ok());
    }

    #[test]
    fn test_validate_marker_backed_up_zero_person_wrong_checksum() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        marker.source_count = 0;
        marker.source_set_checksum = "wrong_checksum".to_string();
        assert!(validate_marker(&marker).is_err());
    }

    #[test]
    fn test_validate_marker_verified_zero_person_valid() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::Verified;
        marker.source_count = 0;
        marker.source_set_checksum = crate::checksum::empty_source_set_checksum();
        marker.restored_count = 0;
        assert!(validate_marker(&marker).is_ok());
    }

    #[test]
    fn test_marker_configmap_roundtrip() {
        let mut marker = MigrationMarker::new("2024-01-01T00:00:00Z");
        marker.phase = Phase::BackedUp;
        marker.source_count = 42;
        marker.restored_count = 0;
        marker.source_set_checksum = "abc123".to_string();
        marker.original_replicas = Some(1);

        let cm = marker_to_configmap(&marker, "test-marker", "kaniop");
        let recovered = configmap_to_marker(&cm).unwrap();

        assert_eq!(recovered.phase, marker.phase);
        assert_eq!(recovered.source_count, marker.source_count);
        assert_eq!(recovered.restored_count, marker.restored_count);
        assert_eq!(recovered.source_set_checksum, marker.source_set_checksum);
        assert_eq!(recovered.original_replicas, marker.original_replicas);
        assert_eq!(recovered.migration_version, marker.migration_version);
    }
}
