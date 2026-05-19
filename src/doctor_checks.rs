// Pure helpers for `clawket doctor` data-loss-risk diagnostics (LM-9).
//
// Kept separate from doctor.rs so the decision logic (mode/permission,
// thresholds, snapshot comparison) can be unit-tested without spawning an
// async runtime, the daemon, or touching the real filesystem layout. The
// orchestration (printing, accumulating, exiting) stays in doctor.rs.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Severity {
    Ok,
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn tag(self) -> &'static str {
        match self {
            Severity::Ok => "[OK]",
            Severity::Info => "[INFO]",
            Severity::Warn => "[WARN]",
            Severity::Error => "[ERROR]",
        }
    }
}

/// (LM-9 #2) Treat a directory as world-writable if "others" have write
/// permission AND the sticky bit is not set. Sticky-bit dirs (`/tmp`) are
/// world-writable in the POSIX sense but safe in practice; XDG-style user
/// data dirs that are world-writable without a sticky bit indicate either a
/// bad umask or a directory that escaped from /tmp into the user's home —
/// both worth surfacing.
///
/// `mode` is the unix mode bitmask (e.g. `metadata.mode()` & 0o7777).
pub fn is_world_writable(mode: u32) -> bool {
    let world_write = mode & 0o002 != 0;
    let sticky = mode & 0o1000 != 0;
    world_write && !sticky
}

/// (LM-9 #3) Inspect a list of candidate legacy locations and return the
/// subset that actually exists. Pure over filesystem queries (caller passes
/// the candidates), so tests can drive both branches.
pub fn legacy_remnants_present<F: Fn(&Path) -> bool>(
    candidates: &[PathBuf],
    exists_fn: F,
) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter(|p| exists_fn(p))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorSnapshot {
    pub task_count: u64,
    pub db_mtime_secs: Option<u64>,
}

/// (LM-9 #4) Compare current task count against a previously persisted
/// snapshot. We treat anything below 50% of last seen as "suspected loss"
/// and emit WARN. First run (snapshot=None) or growth → OK.
///
/// The 50% cliff is intentional: dropping a few tasks via `cancelled` is
/// normal noise; halving the catalog is not. The post-incident review
/// showed the actual loss was ~100% (db.sqlite gone), so even a much
/// stricter threshold would catch the canonical failure — picking 50%
/// just keeps false-positives quiet during normal cleanup sweeps.
pub fn classify_task_count_change(prev: Option<u64>, current: u64) -> Severity {
    match prev {
        None => Severity::Info,
        Some(0) => Severity::Ok,
        Some(p) if current * 2 < p => Severity::Warn,
        Some(_) => Severity::Ok,
    }
}

/// (LM-69 / ADR-0010) Classify activity_log size against the configured cap.
///
/// - `< 80%`         → Ok (steady state)
/// - `80% .. < 95%`  → Warn (consider lowering CLAWKET_ACTIVITY_LOG_TOTAL_DAYS
///   or raising CLAWKET_ACTIVITY_LOG_MAX_MB)
/// - `>= 95%`        → Error (rollup is about to start shrinking the cold
///   cutoff aggressively, history will narrow)
///
/// `max_bytes == 0` is treated as "no budget configured" → Info; the daemon
/// itself will still enforce its policy floor (MIN_TOTAL_DAYS_UNDER_PRESSURE)
/// but the doctor has nothing meaningful to compare against.
pub fn classify_activity_log_budget(used_bytes: i64, max_bytes: i64) -> Severity {
    if max_bytes <= 0 {
        return Severity::Info;
    }
    let pct = (used_bytes as f64 / max_bytes as f64) * 100.0;
    if pct >= 95.0 {
        Severity::Error
    } else if pct >= 80.0 {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

/// (LM-9 #5) DB mtime classification:
/// - older than 24h → Ok (well-established install)
/// - within 24h AND no backup nearby → Info (fresh install or recent recreate)
/// - within 24h AND backup exists → Ok
///
/// "Backup" here is any sibling file matching `*.bak` / `*.backup` next to
/// the SQLite file. The check is INFO not WARN: a fresh install is normal
/// after first plugin install; we only want to surface the state, not
/// alarm the user.
pub fn classify_db_freshness(
    db_mtime: Option<SystemTime>,
    now: SystemTime,
    has_backup: bool,
) -> Severity {
    let Some(m) = db_mtime else {
        return Severity::Ok;
    };
    let Ok(age) = now.duration_since(m) else {
        return Severity::Ok;
    };
    if age.as_secs() > 24 * 60 * 60 {
        return Severity::Ok;
    }
    if has_backup {
        Severity::Ok
    } else {
        Severity::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn world_writable_excludes_sticky_dirs() {
        // 0o1777 — /tmp style (world-writable + sticky) → safe
        assert!(!is_world_writable(0o1777));
        // 0o0777 — world-writable without sticky → flagged
        assert!(is_world_writable(0o0777));
        // 0o0755 — typical user dir → safe
        assert!(!is_world_writable(0o0755));
        // 0o0700 — private → safe
        assert!(!is_world_writable(0o0700));
        // 0o0775 — group-writable but not world → safe
        assert!(!is_world_writable(0o0775));
    }

    #[test]
    fn legacy_remnants_filters_by_existence() {
        let cands = vec![
            PathBuf::from("/legacy/a"),
            PathBuf::from("/legacy/b"),
            PathBuf::from("/legacy/c"),
        ];
        let only_b = |p: &Path| p == Path::new("/legacy/b");
        let found = legacy_remnants_present(&cands, only_b);
        assert_eq!(found, vec![PathBuf::from("/legacy/b")]);

        let none = legacy_remnants_present(&cands, |_| false);
        assert!(none.is_empty());

        let all = legacy_remnants_present(&cands, |_| true);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn task_count_change_classifies_correctly() {
        // First run — no prior snapshot → Info
        assert_eq!(classify_task_count_change(None, 100), Severity::Info);
        // Growth or stable → Ok
        assert_eq!(classify_task_count_change(Some(50), 60), Severity::Ok);
        assert_eq!(classify_task_count_change(Some(50), 50), Severity::Ok);
        // Mild drop within 50% → Ok (normal cleanup)
        assert_eq!(classify_task_count_change(Some(100), 60), Severity::Ok);
        assert_eq!(classify_task_count_change(Some(100), 50), Severity::Ok);
        // Sharp drop below 50% → Warn (suspected loss)
        assert_eq!(classify_task_count_change(Some(100), 49), Severity::Warn);
        assert_eq!(classify_task_count_change(Some(100), 0), Severity::Warn);
        // Prior was 0 → can't divide by zero; treat as fresh install
        assert_eq!(classify_task_count_change(Some(0), 5), Severity::Ok);
        assert_eq!(classify_task_count_change(Some(0), 0), Severity::Ok);
    }

    #[test]
    fn db_freshness_well_aged_is_ok() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 3600);
        let two_days_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(8 * 24 * 3600);
        assert_eq!(
            classify_db_freshness(Some(two_days_ago), now, false),
            Severity::Ok
        );
    }

    #[test]
    fn db_freshness_recent_no_backup_is_info() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 3600);
        let one_hour_ago = now - Duration::from_secs(3600);
        assert_eq!(
            classify_db_freshness(Some(one_hour_ago), now, false),
            Severity::Info
        );
    }

    #[test]
    fn db_freshness_recent_with_backup_is_ok() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 3600);
        let one_hour_ago = now - Duration::from_secs(3600);
        assert_eq!(
            classify_db_freshness(Some(one_hour_ago), now, true),
            Severity::Ok
        );
    }

    #[test]
    fn db_freshness_missing_is_ok() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 3600);
        assert_eq!(classify_db_freshness(None, now, false), Severity::Ok);
    }

    #[test]
    fn db_freshness_future_mtime_is_ok() {
        // Clock skew (mtime > now) shouldn't crash or false-alarm.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 3600);
        let future = now + Duration::from_secs(3600);
        assert_eq!(
            classify_db_freshness(Some(future), now, false),
            Severity::Ok
        );
    }

    #[test]
    fn activity_log_budget_classifies_thresholds() {
        let mb = 1024 * 1024;
        let cap = 500 * mb;
        // Under 80%
        assert_eq!(classify_activity_log_budget(100 * mb, cap), Severity::Ok);
        assert_eq!(classify_activity_log_budget(0, cap), Severity::Ok);
        assert_eq!(
            classify_activity_log_budget(((cap as f64) * 0.799) as i64, cap),
            Severity::Ok
        );
        // 80% .. 95%
        assert_eq!(
            classify_activity_log_budget(((cap as f64) * 0.80) as i64, cap),
            Severity::Warn
        );
        assert_eq!(
            classify_activity_log_budget(((cap as f64) * 0.94) as i64, cap),
            Severity::Warn
        );
        // 95%+
        assert_eq!(
            classify_activity_log_budget(((cap as f64) * 0.95) as i64, cap),
            Severity::Error
        );
        assert_eq!(classify_activity_log_budget(cap * 2, cap), Severity::Error);
    }

    #[test]
    fn activity_log_budget_zero_cap_is_info() {
        assert_eq!(classify_activity_log_budget(1_000_000, 0), Severity::Info);
        assert_eq!(classify_activity_log_budget(0, 0), Severity::Info);
        assert_eq!(classify_activity_log_budget(1, -1), Severity::Info);
    }

    #[test]
    fn severity_tags_are_stable() {
        assert_eq!(Severity::Ok.tag(), "[OK]");
        assert_eq!(Severity::Info.tag(), "[INFO]");
        assert_eq!(Severity::Warn.tag(), "[WARN]");
        assert_eq!(Severity::Error.tag(), "[ERROR]");
    }
}
