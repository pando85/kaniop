//! Validation of the `KanidmBackupSchedule` values that Kaniop renders into Kanidm's
//! `[online_backup]` configuration.
//!
//! Kanidm refuses to start when it cannot use these values, so every component that admits,
//! reports on or renders a Schedule must apply the same checks: the webhook, the schedule
//! controller and the Kanidm StatefulSet renderer.

use std::str::FromStr;

use chrono::Utc;
use cron::Schedule;

/// Minimum `localVersions` that still lets the transport sidecar upload backups.
///
/// With a single local version Kanidm deletes each backup file when it writes the next one,
/// before the sidecar's minimum file age is reached, so nothing is ever uploaded.
pub const MIN_LOCAL_VERSIONS: u32 = 2;

/// Rewrites a schedule exactly as Kanidm does before parsing it.
///
/// Kanidm (`server/core/src/interval.rs`, 1.11.x) splits the expression on ASCII whitespace
/// and, only when there are exactly five fields, prepends a `0` seconds field and appends a `*`
/// year field. Every other field count is passed through unchanged.
pub fn kanidm_cron_expression(schedule: &str) -> String {
    let mut fields = schedule.split_ascii_whitespace().collect::<Vec<&str>>();
    if fields.len() == 5 {
        fields.insert(0, "0");
        fields.push("*");
    }
    fields.join(" ")
}

/// Validates a backup schedule the way Kanidm will evaluate it at startup.
///
/// The expression must parse after Kanidm's rewrite and must match at least one future date;
/// otherwise Kanidm exits with "will not match any date" and the primary pod crashloops.
pub fn validate_backup_cron_schedule(schedule: &str) -> Result<(), String> {
    let expression = kanidm_cron_expression(schedule);
    let parsed = Schedule::from_str(&expression).map_err(|e| {
        format!(
            "invalid cron schedule '{schedule}': Kanidm evaluates it as '{expression}', which \
             does not parse ({e}); use a 5-field (min hour dom month dow), 6-field (sec min \
             hour dom month dow) or 7-field (with year) expression"
        )
    })?;
    if parsed.upcoming(Utc).next().is_none() {
        return Err(format!(
            "invalid cron schedule '{schedule}': Kanidm evaluates it as '{expression}', which \
             never fires; Kanidm would refuse to start"
        ));
    }
    Ok(())
}

/// Validates `localVersions` against [`MIN_LOCAL_VERSIONS`].
pub fn validate_local_versions(local_versions: u32) -> Result<(), String> {
    if local_versions < MIN_LOCAL_VERSIONS {
        return Err(format!(
            "localVersions must be at least {MIN_LOCAL_VERSIONS}: with {local_versions} Kanidm \
             deletes each backup file before the transport sidecar can upload it"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kanidm_rewrite_only_applies_to_five_fields() {
        let cases = [
            ("0 2 * * *", "0 0 2 * * * *"),
            ("0  2 *\t* *", "0 0 2 * * * *"),
            ("0 0 2 * * *", "0 0 2 * * *"),
            ("0 0 2 * * * 2030", "0 0 2 * * * 2030"),
            ("0 2 * * * 2030", "0 2 * * * 2030"),
            ("@daily", "@daily"),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(kanidm_cron_expression(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn backup_cron_schedule_table() {
        let cases = [
            // Common 5-field crons.
            ("0 2 * * *", true),
            ("*/15 * * * *", true),
            ("*/2 * * * *", true),
            ("0 0 1 JAN *", true),
            ("0 0 * * MON-FRI", true),
            ("0 0 29 2 *", true),
            // 6 fields: seconds first, no year.
            ("0 0 2 * * *", true),
            ("0 */15 * * * *", true),
            // 7 fields: with year.
            ("0 0 2 * * * *", true),
            ("0 0 2 * * * 2099", true),
            // Shorthands are passed through unchanged.
            ("@daily", true),
            ("@hourly", true),
            ("@weekly", true),
            // Parses but never fires (F4A.3).
            ("0 0 31 2 *", false),
            ("0 0 0 30 2 * *", false),
            ("0 0 0 1 1 * 2020", false),
            // 5 fields plus a year: Kanidm sees 6 fields and reads the year as day of week.
            ("0 2 * * * 2030", false),
            // Syntax errors.
            ("60 * * * *", false),
            ("not-a-cron", false),
            ("@@@@@", false),
            ("* * * *", false),
            ("", false),
        ];
        for (input, valid) in cases {
            let result = validate_backup_cron_schedule(input);
            assert_eq!(
                result.is_ok(),
                valid,
                "input: {input:?}, result: {result:?}"
            );
        }
    }

    #[test]
    fn never_firing_schedule_error_names_the_kanidm_expression() {
        let err = validate_backup_cron_schedule("0 0 31 2 *").unwrap_err();
        assert!(err.contains("'0 0 0 31 2 * *'"), "{err}");
        assert!(err.contains("never fires"), "{err}");
    }

    #[test]
    fn local_versions_minimum() {
        assert!(validate_local_versions(0).is_err());
        assert!(validate_local_versions(1).is_err());
        assert!(validate_local_versions(2).is_ok());
        assert!(validate_local_versions(7).is_ok());
    }
}
