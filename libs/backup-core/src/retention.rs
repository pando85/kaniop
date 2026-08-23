use chrono::{Datelike, NaiveDateTime};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub keep_last: u32,
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
    pub min_age_hours: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            keep_last: 8,
            daily: 7,
            weekly: 4,
            monthly: 12,
            min_age_hours: 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    pub id: String,
    pub created_at: NaiveDateTime,
    pub consistency: String,
    pub reason: String,
    pub referenced_by_active_restore: bool,
    pub safety_backup_min_retention_hours: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionResult {
    pub retain: Vec<String>,
    pub delete: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionReason {
    pub backup_id: String,
    pub reason: String,
}

pub fn select_deletion_candidates(
    entries: &[BackupEntry],
    policy: &RetentionPolicy,
    now: &NaiveDateTime,
) -> RetentionResult {
    let mut retain_ids: HashSet<String> = HashSet::new();
    let mut retain_reasons: Vec<RetentionReason> = Vec::new();

    let mut sorted: Vec<&BackupEntry> = entries.iter().collect();
    sorted.sort_by_key(|a| std::cmp::Reverse(a.created_at));

    for entry in &sorted {
        if entry.referenced_by_active_restore {
            retain_ids.insert(entry.id.clone());
            retain_reasons.push(RetentionReason {
                backup_id: entry.id.clone(),
                reason: "active-restore".to_string(),
            });
        }
    }

    for entry in &sorted {
        let age_hours = (*now - entry.created_at).num_hours();
        if age_hours < policy.min_age_hours as i64 {
            retain_ids.insert(entry.id.clone());
            retain_reasons.push(RetentionReason {
                backup_id: entry.id.clone(),
                reason: "min-age".to_string(),
            });
        }
    }

    for entry in &sorted {
        if let Some(safety_hours) = entry.safety_backup_min_retention_hours {
            if entry.reason == "restore-safety" {
                let age_hours = (*now - entry.created_at).num_hours();
                if age_hours < safety_hours as i64 {
                    retain_ids.insert(entry.id.clone());
                    retain_reasons.push(RetentionReason {
                        backup_id: entry.id.clone(),
                        reason: "safety-retention".to_string(),
                    });
                }
            }
        }
    }

    let keep_last_ids: Vec<String> = sorted
        .iter()
        .take(policy.keep_last as usize)
        .map(|e| e.id.clone())
        .collect();
    for id in &keep_last_ids {
        if retain_ids.insert(id.clone()) {
            retain_reasons.push(RetentionReason {
                backup_id: id.clone(),
                reason: "keep-last".to_string(),
            });
        }
    }

    let daily_retained = retain_by_bucket(
        &sorted,
        policy.daily,
        |dt| dt.date().num_days_from_ce(),
        now,
    );
    for id in daily_retained {
        if retain_ids.insert(id.clone()) {
            retain_reasons.push(RetentionReason {
                backup_id: id,
                reason: "daily".to_string(),
            });
        }
    }

    let weekly_retained = retain_by_weekly_bucket(&sorted, policy.weekly, now);
    for id in weekly_retained {
        if retain_ids.insert(id.clone()) {
            retain_reasons.push(RetentionReason {
                backup_id: id,
                reason: "weekly".to_string(),
            });
        }
    }

    let monthly_retained = retain_by_monthly_bucket(&sorted, policy.monthly, now);
    for id in monthly_retained {
        if retain_ids.insert(id.clone()) {
            retain_reasons.push(RetentionReason {
                backup_id: id,
                reason: "monthly".to_string(),
            });
        }
    }

    let mut retain = Vec::new();
    let mut delete = Vec::new();
    for entry in &sorted {
        if retain_ids.contains(&entry.id) {
            retain.push(entry.id.clone());
        } else {
            delete.push(entry.id.clone());
        }
    }

    RetentionResult { retain, delete }
}

fn retain_by_bucket<F>(
    sorted: &[&BackupEntry],
    count: u32,
    bucket_fn: F,
    _now: &NaiveDateTime,
) -> Vec<String>
where
    F: Fn(&NaiveDateTime) -> i32,
{
    let mut seen_buckets: BTreeMap<i32, String> = BTreeMap::new();
    for entry in sorted {
        let bucket = bucket_fn(&entry.created_at);
        seen_buckets
            .entry(bucket)
            .or_insert_with(|| entry.id.clone());
    }
    seen_buckets
        .into_values()
        .rev()
        .take(count as usize)
        .collect()
}

fn retain_by_weekly_bucket(
    sorted: &[&BackupEntry],
    count: u32,
    _now: &NaiveDateTime,
) -> Vec<String> {
    let mut seen_buckets: BTreeMap<(i32, u32), String> = BTreeMap::new();
    for entry in sorted {
        let date = entry.created_at.date();
        let year = date.iso_week().year();
        let week = date.iso_week().week();
        seen_buckets
            .entry((year, week))
            .or_insert_with(|| entry.id.clone());
    }
    seen_buckets
        .into_values()
        .rev()
        .take(count as usize)
        .collect()
}

fn retain_by_monthly_bucket(
    sorted: &[&BackupEntry],
    count: u32,
    _now: &NaiveDateTime,
) -> Vec<String> {
    let mut seen_buckets: BTreeMap<(i32, u32), String> = BTreeMap::new();
    for entry in sorted {
        let date = entry.created_at.date();
        seen_buckets
            .entry((date.year(), date.month()))
            .or_insert_with(|| entry.id.clone());
    }
    seen_buckets
        .into_values()
        .rev()
        .take(count as usize)
        .collect()
}

pub fn parse_timestamp(ts: &str) -> Option<NaiveDateTime> {
    ts.parse::<NaiveDateTime>()
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(ts).map(|dt| dt.naive_utc()))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(id: &str, days_ago: i64) -> BackupEntry {
        let now = Utc::now().naive_utc();
        let created = now - chrono::Duration::days(days_ago);
        BackupEntry {
            id: id.to_string(),
            created_at: created,
            consistency: "kanidm-offline".to_string(),
            reason: "scheduled".to_string(),
            referenced_by_active_restore: false,
            safety_backup_min_retention_hours: None,
        }
    }

    fn entry_with_reason(id: &str, days_ago: i64, reason: &str) -> BackupEntry {
        let mut e = entry(id, days_ago);
        e.reason = reason.to_string();
        e
    }

    fn safety_entry(id: &str, days_ago: i64, retention_hours: u32) -> BackupEntry {
        let mut e = entry_with_reason(id, days_ago, "restore-safety");
        e.safety_backup_min_retention_hours = Some(retention_hours);
        e
    }

    #[test]
    fn keep_last_retains_newest() {
        let entries = vec![entry("old", 30), entry("mid", 15), entry("new", 1)];
        let policy = RetentionPolicy {
            keep_last: 2,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"new".to_string()));
        assert!(result.retain.contains(&"mid".to_string()));
        assert!(result.delete.contains(&"old".to_string()));
    }

    #[test]
    fn min_age_protects_young_backups() {
        let entries = vec![entry("young", 0), entry("old", 30)];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 24,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"young".to_string()));
        assert!(result.delete.contains(&"old".to_string()));
    }

    #[test]
    fn active_restore_reference_protects_backup() {
        let mut e = entry("protected", 60);
        e.referenced_by_active_restore = true;
        let entries = vec![e, entry("other", 1)];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"protected".to_string()));
    }

    #[test]
    fn safety_backup_retention_protects_safety_backups() {
        let entries = vec![safety_entry("safety-recent", 1, 720), entry("normal", 1)];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"safety-recent".to_string()));
    }

    #[test]
    fn daily_retention_keeps_one_per_day() {
        let now = chrono::NaiveDateTime::parse_from_str("2026-08-20T12:00:00", "%Y-%m-%dT%H:%M:%S")
            .unwrap();
        let entries = vec![
            BackupEntry {
                id: "day1-a".to_string(),
                created_at: now - chrono::Duration::days(1),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
            BackupEntry {
                id: "day1-b".to_string(),
                created_at: now - chrono::Duration::days(1) - chrono::Duration::hours(2),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
            BackupEntry {
                id: "day2".to_string(),
                created_at: now - chrono::Duration::days(2),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
        ];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 7,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"day1-a".to_string()));
        assert!(result.delete.contains(&"day1-b".to_string()));
        assert!(result.retain.contains(&"day2".to_string()));
    }

    #[test]
    fn union_of_retention_sets() {
        let entries = vec![
            entry("a", 1),
            entry("b", 2),
            entry("c", 3),
            entry("d", 10),
            entry("e", 60),
        ];
        let policy = RetentionPolicy {
            keep_last: 2,
            daily: 3,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"a".to_string()));
        assert!(result.retain.contains(&"b".to_string()));
        assert!(result.retain.contains(&"c".to_string()));
        assert_eq!(result.delete.len(), 2);
    }

    #[test]
    fn protected_backups_never_selected_for_deletion() {
        let mut protected = entry("protected", 60);
        protected.referenced_by_active_restore = true;
        let entries = vec![protected, entry("deletable", 60)];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(!result.delete.contains(&"protected".to_string()));
        assert!(result.delete.contains(&"deletable".to_string()));
    }

    #[test]
    fn parse_rfc3339_timestamp() {
        let dt = parse_timestamp("2026-08-18T02:03:41Z");
        assert!(dt.is_some());
        let dt = dt.unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 8);
    }

    #[test]
    fn empty_entries_returns_empty_result() {
        let policy = RetentionPolicy::default();
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&[], &policy, &now);
        assert!(result.retain.is_empty());
        assert!(result.delete.is_empty());
    }

    #[test]
    fn weekly_retention_keeps_one_per_week() {
        let now = Utc::now().naive_utc();
        let entries = vec![
            BackupEntry {
                id: "week1-a".to_string(),
                created_at: now - chrono::Duration::days(7),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
            BackupEntry {
                id: "week1-b".to_string(),
                created_at: now - chrono::Duration::days(7) - chrono::Duration::hours(12),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
            BackupEntry {
                id: "week3".to_string(),
                created_at: now - chrono::Duration::days(21),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
        ];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 4,
            monthly: 0,
            min_age_hours: 0,
        };
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"week1-a".to_string()));
        assert!(result.delete.contains(&"week1-b".to_string()));
        assert!(result.retain.contains(&"week3".to_string()));
    }

    #[test]
    fn monthly_retention_keeps_one_per_month() {
        let now = Utc::now().naive_utc();
        let entries = vec![
            BackupEntry {
                id: "month1-a".to_string(),
                created_at: now - chrono::Duration::days(30),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
            BackupEntry {
                id: "month1-b".to_string(),
                created_at: now - chrono::Duration::days(30) - chrono::Duration::days(5),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
            BackupEntry {
                id: "month6".to_string(),
                created_at: now - chrono::Duration::days(180),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
                referenced_by_active_restore: false,
                safety_backup_min_retention_hours: None,
            },
        ];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 12,
            min_age_hours: 0,
        };
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"month1-a".to_string()));
        assert!(result.delete.contains(&"month1-b".to_string()));
        assert!(result.retain.contains(&"month6".to_string()));
    }

    #[test]
    fn expired_safety_backup_can_be_deleted() {
        let entries = vec![safety_entry("safety-expired", 40, 720)];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.delete.contains(&"safety-expired".to_string()));
    }

    #[test]
    fn active_restore_overrides_all_other_rules() {
        let mut e = entry("protected", 365);
        e.referenced_by_active_restore = true;
        let entries = vec![e];
        let policy = RetentionPolicy {
            keep_last: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            min_age_hours: 0,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        assert!(result.retain.contains(&"protected".to_string()));
        assert!(result.delete.is_empty());
    }

    #[test]
    fn retain_and_delete_are_disjoint() {
        let entries = vec![entry("a", 1), entry("b", 2), entry("c", 3), entry("d", 60)];
        let policy = RetentionPolicy {
            keep_last: 2,
            daily: 3,
            weekly: 2,
            monthly: 1,
            min_age_hours: 12,
        };
        let now = Utc::now().naive_utc();
        let result = select_deletion_candidates(&entries, &policy, &now);
        for id in &result.retain {
            assert!(!result.delete.contains(id));
        }
        assert_eq!(result.retain.len() + result.delete.len(), entries.len());
    }

    #[test]
    fn parse_naive_datetime_format() {
        let dt = parse_timestamp("2026-08-18T02:03:41");
        assert!(dt.is_some());
    }

    #[test]
    fn parse_invalid_timestamp_returns_none() {
        assert!(parse_timestamp("not-a-timestamp").is_none());
    }

    #[test]
    fn default_retention_policy_values() {
        let policy = RetentionPolicy::default();
        assert_eq!(policy.keep_last, 8);
        assert_eq!(policy.daily, 7);
        assert_eq!(policy.weekly, 4);
        assert_eq!(policy.monthly, 12);
        assert_eq!(policy.min_age_hours, 24);
    }
}
