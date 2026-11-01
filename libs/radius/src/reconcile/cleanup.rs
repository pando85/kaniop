//! Finalizer for a `KanidmRadiusServer`.
//!
//! Order (plan, "Finalizer"):
//!
//! 1. Delete our Deployment in the foreground.
//! 2. Wait until no ReplicaSet or pod uses our config Secrets or tokens.
//! 3. Remove our `idm_radius_servers` membership edge and delete the account
//!    (which removes every token on it).
//! 4. Remove the finalizer; owner-reference GC removes the config Secrets and
//!    the Service.
//!
//! Skips (D7): when the referenced Kanidm CR no longer exists, or the account
//! exists without our ownership marker, the Kanidm cleanup is skipped with a
//! Warning event and the finalizer is removed: deletion never blocks forever
//! on something the CR does not own or cannot reach by design. A Kanidm CR
//! that exists but is unreachable is a transient error and retries with
//! backoff.
//!
//! [`next_step`] and [`identity_cleanup`] are pure; the reconciler performs
//! one step per pass and requeues while waiting.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::reconcile::error::{ExecError, ExecResult, Failure};
use crate::reconcile::identity::DeleteOutcome;
use crate::reconcile::status::ConditionKind;

/// What the finalizer sees in one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupView {
    /// Our Deployment, if it still exists.
    pub deployment: Option<DeploymentPhase>,
    /// `replicaset/<name>` or `pod/<name>` still using our Secrets or tokens.
    pub consumers: Vec<String>,
    /// Whether the referenced Kanidm CR exists.
    pub kanidm_present: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeploymentPhase {
    Live,
    /// Deletion started (foreground: it stays until its pods are gone).
    Deleting,
}

/// Why the Kanidm cleanup was skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    KanidmGone,
    AccountNotOwned,
}

impl SkipReason {
    /// Event reason.
    pub fn reason(self) -> &'static str {
        match self {
            Self::KanidmGone => "KanidmNotFound",
            Self::AccountNotOwned => "AccountNotOwned",
        }
    }

    /// Event note; names no credential.
    pub fn note(self) -> &'static str {
        match self {
            Self::KanidmGone => {
                "referenced Kanidm no longer exists; its RADIUS service account was not cleaned up"
            }
            Self::AccountNotOwned => {
                "Kanidm account for this resource exists without its ownership marker; left untouched"
            }
        }
    }
}

/// The next thing the finalizer does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupStep {
    DeleteDeployment,
    AwaitDrain { consumers: Vec<String> },
    DeleteIdentity,
    Skip(SkipReason),
}

/// How the finalizer ends after the identity step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    RemoveFinalizer,
    /// Emit a Warning event, then remove the finalizer.
    SkipAndRemove(SkipReason),
}

/// Decide the next finalizer step. Pure.
pub fn next_step(view: &CleanupView) -> CleanupStep {
    match view.deployment {
        Some(DeploymentPhase::Live) => CleanupStep::DeleteDeployment,
        Some(DeploymentPhase::Deleting) => CleanupStep::AwaitDrain {
            consumers: view.consumers.clone(),
        },
        None if !view.consumers.is_empty() => CleanupStep::AwaitDrain {
            consumers: view.consumers.clone(),
        },
        None if view.kanidm_present => CleanupStep::DeleteIdentity,
        None => CleanupStep::Skip(SkipReason::KanidmGone),
    }
}

/// Event reason while deletion waits for consumers to stop.
pub const DRAIN_WARNING_REASON: &str = "AwaitingConsumers";
/// An unchanged set of consumers is warned about at most this often.
pub const DRAIN_WARNING_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Consumers named in one event note; the rest are counted.
const DRAIN_NOTE_MAX_CONSUMERS: usize = 10;

/// Event note naming the consumers deletion waits for. Bounded: at most
/// [`DRAIN_NOTE_MAX_CONSUMERS`] names, the rest counted.
pub fn drain_note(consumers: &[String]) -> String {
    let mut named = consumers
        .iter()
        .take(DRAIN_NOTE_MAX_CONSUMERS)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let rest = consumers.len().saturating_sub(DRAIN_NOTE_MAX_CONSUMERS);
    if rest > 0 {
        named.push_str(&format!(" and {rest} more"));
    }
    format!("deletion waits until these stop using the config Secrets or tokens: {named}")
}

/// The consumer set each deleting CR (by UID) was last warned about, and
/// when. Kept in memory: the finalizer requeues every few seconds and an
/// event per pass would flood the namespace, while an operator restart only
/// repeats one warning.
#[derive(Debug, Default)]
pub struct DrainWarnings {
    last: Mutex<HashMap<String, (Vec<String>, Instant)>>,
}

impl DrainWarnings {
    /// Whether to warn about `consumers` now: the set changed since the last
    /// warning or [`DRAIN_WARNING_INTERVAL`] passed. Records the warning when
    /// it returns true. An empty set never warns.
    pub fn should_warn(&self, uid: &str, consumers: &[String], now: Instant) -> bool {
        if consumers.is_empty() {
            return false;
        }
        let mut set = consumers.to_vec();
        set.sort();
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        // Entries past the interval decide nothing (they would warn anyway);
        // dropping them bounds the map when a finalizer is removed by hand.
        last.retain(|_, (_, at)| now.saturating_duration_since(*at) < DRAIN_WARNING_INTERVAL);
        let due = last.get(uid).is_none_or(|(previous, at)| {
            *previous != set || now.saturating_duration_since(*at) >= DRAIN_WARNING_INTERVAL
        });
        if due {
            last.insert(uid.to_string(), (set, now));
        }
        due
    }

    /// Drop the record of a CR whose finalizer is done.
    pub fn forget(&self, uid: &str) {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(uid);
    }
}

/// Map the result of `delete_identity` to how the finalizer ends.
/// Transient errors keep their source and retry with backoff.
pub fn identity_cleanup(result: ExecResult<DeleteOutcome>) -> Result<Finish, Failure> {
    match result {
        Ok(DeleteOutcome::Deleted | DeleteOutcome::AlreadyAbsent) => Ok(Finish::RemoveFinalizer),
        Err(ExecError::AccountNotOwned { .. }) => {
            Ok(Finish::SkipAndRemove(SkipReason::AccountNotOwned))
        }
        Err(error) => Err(Failure::exec(ConditionKind::IdentityReady)(error)),
    }
}

#[cfg(test)]
mod tests {
    use kaniop_k8s_util::error::Error;

    use super::*;

    fn view(deployment: Option<DeploymentPhase>, consumers: &[&str], kanidm: bool) -> CleanupView {
        CleanupView {
            deployment,
            consumers: consumers.iter().map(|c| c.to_string()).collect(),
            kanidm_present: kanidm,
        }
    }

    #[test]
    fn steps_run_in_order() {
        use DeploymentPhase::*;
        let cases = [
            // 1. A live Deployment is deleted first, whatever else exists.
            (
                view(Some(Live), &["pod/a"], true),
                CleanupStep::DeleteDeployment,
            ),
            (view(Some(Live), &[], false), CleanupStep::DeleteDeployment),
            // 2. Then wait for the Deployment and every consumer to go.
            (
                view(Some(Deleting), &[], true),
                CleanupStep::AwaitDrain { consumers: vec![] },
            ),
            (
                view(None, &["replicaset/b", "pod/a"], true),
                CleanupStep::AwaitDrain {
                    consumers: vec!["replicaset/b".into(), "pod/a".into()],
                },
            ),
            // Draining comes first even when Kanidm is gone.
            (
                view(None, &["pod/a"], false),
                CleanupStep::AwaitDrain {
                    consumers: vec!["pod/a".into()],
                },
            ),
            // 3. Drained: clean up the Kanidm identity.
            (view(None, &[], true), CleanupStep::DeleteIdentity),
            // D7: Kanidm CR gone, skip with a warning.
            (
                view(None, &[], false),
                CleanupStep::Skip(SkipReason::KanidmGone),
            ),
        ];
        for (view, expected) in cases {
            assert_eq!(next_step(&view), expected, "{view:?}");
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn drain_warnings_only_on_change_or_after_the_interval() {
        let warnings = DrainWarnings::default();
        let t0 = Instant::now();
        let set = names(&["pod/a", "replicaset/b"]);
        assert!(!warnings.should_warn("uid-1", &[], t0), "nothing to name");
        assert!(warnings.should_warn("uid-1", &set, t0), "first time");
        assert!(!warnings.should_warn("uid-1", &set, t0 + Duration::from_secs(5)));
        // Same set in another order is unchanged.
        let reordered = names(&["replicaset/b", "pod/a"]);
        assert!(!warnings.should_warn("uid-1", &reordered, t0 + Duration::from_secs(10)));
        // Another CR is independent.
        assert!(warnings.should_warn("uid-2", &set, t0 + Duration::from_secs(10)));
        // A changed set warns at once.
        let shrunk = names(&["pod/a"]);
        assert!(warnings.should_warn("uid-1", &shrunk, t0 + Duration::from_secs(15)));
        assert!(!warnings.should_warn("uid-1", &shrunk, t0 + Duration::from_secs(20)));
        // An unchanged set warns again once the interval has passed.
        let later = t0 + Duration::from_secs(15) + DRAIN_WARNING_INTERVAL;
        assert!(warnings.should_warn("uid-1", &shrunk, later));
        assert!(!warnings.should_warn("uid-1", &shrunk, later + Duration::from_secs(5)));
        // Forgotten after the finalizer is done.
        warnings.forget("uid-1");
        assert!(warnings.should_warn("uid-1", &shrunk, later + Duration::from_secs(10)));
    }

    #[test]
    fn drain_note_names_consumers_and_is_bounded() {
        let note = drain_note(&names(&["pod/a", "replicaset/b"]));
        assert!(note.contains("pod/a, replicaset/b"), "{note}");
        let many: Vec<String> = (0..25).map(|i| format!("pod/p{i}")).collect();
        let note = drain_note(&many);
        assert!(note.contains("pod/p9"), "{note}");
        assert!(!note.contains("pod/p10"), "{note}");
        assert!(note.contains("and 15 more"), "{note}");
    }

    #[test]
    fn identity_cleanup_outcomes() {
        assert_eq!(
            identity_cleanup(Ok(DeleteOutcome::Deleted)).unwrap(),
            Finish::RemoveFinalizer
        );
        assert_eq!(
            identity_cleanup(Ok(DeleteOutcome::AlreadyAbsent)).unwrap(),
            Finish::RemoveFinalizer
        );
        // D7: an account that is not ours is skipped, never deleted, and does
        // not block deletion.
        assert_eq!(
            identity_cleanup(Err(ExecError::AccountNotOwned {
                account: "kaniop_radius_x".into()
            }))
            .unwrap(),
            Finish::SkipAndRemove(SkipReason::AccountNotOwned)
        );
        // An unreachable Kanidm retries with backoff and keeps its source.
        match identity_cleanup(Err(ExecError::Api(Error::MissingData("down".into())))) {
            Err(Failure::Transient { condition, error }) => {
                assert_eq!(condition, ConditionKind::IdentityReady);
                assert_eq!(error.to_string(), "down");
            }
            other => panic!("expected transient, got {other:?}"),
        }
    }
}
