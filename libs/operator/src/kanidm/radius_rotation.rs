//! Crash-resilient RADIUS token-rotation planning.
//!
//! This does not grant permission to revoke a token by itself. The controller
//! must persist the state transitions and obtain a *real* authentication probe
//! for the exact pending token/revision before executing RevokePrevious.
//! Deployment.readyReplicas alone is NEVER valid proof of token usability.

use uuid::Uuid;

use super::radius_crd::KanidmRadiusServerStatus;
use super::radius_generation::config_secret_name;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenGeneration {
    /// Kanidm token ID; never the bearer token value.
    pub token_id: String,
    /// UID-owned, revision-addressed Kubernetes config Secret name.
    pub config_secret: String,
    pub revision: String,
}

/// Persisted, non-secret token/Secret checkpoint recovered from CR status.
/// A generation reservation is recorded before issuing a write-only bearer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistedRadiusState {
    Empty,
    Active(TokenGeneration),
    Reserved {
        active: Option<TokenGeneration>,
        generation: String,
    },
    Pending {
        active: Option<TokenGeneration>,
        pending: TokenGeneration,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadiusStateError {
    InvalidIdentifier,
    PartialActive,
    PartialPending,
    SecretOwnershipMismatch,
    DuplicateGeneration,
}

fn recorded_generation(
    uid: &str,
    token_id: &str,
    generation: &str,
    secret: &str,
) -> Result<TokenGeneration, RadiusStateError> {
    Uuid::parse_str(token_id).map_err(|_| RadiusStateError::InvalidIdentifier)?;
    Uuid::parse_str(generation).map_err(|_| RadiusStateError::InvalidIdentifier)?;
    let expected =
        config_secret_name(uid, generation).map_err(|_| RadiusStateError::InvalidIdentifier)?;
    if secret != expected {
        return Err(RadiusStateError::SecretOwnershipMismatch);
    }
    Ok(TokenGeneration {
        token_id: token_id.into(),
        config_secret: secret.into(),
        revision: generation.into(),
    })
}

/// Fail closed on partially persisted tokens or foreign Secret names.
/// A reserved generation with no token ID is recoverable only after checking
/// Kanidm labels; never blindly reissue a bearer after a timeout.
pub fn classify_persisted_state(
    uid: &str,
    status: Option<&KanidmRadiusServerStatus>,
) -> Result<PersistedRadiusState, RadiusStateError> {
    Uuid::parse_str(uid).map_err(|_| RadiusStateError::InvalidIdentifier)?;
    let Some(status) = status else {
        return Ok(PersistedRadiusState::Empty);
    };

    let active = match (
        status.active_token_id.as_deref(),
        status.active_token_generation.as_deref(),
        status.active_config_secret.as_deref(),
    ) {
        (None, None, None) => None,
        (Some(id), Some(generation), Some(secret)) => {
            Some(recorded_generation(uid, id, generation, secret)?)
        }
        _ => return Err(RadiusStateError::PartialActive),
    };

    enum Candidate {
        Reserved(String),
        Issued(TokenGeneration),
    }
    let candidate = match (
        status.pending_token_id.as_deref(),
        status.pending_token_generation.as_deref(),
        status.pending_config_secret.as_deref(),
    ) {
        (None, None, None) => None,
        (None, Some(generation), None) => {
            Uuid::parse_str(generation).map_err(|_| RadiusStateError::InvalidIdentifier)?;
            Some(Candidate::Reserved(generation.into()))
        }
        (Some(id), Some(generation), Some(secret)) => Some(Candidate::Issued(recorded_generation(
            uid, id, generation, secret,
        )?)),
        _ => return Err(RadiusStateError::PartialPending),
    };

    if let (Some(active), Some(candidate)) = (&active, &candidate) {
        let generation = match candidate {
            Candidate::Reserved(generation) => generation.as_str(),
            Candidate::Issued(pending) => pending.revision.as_str(),
        };
        if Uuid::parse_str(&active.revision).ok() == Uuid::parse_str(generation).ok()
            || matches!(candidate, Candidate::Issued(pending)
                if pending.token_id == active.token_id
                    || pending.config_secret == active.config_secret)
        {
            return Err(RadiusStateError::DuplicateGeneration);
        }
    }

    Ok(match candidate {
        Some(Candidate::Reserved(generation)) => {
            PersistedRadiusState::Reserved { active, generation }
        }
        Some(Candidate::Issued(pending)) => PersistedRadiusState::Pending { active, pending },
        None => match active {
            Some(active) => PersistedRadiusState::Active(active),
            None => PersistedRadiusState::Empty,
        },
    })
}

/// Declarative reconciliation intent derived from persisted checkpoints.
///
/// Planning is side-effect free: the executor must observe authoritative
/// Kanidm/Kubernetes state before applying any operation. In particular,
/// a reservation is NOT permission to issue another write-only bearer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadiusReconcileIntent {
    EnsureIdentity,
    RecoverReservation,
    /// Inspect Kanidm and the generation Secret before abandoning a lost bearer.
    /// Never revoke a token while any pod may still consume it.
    QuarantineOrphanedGeneration,
    RecoverIssuedGeneration,
    ReconcileActiveGeneration,
    ReconcilePendingGeneration,
}

/// Recovery decision for a write-only bearer after a controller restart.
///
/// A Kanidm token without a corresponding persisted Secret cannot be
/// reconstructed. Quarantine it until a separate drain/ownership check permits
/// revocation; preserve the active generation throughout recovery.
pub fn reserved_generation_recovery(
    token_exists: bool,
    secret_exists: bool,
) -> RadiusReconcileIntent {
    if token_exists && !secret_exists {
        RadiusReconcileIntent::QuarantineOrphanedGeneration
    } else {
        RadiusReconcileIntent::RecoverReservation
    }
}

/// Deterministic next intent for a checkpoint. Repeated observations produce
/// the same result; no random generation or external write happens here.
pub fn reconcile_intent(state: &PersistedRadiusState) -> RadiusReconcileIntent {
    match state {
        PersistedRadiusState::Empty => RadiusReconcileIntent::EnsureIdentity,
        PersistedRadiusState::Reserved { .. } => RadiusReconcileIntent::RecoverReservation,
        PersistedRadiusState::Pending { .. } => RadiusReconcileIntent::RecoverIssuedGeneration,
        PersistedRadiusState::Active(_) => RadiusReconcileIntent::ReconcileActiveGeneration,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticationEvidence {
    /// Must come from an actual successful RADIUS login using this generation.
    pub token_id: String,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotationObservation {
    pub active: TokenGeneration,
    pub pending: Option<TokenGeneration>,
    pub rotation_due: bool,
    /// The candidate's generated Secret is stored and UID-owner-verified.
    pub candidate_secret_present: bool,
    /// Number of pods from the desired (pending) revision that are available.
    pub candidate_ready_pods: u32,
    pub desired_replicas: u32,
    /// Old revision pods still exist. They may still require token A.
    pub previous_revision_pods: u32,
    /// Revision carried by the candidate pods, NOT a Deployment timestamp.
    pub observed_revision: Option<String>,
    /// Evidence from a live Kanidm-backed RADIUS authentication challenge.
    pub authentication: Option<AuthenticationEvidence>,
    /// Previously committed promotion, persisted independently from rotation.
    pub promoted_candidate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationAction {
    Idle,
    MintCandidate,
    PersistCandidateSecret,
    RollCandidate,
    ProbeAuthentication,
    PromoteCandidate,
    WaitForOldPods,
    RevokePrevious,
}

impl RotationObservation {
    /// This guard must be checked again after every reconciliation restart,
    /// even if status claims that a candidate had once been verified.
    pub fn candidate_is_verified(&self) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        self.candidate_secret_present
            && self.desired_replicas > 0
            && self.candidate_ready_pods >= self.desired_replicas
            && self.observed_revision.as_deref() == Some(pending.revision.as_str())
            && self.authentication.as_ref().is_some_and(|proof| {
                proof.token_id == pending.token_id && proof.revision == pending.revision
            })
    }

    /// Determine the next idempotent operation without ever revoking a token
    /// solely on a healthy Deployment status or stale probe evidence.
    pub fn next_action(&self) -> RotationAction {
        let Some(pending) = &self.pending else {
            return if self.rotation_due {
                RotationAction::MintCandidate
            } else {
                RotationAction::Idle
            };
        };
        if !self.candidate_secret_present {
            return RotationAction::PersistCandidateSecret;
        }
        if self.observed_revision.as_deref() != Some(pending.revision.as_str())
            || self.candidate_ready_pods < self.desired_replicas
            || self.desired_replicas == 0
        {
            return RotationAction::RollCandidate;
        }
        if !self.candidate_is_verified() {
            return RotationAction::ProbeAuthentication;
        }
        if !self.promoted_candidate {
            return RotationAction::PromoteCandidate;
        }
        if self.previous_revision_pods != 0 {
            return RotationAction::WaitForOldPods;
        }
        RotationAction::RevokePrevious
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";
    const ACTIVE_GEN: &str = "f1085999-9e32-4a9e-ad8e-3cae381b30c6";
    const PENDING_GEN: &str = "31b30b65-037a-418f-b7ca-0c376da2db28";
    const ACTIVE_ID: &str = "9ad74e68-147f-49e8-96db-dc21c920ff3b";
    const PENDING_ID: &str = "ce1e7a17-7a8f-4fa0-8e30-f66b51f5c37a";

    fn recorded_status() -> KanidmRadiusServerStatus {
        KanidmRadiusServerStatus {
            active_token_id: Some(ACTIVE_ID.into()),
            active_token_generation: Some(ACTIVE_GEN.into()),
            active_config_secret: Some(config_secret_name(UID, ACTIVE_GEN).unwrap()),
            ..Default::default()
        }
    }

    #[test]
    fn orphaned_write_only_token_is_quarantined_not_reissued() {
        assert_eq!(
            reserved_generation_recovery(true, false),
            RadiusReconcileIntent::QuarantineOrphanedGeneration
        );
        assert_eq!(
            reserved_generation_recovery(false, false),
            RadiusReconcileIntent::RecoverReservation
        );
    }

    #[test]
    fn declarative_intent_is_stable_and_never_reissues_reserved_bearers() {
        let reserved = PersistedRadiusState::Reserved {
            active: None,
            generation: PENDING_GEN.into(),
        };
        for _ in 0..3 {
            assert_eq!(
                reconcile_intent(&reserved),
                RadiusReconcileIntent::RecoverReservation
            );
            assert_eq!(
                reconcile_intent(&PersistedRadiusState::Empty),
                RadiusReconcileIntent::EnsureIdentity
            );
        }
        assert_eq!(
            reconcile_intent(&PersistedRadiusState::Active(TokenGeneration {
                token_id: ACTIVE_ID.into(),
                config_secret: config_secret_name(UID, ACTIVE_GEN).unwrap(),
                revision: ACTIVE_GEN.into(),
            })),
            RadiusReconcileIntent::ReconcileActiveGeneration
        );
    }

    #[test]
    fn persisted_state_supports_initial_issue_and_overlap() {
        assert_eq!(
            classify_persisted_state(UID, None),
            Ok(PersistedRadiusState::Empty)
        );
        let mut status = KanidmRadiusServerStatus {
            pending_token_generation: Some(PENDING_GEN.into()),
            ..Default::default()
        };
        assert!(matches!(
            classify_persisted_state(UID, Some(&status)),
            Ok(PersistedRadiusState::Reserved { active: None, .. })
        ));
        status.pending_token_id = Some(PENDING_ID.into());
        status.pending_config_secret = Some(config_secret_name(UID, PENDING_GEN).unwrap());
        assert!(matches!(
            classify_persisted_state(UID, Some(&status)),
            Ok(PersistedRadiusState::Pending { active: None, .. })
        ));
        status = recorded_status();
        assert!(matches!(
            classify_persisted_state(UID, Some(&status)),
            Ok(PersistedRadiusState::Active(_))
        ));
        status.pending_token_generation = Some(PENDING_GEN.into());
        status.pending_token_id = Some(PENDING_ID.into());
        status.pending_config_secret = Some(config_secret_name(UID, PENDING_GEN).unwrap());
        assert!(matches!(
            classify_persisted_state(UID, Some(&status)),
            Ok(PersistedRadiusState::Pending {
                active: Some(_),
                ..
            })
        ));
        status.observed_config_revision = Some("stale-observation".into());
        assert!(classify_persisted_state(UID, Some(&status)).is_ok());
    }

    #[test]
    fn recovery_rejects_partial_foreign_and_duplicate_generations() {
        let mut status = recorded_status();
        status.active_config_secret = None;
        assert_eq!(
            classify_persisted_state(UID, Some(&status)),
            Err(RadiusStateError::PartialActive)
        );
        status = recorded_status();
        status.pending_token_generation = Some(PENDING_GEN.into());
        status.pending_token_id = Some(PENDING_ID.into());
        assert_eq!(
            classify_persisted_state(UID, Some(&status)),
            Err(RadiusStateError::PartialPending)
        );
        status.pending_token_id = None;
        status.pending_token_generation = Some(ACTIVE_GEN.into());
        assert_eq!(
            classify_persisted_state(UID, Some(&status)),
            Err(RadiusStateError::DuplicateGeneration)
        );
        status = recorded_status();
        status.active_config_secret = Some("foreign-secret".into());
        assert_eq!(
            classify_persisted_state(UID, Some(&status)),
            Err(RadiusStateError::SecretOwnershipMismatch)
        );
        status = recorded_status();
        assert_eq!(
            classify_persisted_state("ad8cf3d5-b2a9-44d6-b257-058554ab09d5", Some(&status)),
            Err(RadiusStateError::SecretOwnershipMismatch)
        );
        assert_eq!(
            classify_persisted_state("not-a-uid", Some(&status)),
            Err(RadiusStateError::InvalidIdentifier)
        );
    }

    fn observation() -> RotationObservation {
        RotationObservation {
            active: TokenGeneration {
                token_id: "A".into(),
                config_secret: "config-A".into(),
                revision: "rev-A".into(),
            },
            pending: Some(TokenGeneration {
                token_id: "B".into(),
                config_secret: "config-B".into(),
                revision: "rev-B".into(),
            }),
            rotation_due: true,
            candidate_secret_present: true,
            candidate_ready_pods: 2,
            desired_replicas: 2,
            previous_revision_pods: 1,
            observed_revision: Some("rev-B".into()),
            authentication: Some(AuthenticationEvidence {
                token_id: "B".into(),
                revision: "rev-B".into(),
            }),
            promoted_candidate: false,
        }
    }

    #[test]
    fn cannot_revoke_without_proven_authenticated_candidate() {
        let mut o = observation();
        assert_eq!(o.next_action(), RotationAction::PromoteCandidate);
        o.authentication = None;
        assert_eq!(o.next_action(), RotationAction::ProbeAuthentication);
        assert!(!o.candidate_is_verified());
        o.authentication = Some(AuthenticationEvidence {
            token_id: "A".into(),
            revision: "rev-A".into(),
        });
        assert_eq!(o.next_action(), RotationAction::ProbeAuthentication);
        o.authentication = Some(AuthenticationEvidence {
            token_id: "B".into(),
            revision: "rev-A".into(),
        });
        assert_eq!(o.next_action(), RotationAction::ProbeAuthentication);
    }

    #[test]
    fn deployment_readiness_is_not_authentication() {
        let mut o = observation();
        o.authentication = None;
        o.candidate_ready_pods = 2;
        assert_eq!(o.next_action(), RotationAction::ProbeAuthentication);
        o.observed_revision = Some("rev-A".into());
        assert_eq!(o.next_action(), RotationAction::RollCandidate);
        o.observed_revision = Some("rev-B".into());
        o.candidate_ready_pods = 1;
        assert_eq!(o.next_action(), RotationAction::RollCandidate);
    }

    #[test]
    fn do_not_revoke_while_old_pods_need_token_a() {
        let mut o = observation();
        o.promoted_candidate = true;
        assert_eq!(o.next_action(), RotationAction::WaitForOldPods);
        o.previous_revision_pods = 0;
        assert_eq!(o.next_action(), RotationAction::RevokePrevious);
    }

    #[test]
    fn restart_recovers_pending_candidate_instead_of_minting_again() {
        let mut o = observation();
        o.candidate_secret_present = false;
        assert_eq!(o.next_action(), RotationAction::PersistCandidateSecret);
        o.pending = None;
        assert_eq!(o.next_action(), RotationAction::MintCandidate);
        o.rotation_due = false;
        assert_eq!(o.next_action(), RotationAction::Idle);
    }

    #[test]
    fn stale_or_foreign_probe_never_promotes() {
        let mut o = observation();
        o.authentication = Some(AuthenticationEvidence {
            token_id: "foreign-uid".into(),
            revision: "rev-B".into(),
        });
        assert!(!o.candidate_is_verified());
        assert_eq!(o.next_action(), RotationAction::ProbeAuthentication);
    }
}
