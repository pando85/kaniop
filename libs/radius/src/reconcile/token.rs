//! Pure token planner for the RADIUS service account.
//!
//! Every reconcile observes the world (tokens on the dedicated Kanidm account,
//! owned config Secrets, the Deployment pod template, ReplicaSets and pods)
//! and [`plan_tokens`] decides which token pods should use, which tokens to
//! revoke and which config Secrets to garbage-collect. The planner performs no
//! I/O and never reads CR status, so losing status cannot change a decision.
//!
//! Safety invariants, each covered by a test below:
//!
//! - The current token, the template's token and every token a consumer
//!   (template, ReplicaSet, pod) references are never revoked.
//! - If any consumer reference cannot be resolved to a token id (unparsable
//!   annotation, or a mounted config Secret that is gone or carries no
//!   parsable token id), nothing is revoked in that pass: the planner cannot
//!   prove which token that consumer holds.
//! - No token younger than [`TokenPolicy::orphan_grace`] is revoked, whether
//!   or not it has an owned Secret. A token minted by a reconcile that crashed
//!   (or is still in flight) before its Secret or Deployment landed is
//!   therefore safe until the grace expires.
//! - A config change alone never mints: the current token is derived from the
//!   template's Secret, not from the rendered content.
//! - Minting never raises the account above [`TokenPolicy::max_tokens`].
//! - Output lists are sorted, so equal inputs give equal plans.
//!
//! Clock assumption: `issued_at` and `expiry` come from the Kanidm server
//! clock and `now` from the operator clock. Skew is assumed to be well below
//! the orphan grace (10 minutes) and the safety margin (1 day). If Kanidm
//! runs ahead, tokens look younger and are held longer (safe direction); if
//! it runs behind, the grace shrinks by the skew. All date arithmetic is
//! checked: an out-of-range result means "not yet" for due and grace checks
//! and "already" for the expiry check, so overflow never panics and always
//! errs toward keeping tokens alive and minting early.
//!
//! Executors must take `revoke` and `gc_secrets` only from a plan computed on
//! a fresh observation made after the Deployment apply (reconcile step 8),
//! never from the plan that decided to mint.

use std::collections::{BTreeMap, BTreeSet};

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::crd::RadiusTokenRotationSpec;

/// Tokens younger than this are never revoked (crash and in-flight safety).
pub const ORPHAN_GRACE: Duration = Duration::minutes(10);
/// Upper bound on tokens on the account; minting stops at this count.
pub const MAX_TOKENS: usize = 4;
/// A token whose expiry falls within this margin is replaced before it lapses.
pub const SAFETY_MARGIN: Duration = Duration::days(1);
/// Longest accepted rotation period (matches the CRD validation rule).
const MAX_PERIOD_DAYS: i64 = 365;

/// A token listed on the dedicated Kanidm account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedToken {
    pub id: Uuid,
    /// Kanidm `ApiToken.issued_at` (Kanidm server clock).
    pub issued_at: OffsetDateTime,
    /// Kanidm `ApiToken.expiry`; `None` never expires.
    pub expiry: Option<OffsetDateTime>,
}

/// An owned config Secret, as read from the API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedSecret {
    pub name: String,
    /// Raw `kaniop.rs/radius-token-id` annotation; parsed by the planner.
    pub token_id: Option<String>,
    /// Raw `kaniop.rs/radius-rotation-request` annotation.
    pub rotation_request: Option<String>,
    /// Whether a non-empty `auth_token` could be read from the Secret data.
    pub bearer_readable: bool,
}

/// What the Deployment pod template points at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplateRef {
    /// Raw `kaniop.rs/radius-token-id` template annotation.
    pub token_id: Option<String>,
    /// Name of the config Secret the template mounts.
    pub config_secret: Option<String>,
}

/// Kind of a running consumer of a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConsumerKind {
    /// A ReplicaSet owned by the Deployment that still has replicas.
    ReplicaSet,
    /// A non-terminal pod (Running, Pending, Unknown or Terminating).
    Pod,
}

/// A ReplicaSet or pod that may hold a token.
///
/// The observer includes every ReplicaSet with replicas and every pod that is
/// not Succeeded/Failed and either carries the token-id annotation or mounts a
/// config Secret it identifies as ours. It lists each such Secret name even if
/// the Secret no longer exists, so the planner can tell the reference is
/// unresolved instead of silently dropping it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consumer {
    pub kind: ConsumerKind,
    pub name: String,
    /// Raw `kaniop.rs/radius-token-id` annotation.
    pub token_id: Option<String>,
    /// Config Secrets mounted by this consumer.
    pub config_secrets: Vec<String>,
}

/// Everything the planner looks at. Deliberately has no CR status field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenWorld {
    pub tokens: Vec<ObservedToken>,
    pub secrets: Vec<ObservedSecret>,
    /// `None` when the Deployment does not exist.
    pub template: Option<TemplateRef>,
    pub consumers: Vec<Consumer>,
    /// Raw `kaniop.rs/radius-rotate-token` annotation on the CR.
    pub rotate_request: Option<String>,
}

/// Rotation and safety parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPolicy {
    /// Rotation period when automatic rotation is enabled.
    pub rotation_period: Option<Duration>,
    pub safety_margin: Duration,
    pub orphan_grace: Duration,
    pub max_tokens: usize,
}

impl TokenPolicy {
    /// Policy for a CR's `tokenRotation`. An out-of-range period (the CRD
    /// rejects it, so only reachable by bypassing validation) is clamped to
    /// 1..=365 days.
    pub fn from_spec(rotation: Option<&RadiusTokenRotationSpec>) -> Self {
        let rotation_period = rotation
            .filter(|r| r.enabled)
            .map(|r| Duration::days(i64::from(r.period_days).clamp(1, MAX_PERIOD_DAYS)));
        Self {
            rotation_period,
            safety_margin: SAFETY_MARGIN,
            orphan_grace: ORPHAN_GRACE,
            max_tokens: MAX_TOKENS,
        }
    }

    /// Expiry to request for a token minted at `now`: `now + 2 x period` when
    /// rotation is enabled (D3), otherwise none.
    pub fn new_token_expiry(&self, now: OffsetDateTime) -> Option<OffsetDateTime> {
        let period = self.rotation_period?;
        now.checked_add(period.checked_mul(2)?)
    }
}

/// Why a new token is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintReason {
    /// No Deployment template, or the template references no token at all.
    Initial,
    /// The template's token cannot be used: its config Secret is missing, has
    /// no readable bearer or no parsable token id, or the token is no longer
    /// on the Kanidm account (revoked out of band).
    BearerLost,
    /// The token has expired or expires within the safety margin.
    Expired,
    /// `issued_at + periodDays` has passed.
    Due,
    /// The CR rotate annotation differs from the value on the config Secret.
    Requested,
}

impl MintReason {
    /// Stable reason string for conditions and events.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Initial => "Initial",
            Self::BearerLost => "BearerLost",
            Self::Expired => "Expired",
            Self::Due => "Due",
            Self::Requested => "Requested",
        }
    }
}

/// Why a needed mint is withheld.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockReason {
    /// The account already holds `max_tokens` tokens.
    TooManyTokens,
}

impl BlockReason {
    /// Stable reason string for conditions and events.
    pub fn reason(self) -> &'static str {
        match self {
            Self::TooManyTokens => "TooManyTokens",
        }
    }
}

/// The token pods should run with after this reconcile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurrentToken {
    Keep(Uuid),
    Mint(MintReason),
    Blocked(BlockReason),
}

/// Planner output. All lists are sorted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPlan {
    pub current: CurrentToken,
    /// Tokens to revoke.
    pub revoke: Vec<Uuid>,
    /// Owned config Secrets nothing references.
    pub gc_secrets: Vec<String>,
    /// Tokens on the account kept but not current (`status.retiringTokenIds`).
    pub retiring: Vec<Uuid>,
    /// Consumers whose token could not be resolved; while non-empty nothing
    /// is revoked. Entries are `template`, `replicaset/<name>` or `pod/<name>`.
    pub unresolved_consumers: Vec<String>,
}

/// Decide token actions from the observed world. Pure and deterministic.
pub fn plan_tokens(world: &TokenWorld, policy: &TokenPolicy, now: OffsetDateTime) -> TokenPlan {
    let tokens: BTreeMap<Uuid, &ObservedToken> = world.tokens.iter().map(|t| (t.id, t)).collect();
    let secrets: BTreeMap<&str, &ObservedSecret> =
        world.secrets.iter().map(|s| (s.name.as_str(), s)).collect();

    let usage = resolve_usage(world, &secrets);

    let current = match current_token(world, policy, now, &tokens, &secrets) {
        Ok(id) => CurrentToken::Keep(id),
        Err(_) if tokens.len() >= policy.max_tokens => {
            CurrentToken::Blocked(BlockReason::TooManyTokens)
        }
        Err(reason) => CurrentToken::Mint(reason),
    };
    let keep_id = match current {
        CurrentToken::Keep(id) => Some(id),
        CurrentToken::Mint(_) | CurrentToken::Blocked(_) => None,
    };
    let young = |t: &ObservedToken| !reached(t.issued_at, policy.orphan_grace, now);

    let mut revoke = Vec::new();
    let mut retiring = Vec::new();
    for (id, t) in &tokens {
        if Some(*id) == keep_id {
            continue;
        }
        if usage.unresolved.is_empty() && !usage.in_use.contains(id) && !young(t) {
            revoke.push(*id);
        } else {
            retiring.push(*id);
        }
    }

    let gc_secrets = secrets
        .values()
        .filter(|s| !usage.referenced_secrets.contains(s.name.as_str()))
        .filter(|s| {
            // A Secret of a token still inside the grace may belong to an
            // in-flight rollout; keep it until the token ages out.
            parse_id(s.token_id.as_deref())
                .and_then(|id| tokens.get(&id))
                .is_none_or(|t| !young(t))
        })
        .map(|s| s.name.clone())
        .collect();

    TokenPlan {
        current,
        revoke,
        gc_secrets,
        retiring,
        unresolved_consumers: usage.unresolved.into_iter().collect(),
    }
}

/// Tokens and Secrets referenced by the template, ReplicaSets and pods.
struct Usage<'a> {
    in_use: BTreeSet<Uuid>,
    referenced_secrets: BTreeSet<&'a str>,
    unresolved: BTreeSet<String>,
}

fn resolve_usage<'a>(
    world: &'a TokenWorld,
    secrets: &BTreeMap<&str, &ObservedSecret>,
) -> Usage<'a> {
    let mut usage = Usage {
        in_use: BTreeSet::new(),
        referenced_secrets: BTreeSet::new(),
        unresolved: BTreeSet::new(),
    };
    let template = world.template.as_ref().map(|t| {
        (
            "template".to_string(),
            t.token_id.as_deref(),
            t.config_secret
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )
    });
    let consumers = world.consumers.iter().map(|c| {
        let kind = match c.kind {
            ConsumerKind::ReplicaSet => "replicaset",
            ConsumerKind::Pod => "pod",
        };
        (
            format!("{kind}/{}", c.name),
            c.token_id.as_deref(),
            c.config_secrets.iter().map(String::as_str).collect(),
        )
    });
    for (label, annotation, mounted) in template.into_iter().chain(consumers) {
        // Every reference must resolve; one that does not means the consumer
        // may hold a token we cannot name.
        let mut resolved_any = false;
        let mut all_resolved = true;
        let ids = annotation
            .map(|raw| Uuid::parse_str(raw).ok())
            .into_iter()
            .chain(mounted.iter().map(|name| {
                usage.referenced_secrets.insert(name);
                secrets
                    .get(name)
                    .and_then(|s| parse_id(s.token_id.as_deref()))
            }));
        for id in ids {
            match id {
                Some(id) => {
                    usage.in_use.insert(id);
                    resolved_any = true;
                }
                None => all_resolved = false,
            }
        }
        if !(resolved_any && all_resolved) {
            usage.unresolved.insert(label);
        }
    }
    usage
}

/// The template's token if it can stay, else why a new one is needed.
///
/// The template's config Secret is authoritative: its token-id annotation was
/// written together with the bearer the pods load. The template's own
/// annotation only feeds the in-use set.
fn current_token(
    world: &TokenWorld,
    policy: &TokenPolicy,
    now: OffsetDateTime,
    tokens: &BTreeMap<Uuid, &ObservedToken>,
    secrets: &BTreeMap<&str, &ObservedSecret>,
) -> Result<Uuid, MintReason> {
    let template = world.template.as_ref().ok_or(MintReason::Initial)?;
    let Some(secret_name) = template.config_secret.as_deref() else {
        return Err(match template.token_id {
            None => MintReason::Initial,
            Some(_) => MintReason::BearerLost,
        });
    };
    let secret = secrets
        .get(secret_name)
        .filter(|s| s.bearer_readable)
        .ok_or(MintReason::BearerLost)?;
    let id = parse_id(secret.token_id.as_deref()).ok_or(MintReason::BearerLost)?;
    let token = tokens.get(&id).ok_or(MintReason::BearerLost)?;

    let expiring = token.expiry.is_some_and(|expiry| {
        expiry
            .checked_sub(policy.safety_margin)
            .is_none_or(|limit| now >= limit)
    });
    if expiring {
        return Err(MintReason::Expired);
    }
    if policy
        .rotation_period
        .is_some_and(|period| reached(token.issued_at, period, now))
    {
        return Err(MintReason::Due);
    }
    let requested = non_empty(world.rotate_request.as_deref())
        .is_some_and(|wanted| non_empty(secret.rotation_request.as_deref()) != Some(wanted));
    if requested {
        return Err(MintReason::Requested);
    }
    Ok(id)
}

/// `start + span <= now`; an out-of-range sum is "not yet".
fn reached(start: OffsetDateTime, span: Duration, now: OffsetDateTime) -> bool {
    start.checked_add(span).is_some_and(|at| now >= at)
}

fn parse_id(raw: Option<&str>) -> Option<Uuid> {
    raw.and_then(|r| Uuid::parse_str(r).ok())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Uuid = Uuid::from_u128(0xa);
    const B: Uuid = Uuid::from_u128(0xb);
    const C: Uuid = Uuid::from_u128(0xc);
    const D: Uuid = Uuid::from_u128(0xd);
    const E: Uuid = Uuid::from_u128(0xe);

    fn now() -> OffsetDateTime {
        // 2026-10-31T12:00:00Z
        OffsetDateTime::from_unix_timestamp(1_793_448_000).expect("valid timestamp")
    }

    fn ago(d: Duration) -> OffsetDateTime {
        now() - d
    }

    fn token(id: Uuid, issued: OffsetDateTime) -> ObservedToken {
        ObservedToken {
            id,
            issued_at: issued,
            expiry: None,
        }
    }

    fn token_exp(id: Uuid, issued: OffsetDateTime, expiry: OffsetDateTime) -> ObservedToken {
        ObservedToken {
            id,
            issued_at: issued,
            expiry: Some(expiry),
        }
    }

    fn secret(name: &str, id: Uuid) -> ObservedSecret {
        ObservedSecret {
            name: name.to_string(),
            token_id: Some(id.to_string()),
            rotation_request: None,
            bearer_readable: true,
        }
    }

    fn template(id: Uuid, secret: &str) -> Option<TemplateRef> {
        Some(TemplateRef {
            token_id: Some(id.to_string()),
            config_secret: Some(secret.to_string()),
        })
    }

    fn pod(name: &str, id: Option<&str>, secrets: &[&str]) -> Consumer {
        Consumer {
            kind: ConsumerKind::Pod,
            name: name.to_string(),
            token_id: id.map(str::to_string),
            config_secrets: secrets.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn rs(name: &str, id: Uuid, secret: &str) -> Consumer {
        Consumer {
            kind: ConsumerKind::ReplicaSet,
            name: name.to_string(),
            token_id: Some(id.to_string()),
            config_secrets: vec![secret.to_string()],
        }
    }

    fn rotation(days: i32) -> TokenPolicy {
        TokenPolicy::from_spec(Some(&RadiusTokenRotationSpec {
            enabled: true,
            period_days: days,
        }))
    }

    fn no_rotation() -> TokenPolicy {
        TokenPolicy::from_spec(None)
    }

    /// Steady state: template, ReplicaSet and pod all on token A via `s-a`.
    fn steady(issued: OffsetDateTime) -> TokenWorld {
        TokenWorld {
            tokens: vec![token(A, issued)],
            secrets: vec![secret("s-a", A)],
            template: template(A, "s-a"),
            consumers: vec![
                rs("rs-a", A, "s-a"),
                pod("p-a", Some(&A.to_string()), &["s-a"]),
            ],
            rotate_request: None,
        }
    }

    struct Case {
        name: &'static str,
        world: TokenWorld,
        policy: TokenPolicy,
        current: CurrentToken,
        revoke: Vec<Uuid>,
        gc: Vec<&'static str>,
    }

    fn cases() -> Vec<Case> {
        let old = ago(Duration::days(3));
        vec![
            Case {
                name: "empty world mints Initial",
                world: TokenWorld::default(),
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::Initial),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "status loss with valid template token keeps it",
                world: steady(old),
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "config change keeps the token while old pods drain",
                world: TokenWorld {
                    tokens: vec![token(A, old)],
                    secrets: vec![secret("s-a1", A), secret("s-a2", A)],
                    template: template(A, "s-a2"),
                    consumers: vec![
                        rs("rs-old", A, "s-a1"),
                        pod("p-old", Some(&A.to_string()), &["s-a1"]),
                        pod("p-new", Some(&A.to_string()), &["s-a2"]),
                    ],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "config change after drain collects old Secret, never the token",
                world: TokenWorld {
                    tokens: vec![token(A, old)],
                    secrets: vec![secret("s-a1", A), secret("s-a2", A)],
                    template: template(A, "s-a2"),
                    consumers: vec![pod("p-new", Some(&A.to_string()), &["s-a2"])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec!["s-a1"],
            },
            Case {
                name: "rotation due mints Due",
                world: steady(ago(Duration::days(31))),
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::Due),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "rotation due exactly at the period boundary",
                world: steady(ago(Duration::days(30))),
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::Due),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "one second before due keeps",
                world: steady(ago(Duration::days(30) - Duration::seconds(1))),
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "expired token mints Expired",
                world: TokenWorld {
                    tokens: vec![token_exp(A, old, ago(Duration::hours(1)))],
                    ..steady(old)
                },
                policy: no_rotation(),
                current: CurrentToken::Mint(MintReason::Expired),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "expiry within safety margin mints Expired",
                world: TokenWorld {
                    tokens: vec![token_exp(A, old, now() + Duration::hours(23))],
                    ..steady(old)
                },
                policy: no_rotation(),
                current: CurrentToken::Mint(MintReason::Expired),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "expiry beyond safety margin keeps",
                world: TokenWorld {
                    tokens: vec![token_exp(A, old, now() + Duration::hours(25))],
                    ..steady(old)
                },
                policy: no_rotation(),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "rotate annotation differing from Secret mints Requested",
                world: TokenWorld {
                    rotate_request: Some("leak-2026-10-31".into()),
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::Requested),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "rotate annotation equal to Secret keeps",
                world: TokenWorld {
                    secrets: vec![ObservedSecret {
                        rotation_request: Some("leak-1".into()),
                        ..secret("s-a", A)
                    }],
                    rotate_request: Some("leak-1".into()),
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "removing the rotate annotation does not rotate",
                world: TokenWorld {
                    secrets: vec![ObservedSecret {
                        rotation_request: Some("leak-1".into()),
                        ..secret("s-a", A)
                    }],
                    rotate_request: None,
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "unreadable bearer mints BearerLost",
                world: TokenWorld {
                    secrets: vec![ObservedSecret {
                        bearer_readable: false,
                        ..secret("s-a", A)
                    }],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::BearerLost),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "deleted config Secret mints BearerLost and holds the token",
                world: TokenWorld {
                    secrets: vec![],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::BearerLost),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "token revoked out of band mints BearerLost; dangling Secret kept while used",
                world: TokenWorld {
                    tokens: vec![],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::BearerLost),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "dangling Secret of an out-of-band revoked token is collected once unused",
                world: TokenWorld {
                    tokens: vec![token(B, old)],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![pod("p-b", Some(&B.to_string()), &["s-b"])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec!["s-a"],
            },
            Case {
                name: "old token held while only the template references it",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    template: Some(TemplateRef {
                        token_id: Some(A.to_string()),
                        config_secret: Some("s-b".into()),
                    }),
                    consumers: vec![],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec!["s-a"],
            },
            Case {
                name: "old token held while a ReplicaSet uses it",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![rs("rs-a", A, "s-a")],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "old token held while a Terminating pod mounts its Secret (no annotation)",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![pod("p-a-terminating", None, &["s-a"])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "old token held while a pod carries only its annotation",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![pod("p-a", Some(&A.to_string()), &[])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "two tokens both referenced by pods during rollout are both held",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, ago(Duration::minutes(1)))],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![
                        rs("rs-a", A, "s-a"),
                        rs("rs-b", B, "s-b"),
                        pod("p-a", Some(&A.to_string()), &["s-a"]),
                        pod("p-b", Some(&B.to_string()), &["s-b"]),
                    ],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "retired token revoked once unused and its Secret collected",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, ago(Duration::hours(1)))],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![pod("p-b", Some(&B.to_string()), &["s-b"])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![A],
                gc: vec!["s-a"],
            },
            Case {
                name: "orphan younger than grace is kept",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, ago(Duration::minutes(9)))],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "orphan exactly at grace is revoked",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, ago(Duration::minutes(10)))],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![B],
                gc: vec![],
            },
            Case {
                name: "orphan older than grace is revoked",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, ago(Duration::hours(2)))],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![B],
                gc: vec![],
            },
            Case {
                name: "young retired token and its Secret from a crash before apply are held",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, ago(Duration::minutes(2)))],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "orphan issued in the future (Kanidm clock ahead) is kept",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, now() + Duration::minutes(5))],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "more than max tokens blocks minting but still revokes unused ones",
                world: TokenWorld {
                    tokens: vec![
                        token(A, ago(Duration::days(40))),
                        token(B, old),
                        token(C, old),
                        token(D, ago(Duration::minutes(1))),
                        token(E, ago(Duration::minutes(1))),
                    ],
                    ..steady(ago(Duration::days(40)))
                },
                policy: rotation(30),
                current: CurrentToken::Blocked(BlockReason::TooManyTokens),
                revoke: vec![B, C],
                gc: vec![],
            },
            Case {
                name: "exactly max tokens blocks a mint that would exceed it",
                world: TokenWorld {
                    tokens: vec![
                        token(A, old),
                        token(B, ago(Duration::minutes(1))),
                        token(C, ago(Duration::minutes(1))),
                        token(D, ago(Duration::minutes(1))),
                    ],
                    rotate_request: Some("leak".into()),
                    ..steady(old)
                },
                policy: no_rotation(),
                current: CurrentToken::Blocked(BlockReason::TooManyTokens),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "more than max tokens does not block a Keep",
                world: TokenWorld {
                    tokens: vec![
                        token(A, old),
                        token(B, ago(Duration::minutes(1))),
                        token(C, ago(Duration::minutes(1))),
                        token(D, ago(Duration::minutes(1))),
                        token(E, ago(Duration::minutes(1))),
                    ],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "current never revoked even when template annotation names another token",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![secret("s-b", B)],
                    template: Some(TemplateRef {
                        token_id: Some(A.to_string()),
                        config_secret: Some("s-b".into()),
                    }),
                    consumers: vec![pod("p-b", Some(&A.to_string()), &["s-b"])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "current never revoked when a stray Secret claims it and nobody mounts it",
                world: TokenWorld {
                    tokens: vec![token(A, old)],
                    secrets: vec![secret("s-a", A), secret("stray", A)],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec!["stray"],
            },
            Case {
                name: "unparsable pod annotation blocks every revocation",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![secret("s-a", A), secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![pod("p-weird", Some("not-a-uuid"), &[])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec!["s-a"],
            },
            Case {
                name: "pod mounting a Secret with an unparsable token id blocks every revocation",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![
                        ObservedSecret {
                            token_id: Some("garbage".into()),
                            ..secret("s-x", A)
                        },
                        secret("s-b", B),
                    ],
                    template: template(B, "s-b"),
                    consumers: vec![pod("p-x", None, &["s-x"])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "pod mounting a deleted config Secret blocks every revocation",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old), token(C, old)],
                    secrets: vec![secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![pod("p-gone", Some(&A.to_string()), &["s-a-deleted"])],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "non-canonical UUID spelling in a pod annotation still holds the token",
                world: TokenWorld {
                    tokens: vec![token(A, old), token(B, old)],
                    secrets: vec![secret("s-b", B)],
                    template: template(B, "s-b"),
                    consumers: vec![pod(
                        "p-a",
                        Some(&A.simple().to_string().to_uppercase()),
                        &[],
                    )],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Keep(B),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "unparsable token id on the template Secret mints BearerLost",
                world: TokenWorld {
                    secrets: vec![ObservedSecret {
                        token_id: Some("nope".into()),
                        ..secret("s-a", A)
                    }],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::BearerLost),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "template with a token annotation but no Secret mints BearerLost",
                world: TokenWorld {
                    tokens: vec![token(A, old)],
                    secrets: vec![],
                    template: Some(TemplateRef {
                        token_id: Some(A.to_string()),
                        config_secret: None,
                    }),
                    consumers: vec![],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::BearerLost),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "template without any reference mints Initial and revokes nothing",
                world: TokenWorld {
                    tokens: vec![token(A, old)],
                    secrets: vec![],
                    template: Some(TemplateRef::default()),
                    consumers: vec![],
                    rotate_request: None,
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::Initial),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "deployment gone mints Initial and holds tokens pods still use",
                world: TokenWorld {
                    template: None,
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::Initial),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "rotation disabled and no expiry keeps forever",
                world: steady(ago(Duration::days(3650))),
                policy: no_rotation(),
                current: CurrentToken::Keep(A),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "expired takes precedence over due",
                world: TokenWorld {
                    tokens: vec![token_exp(
                        A,
                        ago(Duration::days(61)),
                        ago(Duration::days(1)),
                    )],
                    ..steady(ago(Duration::days(61)))
                },
                policy: rotation(30),
                current: CurrentToken::Mint(MintReason::Expired),
                revoke: vec![],
                gc: vec![],
            },
            Case {
                name: "expired unused tokens are revoked",
                world: TokenWorld {
                    tokens: vec![
                        token(A, old),
                        token_exp(B, ago(Duration::days(70)), ago(Duration::days(10))),
                    ],
                    ..steady(old)
                },
                policy: rotation(30),
                current: CurrentToken::Keep(A),
                revoke: vec![B],
                gc: vec![],
            },
        ]
    }

    #[test]
    fn planner_table() {
        let mut failures = Vec::new();
        for case in cases() {
            let plan = plan_tokens(&case.world, &case.policy, now());
            let gc: Vec<String> = case.gc.iter().map(|s| s.to_string()).collect();
            if plan.current != case.current || plan.revoke != case.revoke || plan.gc_secrets != gc {
                failures.push(format!(
                    "{}:\n  expected current={:?} revoke={:?} gc={:?}\n  got      current={:?} revoke={:?} gc={:?}",
                    case.name,
                    case.current,
                    case.revoke,
                    gc,
                    plan.current,
                    plan.revoke,
                    plan.gc_secrets
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Cross-cutting invariants checked on every table world.
    #[test]
    fn invariants_hold_for_every_case() {
        for case in cases() {
            let plan = plan_tokens(&case.world, &case.policy, now());
            let revoke: BTreeSet<Uuid> = plan.revoke.iter().copied().collect();
            if let CurrentToken::Keep(id) = plan.current {
                assert!(!revoke.contains(&id), "{}: current revoked", case.name);
            }
            let mut referenced: Vec<&Option<String>> =
                case.world.consumers.iter().map(|c| &c.token_id).collect();
            if let Some(t) = &case.world.template {
                referenced.push(&t.token_id);
            }
            for raw in referenced.into_iter().flatten() {
                if let Ok(id) = Uuid::parse_str(raw) {
                    assert!(!revoke.contains(&id), "{}: in-use {id} revoked", case.name);
                }
            }
            for t in &case.world.tokens {
                if revoke.contains(&t.id) {
                    assert!(
                        now() - t.issued_at >= ORPHAN_GRACE,
                        "{}: young token revoked",
                        case.name
                    );
                }
            }
            if !plan.unresolved_consumers.is_empty() {
                assert!(
                    plan.revoke.is_empty(),
                    "{}: revoked while unresolved",
                    case.name
                );
            }
            if matches!(plan.current, CurrentToken::Mint(_)) {
                assert!(
                    case.world.tokens.len() < case.policy.max_tokens,
                    "{}: mint would exceed max tokens",
                    case.name
                );
            }
            let mut sorted = plan.revoke.clone();
            sorted.sort();
            assert_eq!(sorted, plan.revoke, "{}: revoke unsorted", case.name);
            let mut sorted = plan.gc_secrets.clone();
            sorted.sort();
            assert_eq!(sorted, plan.gc_secrets, "{}: gc unsorted", case.name);
        }
    }

    #[test]
    fn output_is_independent_of_input_order() {
        for case in cases() {
            let mut reversed = case.world.clone();
            reversed.tokens.reverse();
            reversed.secrets.reverse();
            reversed.consumers.reverse();
            assert_eq!(
                plan_tokens(&case.world, &case.policy, now()),
                plan_tokens(&reversed, &case.policy, now()),
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn retiring_lists_kept_non_current_tokens() {
        let world = TokenWorld {
            tokens: vec![
                token(A, ago(Duration::days(3))),
                token(B, ago(Duration::hours(1))),
            ],
            secrets: vec![secret("s-a", A), secret("s-b", B)],
            template: template(B, "s-b"),
            consumers: vec![rs("rs-a", A, "s-a")],
            rotate_request: None,
        };
        let plan = plan_tokens(&world, &rotation(30), now());
        assert_eq!(plan.current, CurrentToken::Keep(B));
        assert_eq!(plan.retiring, vec![A]);
        assert!(plan.unresolved_consumers.is_empty());
    }

    #[test]
    fn unresolved_consumers_are_reported_sorted() {
        let world = TokenWorld {
            tokens: vec![token(A, ago(Duration::days(3)))],
            secrets: vec![],
            template: Some(TemplateRef {
                token_id: Some("bad".into()),
                config_secret: None,
            }),
            consumers: vec![
                pod("z", Some("bad"), &[]),
                pod("a", None, &["missing"]),
                Consumer {
                    kind: ConsumerKind::ReplicaSet,
                    name: "r".into(),
                    token_id: None,
                    config_secrets: vec![],
                },
            ],
            rotate_request: None,
        };
        let plan = plan_tokens(&world, &rotation(30), now());
        assert_eq!(
            plan.unresolved_consumers,
            vec!["pod/a", "pod/z", "replicaset/r", "template"]
        );
        assert!(plan.revoke.is_empty());
    }

    #[test]
    fn duplicate_token_entries_count_once() {
        let mut world = steady(ago(Duration::days(40)));
        world.tokens = vec![
            token(A, ago(Duration::days(40))),
            token(A, ago(Duration::days(40))),
            token(B, ago(Duration::minutes(1))),
            token(B, ago(Duration::minutes(1))),
            token(C, ago(Duration::minutes(1))),
            token(C, ago(Duration::minutes(1))),
        ];
        let plan = plan_tokens(&world, &rotation(30), now());
        assert_eq!(plan.current, CurrentToken::Mint(MintReason::Due));
    }

    #[test]
    fn extreme_dates_do_not_panic() {
        let world = TokenWorld {
            tokens: vec![
                token_exp(
                    A,
                    OffsetDateTime::new_utc(time::Date::MIN, time::Time::MIDNIGHT),
                    OffsetDateTime::new_utc(time::Date::MIN, time::Time::MIDNIGHT),
                ),
                token(
                    B,
                    OffsetDateTime::new_utc(time::Date::MAX, time::Time::MIDNIGHT),
                ),
            ],
            secrets: vec![secret("s-b", B)],
            template: template(B, "s-b"),
            consumers: vec![],
            rotate_request: None,
        };
        let plan = plan_tokens(&world, &rotation(365), now());
        // Far-future issued_at: never due, never old enough to revoke.
        assert_eq!(plan.current, CurrentToken::Keep(B));
        // Ancient expired token is unused and old: revoked.
        assert_eq!(plan.revoke, vec![A]);

        let world = TokenWorld {
            tokens: vec![token_exp(
                A,
                ago(Duration::days(1)),
                OffsetDateTime::new_utc(time::Date::MIN, time::Time::MIDNIGHT),
            )],
            ..steady(ago(Duration::days(1)))
        };
        assert_eq!(
            plan_tokens(&world, &no_rotation(), now()).current,
            CurrentToken::Mint(MintReason::Expired)
        );
        let max = OffsetDateTime::new_utc(time::Date::MAX, time::Time::MIDNIGHT);
        assert_eq!(rotation(365).new_token_expiry(max), None);
    }

    #[test]
    fn policy_from_spec() {
        assert_eq!(no_rotation().rotation_period, None);
        assert_eq!(
            TokenPolicy::from_spec(Some(&RadiusTokenRotationSpec {
                enabled: false,
                period_days: 7,
            }))
            .rotation_period,
            None
        );
        assert_eq!(rotation(7).rotation_period, Some(Duration::days(7)));
        assert_eq!(rotation(0).rotation_period, Some(Duration::days(1)));
        assert_eq!(
            rotation(i32::MAX).rotation_period,
            Some(Duration::days(365))
        );
        assert_eq!(no_rotation().orphan_grace, Duration::minutes(10));
        assert_eq!(no_rotation().max_tokens, 4);
    }

    #[test]
    fn new_token_expiry_is_twice_the_period() {
        assert_eq!(no_rotation().new_token_expiry(now()), None);
        assert_eq!(
            rotation(30).new_token_expiry(now()),
            Some(now() + Duration::days(60))
        );
    }

    /// With rotation enabled, a freshly minted token becomes Due no later than
    /// it enters the expiry safety margin, for every allowed period.
    #[test]
    fn due_always_precedes_expiry_margin() {
        for days in 1..=365 {
            let policy = rotation(days);
            let issued = now();
            let expiry = policy.new_token_expiry(issued).expect("expiry");
            let world = TokenWorld {
                tokens: vec![token_exp(A, issued, expiry)],
                ..steady(issued)
            };
            let at_due = issued + Duration::days(i64::from(days));
            assert_eq!(
                plan_tokens(&world, &policy, at_due - Duration::seconds(1)).current,
                CurrentToken::Keep(A),
                "period {days}"
            );
            let at_margin = expiry - policy.safety_margin;
            assert!(at_due <= at_margin, "period {days}");
        }
    }

    #[test]
    fn reasons_are_stable() {
        assert_eq!(MintReason::Initial.reason(), "Initial");
        assert_eq!(MintReason::BearerLost.reason(), "BearerLost");
        assert_eq!(MintReason::Expired.reason(), "Expired");
        assert_eq!(MintReason::Due.reason(), "Due");
        assert_eq!(MintReason::Requested.reason(), "Requested");
        assert_eq!(BlockReason::TooManyTokens.reason(), "TooManyTokens");
    }
}
