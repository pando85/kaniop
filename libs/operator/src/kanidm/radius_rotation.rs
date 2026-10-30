//! Crash-resilient RADIUS token-rotation planning.
//!
//! This does not grant permission to revoke a token by itself. The controller
//! must persist the state transitions and obtain a *real* authentication probe
//! for the exact pending token/revision before executing RevokePrevious.
//! Deployment.readyReplicas alone is NEVER valid proof of token usability.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenGeneration {
    /// Kanidm token ID; never the bearer token value.
    pub token_id: String,
    /// UID-owned, revision-addressed Kubernetes config Secret name.
    pub config_secret: String,
    pub revision: String,
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
            && self
                .authentication
                .as_ref()
                .is_some_and(|proof| proof.token_id == pending.token_id && proof.revision == pending.revision)
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
