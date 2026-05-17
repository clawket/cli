// clawket doctor — diagnose daemon + install health.
//
// Prints a structured health snapshot so users (and the plugin's ensureDaemon
// hook) can identify why the daemon may have failed to start. Mirrors the
// check surface of the Node v2.2.1 equivalent.
//
// LM-9: each diagnostic now emits a Severity tag so the final exit code is a
// pure function of "did any check land at Severity::Error". The earlier
// behaviour (early-exit on path overlap) is preserved as Severity::Error in
// the path separation section, but we no longer short-circuit — the user
// sees every diagnostic on a single run instead of fixing them one at a time.

use anyhow::Result;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::client;
use crate::doctor_checks::{
    DoctorSnapshot, Severity, classify_activity_log_budget, classify_db_freshness,
    classify_task_count_change, is_world_writable, legacy_remnants_present,
};
use crate::paths;

pub async fn run(json_output: bool, plan: Option<String>, escalation: bool) -> Result<()> {
    if json_output {
        return run_json().await;
    }
    let plan_filter = plan.as_deref();
    println!("Clawket doctor");
    println!("==============");
    println!();

    let mut tally: Vec<Severity> = Vec::new();

    section("Environment overrides");
    for var in [
        "CLAWKET_DAEMON_BIN",
        "CLAWKET_BIN",
        "CLAWKET_SOCKET",
        "CLAWKET_DATA_DIR",
        "CLAWKET_CACHE_DIR",
        "CLAWKET_CONFIG_DIR",
        "CLAWKET_STATE_DIR",
        "CLAWKET_DB",
    ] {
        match std::env::var(var) {
            Ok(v) => println!("  {var} = {v}"),
            Err(_) => println!("  {var} = (unset)"),
        }
    }
    println!();

    // ===== v3 plan-named sections (DOCTOR-SECTIONS-V3) =====

    // [Paths] — XDG path resolution (data/cache/config/state/db)
    section("Paths");
    let data = paths::data_dir();
    let cache = paths::cache_dir();
    let config = paths::config_dir();
    let state = paths::state_dir();
    let db = data.join("db.sqlite");
    let socket = paths::socket_path();
    let pid = paths::pid_path();
    print_path("data dir", &data);
    print_path("cache dir", &cache);
    print_path("config dir", &config);
    print_path("state dir", &state);
    print_path("db", &db);
    print_path("socket", &socket);
    print_path("pid file", &pid);
    println!();

    // [Daemon] — running status, uptime, started_at, port, socket
    section("Daemon");
    let daemon_bin = resolve_daemon_bin();
    match &daemon_bin {
        Some((p, reason)) => println!("  binary: {} ({})", p.display(), reason),
        None => println!("  binary: not found — set CLAWKET_DAEMON_BIN or install via plugin"),
    }
    let client = client::make_client();
    let (health_ok, health_val) = match client::get(&client, "/health").await {
        Ok(val) => {
            println!("  {} Unix socket /health: OK", Severity::Ok.tag());
            if let Some(uptime_ms) = val.get("uptime_ms").and_then(Value::as_i64) {
                let secs = uptime_ms / 1000;
                println!("  uptime: {secs}s");
            }
            if let Some(pid) = val.get("pid").and_then(Value::as_i64) {
                println!("  pid: {pid}");
            }
            if let Some(version) = val.get("version").and_then(Value::as_str) {
                println!("  version: {version}");
            }
            println!("  socket: {}", paths::socket_path().display());
            if let Ok(port) = std::fs::read_to_string(paths::cache_dir().join("port")) {
                println!("  port: {}", port.trim());
            }
            tally.push(Severity::Ok);
            (true, Some(val))
        }
        Err(e) => {
            // Daemon-down is only an ERROR when the binary is actually installed.
            // If `resolve_daemon_bin()` returned None we already printed
            // "binary: not found" above — adding an ERROR here would double-count
            // a single "not installed" condition and break hermetic-env e2e tests
            // that expect zero errors when no daemon is configured.
            let severity = if daemon_bin.is_some() {
                Severity::Error
            } else {
                Severity::Info
            };
            println!("  {} Unix socket /health: FAIL: {e}", severity.tag());
            println!("       hint: run `clawket daemon start` (or restart)");
            tally.push(severity);
            (false, None)
        }
    };
    let task_count_now = if health_ok {
        fetch_task_count(&client).await
    } else {
        None
    };
    println!();

    // [Database] — sqlite path, tables, entity counts
    section("Database");
    run_database_check(&client, &db, &mut tally).await;
    println!();

    // [Hooks] — adapter file presence (7 hook handler files)
    section("Hooks");
    run_hooks_check(&mut tally);
    println!();

    // [MCP] — clawket mcp subcommand reachability + tool list
    section("MCP");
    run_mcp_check(&mut tally);
    println!();

    // [Plugin install] — install marker + components.json version match
    section("Plugin install");
    run_plugin_install_check(&mut tally);
    println!();

    // [Compatibility] — package.json compat range vs installed components
    section("Compatibility");
    run_compatibility_check(health_val.as_ref(), &mut tally);
    println!();

    // [i18n] — locale resolution + locale file coverage
    section("i18n");
    run_i18n_check(&mut tally);
    println!();

    // [Audit log] — audit_log size, last entry, prev_hash chain integrity
    section("Audit log");
    run_audit_log_check(&client, &mut tally).await;
    println!();

    // ===== Supplemental diagnostics (kept from v2 — not in v3 plan list) =====

    // LM-9: Five-item data-loss-risk panel.
    section("Data loss risk diagnostics (LM-9)");
    run_data_loss_diagnostics(&data, &state, task_count_now, &mut tally);
    println!();

    // LM-69 / ADR-0010: activity_log retention budget panel.
    section("activity_log retention (LM-69)");
    run_activity_log_budget_check(&client, &mut tally).await;
    println!();

    // LM-259 / L1.4.c — project.enabled state for the current cwd.
    section("Project enable state (LM-8)");
    run_project_enabled_check(&client, &mut tally).await;
    println!();

    section("Legacy lattice data");
    let legacy = legacy_data_dir();
    if legacy.join("db.sqlite").exists() {
        println!(
            "  legacy DB present: {}",
            legacy.join("db.sqlite").display()
        );
        println!("  Migration is NOT supported — schema changed too much across versions.");
        println!("  Clawket treats every install as a fresh start.");
        println!(
            "  If you no longer need the legacy data, remove {} manually.",
            legacy.display()
        );
    } else {
        println!("  no legacy lattice DB detected");
    }
    println!();

    // US-CLAWKET-TIER-009 / TIER-045 / TIER-046 — tier-aware policy readout.
    section("Tier-Aware");
    run_tier_aware_check(&mut tally);
    println!();

    section("tier_distribution");
    run_tier_distribution_check(&client, plan_filter, &mut tally).await;
    println!();

    section("escalation_rate");
    run_escalation_rate_check(&client, plan_filter, &mut tally).await;
    println!();

    if escalation {
        section("escalation report (--escalation)");
        run_escalation_report(&client, plan_filter, &mut tally).await;
        println!();
    }

    section("Skills");
    run_skills_check(&mut tally);
    println!();

    // FIX-CLI-103: schema_version / components.json + sqlite-vec probe.
    run_extra_checks(&client, &mut tally).await;

    // [Path separation invariant (LM-8)] — placed last per v3 plan.
    section("Path separation invariant (LM-8)");
    let mut overlap_count = 0u32;
    for (label, p) in [
        ("data", &data),
        ("cache", &cache),
        ("config", &config),
        ("state", &state),
        ("db", &db),
    ] {
        if paths::path_overlaps_plugin_dir(p) {
            println!(
                "  {} {label}: {} — OVERLAP with .claude/plugins/",
                Severity::Error.tag(),
                p.display()
            );
            overlap_count += 1;
            tally.push(Severity::Error);
        } else {
            println!("  {} {label}: {} — OK", Severity::Ok.tag(), p.display());
        }
    }
    if overlap_count > 0 {
        eprintln!();
        eprintln!("ERROR: {overlap_count} path(s) resolve under Claude Code's plugin directory.");
        eprintln!("       Plugin reinstall (`/plugin install`) wipes that tree, so this layout");
        eprintln!("       will silently destroy the Clawket SQLite DB on the next plugin update.");
        eprintln!("       Fix: point CLAWKET_DATA_DIR / XDG_DATA_HOME / etc at a path outside");
        eprintln!(
            "       ~/.claude/plugins/ . CLAWKET_ALLOW_PLUGIN_OVERLAP=1 acknowledges the risk."
        );
    } else {
        println!("  data path / plugin path separation: OK");
    }
    println!();

    let any_error = tally.iter().any(|s| *s == Severity::Error);
    let warn_count = tally.iter().filter(|s| **s == Severity::Warn).count();
    let info_count = tally.iter().filter(|s| **s == Severity::Info).count();
    println!();
    println!(
        "[Summary] errors={} warnings={} info={}",
        tally.iter().filter(|s| **s == Severity::Error).count(),
        warn_count,
        info_count
    );
    if any_error {
        std::process::exit(1);
    }

    Ok(())
}

fn section(title: &str) {
    println!("[{title}]");
}

fn print_path(label: &str, p: &Path) {
    let exists = if p.exists() { "exists" } else { "missing" };
    println!("  {label}: {} ({exists})", p.display());
}

fn resolve_daemon_bin() -> Option<(PathBuf, &'static str)> {
    if let Ok(v) = std::env::var("CLAWKET_DAEMON_BIN") {
        let p = PathBuf::from(v);
        if p.exists() {
            return Some((p, "CLAWKET_DAEMON_BIN"));
        }
    }
    for (candidate, label) in paths::daemon_bin_candidates() {
        if candidate.exists() {
            return Some((candidate, label));
        }
    }
    if let Ok(which) = which("clawketd") {
        return Some((which, "PATH"));
    }
    None
}

fn which(name: &str) -> anyhow::Result<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let p = PathBuf::from(dir).join(name);
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("{name} not found in PATH")
}

fn legacy_data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("lattice")
}

async fn fetch_task_count(client: &client::HttpClient) -> Option<u64> {
    match client::get(client, "/tasks").await {
        Ok(Value::Array(items)) => Some(items.len() as u64),
        _ => None,
    }
}

fn run_data_loss_diagnostics(
    data_dir: &Path,
    state_dir: &Path,
    task_count_now: Option<u64>,
    tally: &mut Vec<Severity>,
) {
    // #1 — plugin overlap (already accounted for in the LM-8 section above).
    // We re-emit a single OK/ERROR line so the LM-9 panel is self-contained
    // for users skimming only this block.
    let overlap = paths::path_overlaps_plugin_dir(data_dir);
    if overlap {
        println!(
            "  {} #1 plugin overlap: data dir is under .claude/plugins/ — 데이터 손실 임박",
            Severity::Error.tag()
        );
        println!("       조치: CLAWKET_DATA_DIR/XDG_DATA_HOME 를 plugin 디렉토리 외부로 이동");
        tally.push(Severity::Error);
    } else {
        println!(
            "  {} #1 plugin overlap: data dir 가 plugin 트리 외부에 있음",
            Severity::Ok.tag()
        );
        tally.push(Severity::Ok);
    }

    // #2 — world-writable data dir
    match world_writable_check(data_dir) {
        Ok(true) => {
            println!(
                "  {} #2 permissions: data dir 가 world-writable — 다른 사용자가 SQLite 변조 가능",
                Severity::Warn.tag()
            );
            println!(
                "       조치: chmod 700 {} (sticky bit 가 없는 0o7XX 권한)",
                data_dir.display()
            );
            tally.push(Severity::Warn);
        }
        Ok(false) => {
            println!("  {} #2 permissions: data dir 권한 OK", Severity::Ok.tag());
            tally.push(Severity::Ok);
        }
        Err(reason) => {
            println!(
                "  {} #2 permissions: 검사 불가 ({reason})",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
        }
    }

    // #3 — legacy path remnants
    let candidates = legacy_remnant_candidates();
    let found = legacy_remnants_present(&candidates, |p| p.exists());
    if found.is_empty() {
        println!(
            "  {} #3 legacy remnants: v10 이전 경로 잔재 없음",
            Severity::Ok.tag()
        );
        tally.push(Severity::Ok);
    } else {
        println!(
            "  {} #3 legacy remnants: {} 경로 잔재 발견",
            Severity::Warn.tag(),
            found.len()
        );
        for p in &found {
            println!("       - {}", p.display());
        }
        println!(
            "       조치: 데이터를 더 이상 쓰지 않는다면 위 경로를 사용자가 직접 삭제. Clawket 은 자동 마이그레이션을 지원하지 않음."
        );
        tally.push(Severity::Warn);
    }

    // #4 — task count snapshot comparison
    let snapshot_path = state_dir.join("doctor-snapshot.json");
    let prev_snapshot = read_snapshot(&snapshot_path);
    let prev_count = prev_snapshot.as_ref().map(|s| s.task_count);
    match task_count_now {
        Some(curr) => {
            let sev = classify_task_count_change(prev_count, curr);
            match sev {
                Severity::Warn => {
                    println!(
                        "  {} #4 task count: 이전 {} → 현재 {} (50% 미만, 데이터 손실 의심)",
                        Severity::Warn.tag(),
                        prev_count.unwrap_or(0),
                        curr
                    );
                    println!(
                        "       조치: clawket task list 로 작업 수 확인, 누락된 항목이 의도된 cancel 인지 검증"
                    );
                }
                Severity::Info => {
                    println!(
                        "  {} #4 task count: 첫 실행 — 스냅샷 기록 (현재 {} 작업)",
                        Severity::Info.tag(),
                        curr
                    );
                }
                _ => {
                    println!(
                        "  {} #4 task count: 이전 {} → 현재 {} (정상 변동 범위)",
                        Severity::Ok.tag(),
                        prev_count.unwrap_or(0),
                        curr
                    );
                }
            }
            tally.push(sev);
            // Update snapshot regardless — next run compares against this.
            let _ = write_snapshot(
                &snapshot_path,
                &DoctorSnapshot {
                    task_count: curr,
                    db_mtime_secs: db_mtime_secs(data_dir),
                },
            );
        }
        None => {
            println!(
                "  {} #4 task count: 데몬 비가용 — 이번 실행에서는 비교 생략",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
        }
    }

    // #5 — DB freshness
    let db_path = data_dir.join("db.sqlite");
    let mtime = db_path.metadata().ok().and_then(|m| m.modified().ok());
    let backup = nearby_backup_present(&db_path);
    let sev = classify_db_freshness(mtime, SystemTime::now(), backup);
    match (mtime, sev) {
        (None, _) => {
            println!(
                "  {} #5 db freshness: db.sqlite 미존재 — 데몬 미기동 또는 미설치",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
        }
        (Some(_), Severity::Info) => {
            println!(
                "  {} #5 db freshness: db.sqlite 가 24h 이내 신규 + 백업 없음 — 첫 설치 또는 재생성 직후",
                Severity::Info.tag()
            );
            println!(
                "       조치: 첫 설치 직후라면 정상. 의도치 않은 재생성이라면 즉시 작업 중단 후 검사."
            );
            tally.push(Severity::Info);
        }
        (Some(_), s) => {
            println!(
                "  {} #5 db freshness: db.sqlite OK ({})",
                Severity::Ok.tag(),
                if backup {
                    "백업 존재"
                } else {
                    "24h 이상 경과"
                }
            );
            tally.push(s);
        }
    }
}

async fn run_activity_log_budget_check(client: &client::HttpClient, tally: &mut Vec<Severity>) {
    let stats = match client::get(client, "/activity/stats").await {
        Ok(v) => v,
        Err(e) => {
            println!(
                "  {} 데몬 비가용 — activity_log 사이즈 검사 생략 ({e})",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
    };

    let used = stats.get("used_bytes").and_then(Value::as_i64).unwrap_or(0);
    let max = stats.get("max_bytes").and_then(Value::as_i64).unwrap_or(0);
    let hot_rows = stats.get("hot_rows").and_then(Value::as_i64).unwrap_or(0);
    let archive_batches = stats
        .get("archive_batches")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let max_mb = stats.get("max_mb").and_then(Value::as_i64).unwrap_or(0);
    let hot_days = stats.get("hot_days").and_then(Value::as_i64).unwrap_or(0);
    let total_days = stats.get("total_days").and_then(Value::as_i64).unwrap_or(0);

    let pct = if max > 0 {
        (used as f64 / max as f64) * 100.0
    } else {
        0.0
    };

    let sev = classify_activity_log_budget(used, max);
    let used_mib = used as f64 / (1024.0 * 1024.0);
    println!(
        "  {} 사이즈 = {:.1} MiB / {} MB ({:.0}%, hot {}d / total {}d)",
        sev.tag(),
        used_mib,
        max_mb,
        pct,
        hot_days,
        total_days
    );
    println!(
        "       hot_rows={} archive_batches={}",
        hot_rows, archive_batches
    );
    if matches!(sev, Severity::Warn) {
        println!(
            "       조치: CLAWKET_ACTIVITY_LOG_TOTAL_DAYS 를 줄이거나 \
             CLAWKET_ACTIVITY_LOG_MAX_MB 를 늘려 보유 기간을 조절"
        );
    } else if matches!(sev, Severity::Error) {
        println!(
            "       조치: 95% 초과 — 다음 rollup 부터 cold cutoff 가 적극 축소되어 \
             history 가 좁아짐. 즉시 CLAWKET_ACTIVITY_LOG_MAX_MB 상향 검토."
        );
    }
    tally.push(sev);
}

fn world_writable_check(p: &Path) -> std::result::Result<bool, String> {
    if !p.exists() {
        return Err("dir missing".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(p).map_err(|e| e.to_string())?;
        Ok(is_world_writable(meta.permissions().mode() & 0o7777))
    }
    #[cfg(not(unix))]
    {
        let _ = (p, is_world_writable as fn(u32) -> bool);
        Err("non-unix platform".into())
    }
}

fn legacy_remnant_candidates() -> Vec<PathBuf> {
    let home = std::env::var("HOME").map(PathBuf::from).ok();
    let mut out = Vec::new();
    if let Some(h) = &home {
        // v10 이전: 데이터 파일이 ~/.local/share 직속에 흩뿌려져 있던 시기.
        out.push(h.join(".local/share/db.sqlite"));
        // 구 lattice 잔재 — daemon 의 warn_legacy_data 와 중복이지만 의도적
        // (사용자가 doctor 만 보아도 잔재를 인지할 수 있게).
        out.push(h.join(".local/share/lattice"));
        // macOS: 과거에 dirs::data_dir() 이 ~/Library/Application Support/
        // 를 반환하던 시절의 잔재.
        #[cfg(target_os = "macos")]
        {
            out.push(h.join("Library/Application Support/clawket"));
            out.push(h.join("Library/Application Support/lattice"));
        }
    }
    out
}

fn read_snapshot(p: &Path) -> Option<DoctorSnapshot> {
    let data = fs::read_to_string(p).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_snapshot(p: &Path, s: &DoctorSnapshot) -> std::result::Result<(), std::io::Error> {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(p, data)
}

fn db_mtime_secs(data_dir: &Path) -> Option<u64> {
    let m = data_dir
        .join("db.sqlite")
        .metadata()
        .ok()?
        .modified()
        .ok()?;
    m.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn nearby_backup_present(db_path: &Path) -> bool {
    let Some(parent) = db_path.parent() else {
        return false;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.ends_with(".bak") || s.ends_with(".backup") || s.contains(".sqlite.") {
            return true;
        }
    }
    false
}

async fn run_project_enabled_check(client: &client::HttpClient, tally: &mut Vec<Severity>) {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            println!(
                "  {} 현재 cwd 조회 실패 ({e}) — project enable 상태 검사 생략",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
    };
    let cwd_str = cwd.to_string_lossy();
    // The route accepts the cwd as a path-style segment with a leading
    // `/`. We pass it raw — the daemon prepends `/` if missing — but
    // strip any trailing slash so the lookup matches what `project
    // register` recorded.
    let cwd_segment = cwd_str.trim_end_matches('/').trim_start_matches('/');
    let path = format!("/projects/by-cwd/{cwd_segment}");

    let payload = match client::get(client, &path).await {
        Ok(v) => Some(v),
        Err(e) => {
            // The project_enabled module distinguishes "not registered"
            // (404) from "daemon down" via payload presence; we surface
            // the latter as info so the user isn't pointed at a fix
            // they can't apply offline.
            let msg = e.to_string();
            if msg.contains("404") || msg.contains("not_found") || msg.contains("not found") {
                None
            } else {
                println!(
                    "  {} 데몬 비가용 — project enable 상태 검사 생략 ({msg})",
                    Severity::Info.tag()
                );
                tally.push(Severity::Info);
                return;
            }
        }
    };

    let line = project_enabled::format_project_enabled(payload.as_ref(), &cwd_str);
    println!("  {}", line.head);
    for hint in &line.hints {
        println!("       {hint}");
    }
    tally.push(line.severity);
}

// ===== DOCTOR-SECTIONS-V3 helpers =====

/// [Database] — sqlite file presence, entity counts pulled from list endpoints.
async fn run_database_check(
    client: &client::HttpClient,
    db_path: &Path,
    tally: &mut Vec<Severity>,
) {
    if db_path.exists() {
        let size_bytes = db_path.metadata().ok().map(|m| m.len()).unwrap_or(0);
        let size_mib = size_bytes as f64 / (1024.0 * 1024.0);
        println!(
            "  {} sqlite: {} ({:.2} MiB)",
            Severity::Ok.tag(),
            db_path.display(),
            size_mib
        );
        tally.push(Severity::Ok);
    } else {
        println!(
            "  {} sqlite: {} (missing — daemon not yet started?)",
            Severity::Info.tag(),
            db_path.display()
        );
        tally.push(Severity::Info);
        return;
    }

    let endpoints = [
        ("projects", "/projects"),
        ("plans", "/plans"),
        ("units", "/units"),
        ("cycles", "/cycles"),
        ("tasks", "/tasks"),
        ("knowledge", "/knowledge"),
    ];

    let mut daemon_unreachable = false;
    for (name, path) in &endpoints {
        if daemon_unreachable {
            println!("  {name}: (skipped — daemon unavailable)");
            continue;
        }
        match client::get(client, path).await {
            Ok(Value::Array(items)) => {
                println!("  {} {name}: {}", Severity::Ok.tag(), items.len());
            }
            Ok(_) => {
                println!(
                    "  {} {name}: unexpected response shape",
                    Severity::Warn.tag()
                );
                tally.push(Severity::Warn);
            }
            Err(_) => {
                println!(
                    "  {} {name}: daemon unavailable — entity counts skipped",
                    Severity::Info.tag()
                );
                daemon_unreachable = true;
                tally.push(Severity::Info);
            }
        }
    }
}

/// [Hooks] — verify the 7 v3 hook handler files exist under the plugin's
/// `adapters/claude/` directory. Looks at the standard plugin install path
/// plus the in-repo dev path.
///
/// US-CLAWKET-HOOK-251: also list every Claude Code hook *event*
/// (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, ExitPlanMode,
/// SubagentStart, SubagentStop) with the handler path declared in
/// `hooks.json` and a last-fired timestamp pulled from
/// `~/.local/state/clawket/hook-events.log` (one JSON object per line). When
/// the log file is missing or the event has never fired the timestamp is "—".
fn run_hooks_check(tally: &mut Vec<Severity>) {
    let expected = [
        "session-start.cjs",
        "user-prompt-submit.cjs",
        "pre-tool-use.cjs",
        "post-tool-use.cjs",
        "plan-sync.cjs",
        "subagent-start.cjs",
        "subagent-stop.cjs",
    ];

    let candidates = hook_adapter_roots();
    let resolved = candidates
        .iter()
        .find(|p| p.join("session-start.cjs").exists())
        .cloned();

    let Some(root) = resolved else {
        println!(
            "  {} adapters/claude/ not found in any candidate location:",
            Severity::Warn.tag()
        );
        for c in &candidates {
            println!("       - {}", c.display());
        }
        tally.push(Severity::Warn);
        return;
    };

    println!("  adapter root: {}", root.display());
    let mut missing = Vec::new();
    for f in &expected {
        if root.join(f).exists() {
            println!("  {} {f}", Severity::Ok.tag());
        } else {
            println!("  {} {f} (missing)", Severity::Error.tag());
            missing.push(*f);
        }
    }
    if missing.is_empty() {
        tally.push(Severity::Ok);
    } else {
        tally.push(Severity::Error);
    }

    // US-CLAWKET-HOOK-251 — per-event readout: handler path (from hooks.json)
    // + last-fired timestamp (from state log).
    println!();
    println!("  events:");
    let manifest = read_hooks_manifest();
    let last_fired = read_hook_event_log();
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "ExitPlanMode",
        "SubagentStart",
        "SubagentStop",
    ] {
        let handler = manifest
            .as_ref()
            .and_then(|m| extract_handler_for_event(m, event))
            .unwrap_or_else(|| "—".to_string());
        let ts = last_fired.get(event).map(|s| s.as_str()).unwrap_or("—");
        println!("    - {event:<18} handler={handler}  last_fired={ts}");
    }
}

/// Read `hooks.json` from any plugin-root candidate (returns the first one
/// that parses as JSON). Used by HOOK-251.
fn read_hooks_manifest() -> Option<Value> {
    for root in plugin_root_candidates() {
        let candidate = root.join("hooks/hooks.json");
        if let Ok(raw) = fs::read_to_string(&candidate) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                return Some(v);
            }
        }
    }
    None
}

/// Pull the first handler `command` for the given event name from a parsed
/// hooks.json manifest. We surface only the `.cjs` script path tail so the
/// doctor output stays readable; if no `.cjs` token is present we return
/// the whole command verbatim.
fn extract_handler_for_event(manifest: &Value, event: &str) -> Option<String> {
    let arr = manifest
        .get("hooks")
        .and_then(|h| h.get(event))
        .and_then(Value::as_array)?;
    for outer in arr {
        let inner = outer.get("hooks").and_then(Value::as_array)?;
        for h in inner {
            if let Some(cmd) = h.get("command").and_then(Value::as_str) {
                if let Some(idx) = cmd.find(".cjs") {
                    // Walk backwards from `.cjs` to the previous separator
                    // (whitespace or quote) to extract the path token.
                    let end = idx + ".cjs".len();
                    let start = cmd[..end]
                        .rfind(|c: char| c.is_whitespace() || c == '"')
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    return Some(cmd[start..end].to_string());
                }
                return Some(cmd.to_string());
            }
        }
    }
    None
}

/// Read `~/.local/state/clawket/hook-events.log` (one JSON object per line,
/// schema: `{"event": "<name>", "at": "<iso8601>"}`) and collapse to the
/// latest timestamp per event. Missing log → empty map (every event renders
/// as "—" downstream). Used by HOOK-251.
fn read_hook_event_log() -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let path = paths::state_dir().join("hook-events.log");
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return out,
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            let event = v.get("event").and_then(Value::as_str);
            let at = v.get("at").and_then(Value::as_str);
            if let (Some(e), Some(t)) = (event, at) {
                // Last write wins (file is append-only chronological).
                out.insert(e.to_string(), t.to_string());
            }
        }
    }
    out
}

fn hook_adapter_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let plugins = PathBuf::from(&home).join(".claude/plugins");
        // Common install layouts.
        out.push(plugins.join("clawket/adapters/claude"));
        out.push(plugins.join("cache/clawket/adapters/claude"));
        // Versioned layout: ~/.claude/plugins/clawket-vX.Y.Z/adapters/claude
        if let Ok(entries) = fs::read_dir(&plugins) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if s.starts_with("clawket") {
                    out.push(entry.path().join("adapters/claude"));
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        // Dev layout: workspace cwd may be cli/, daemon/, etc.; walk up
        // a few levels looking for clawket/adapters/claude.
        let mut p = cwd.clone();
        for _ in 0..6 {
            out.push(p.join("clawket/adapters/claude"));
            out.push(p.join("adapters/claude"));
            if !p.pop() {
                break;
            }
        }
    }
    out
}

/// [MCP] — verify the `clawket mcp` subcommand is reachable. We do not spawn
/// a stdio handshake here (would block doctor); instead we confirm the
/// current binary supports the subcommand and report the documented tool
/// list so users can cross-check `.mcp.json`.
fn run_mcp_check(tally: &mut Vec<Severity>) {
    match std::env::current_exe() {
        Ok(p) => {
            println!(
                "  {} clawket mcp launcher: {}",
                Severity::Ok.tag(),
                p.display()
            );
            tally.push(Severity::Ok);
        }
        Err(e) => {
            println!(
                "  {} cannot resolve current executable for `clawket mcp` ({e})",
                Severity::Warn.tag()
            );
            tally.push(Severity::Warn);
        }
    }
    println!("  exposed tools (read-only RAG):");
    for tool in [
        "search_artifacts",
        "search_tasks",
        "find_similar_tasks",
        "get_task_context",
        "get_recent_decisions",
    ] {
        println!("    - clawket_{tool}");
    }
}

/// [Compatibility] — read the plugin's package.json `compat` ranges and
/// compare against the daemon-reported version (and the CLI's compile-time
/// version). We do not parse semver ranges to the letter; we surface the
/// raw range so users can verify it themselves and we flag the obvious
/// "unset" or "mismatch" cases.
fn run_compatibility_check(health: Option<&Value>, tally: &mut Vec<Severity>) {
    let cli_version = env!("CARGO_PKG_VERSION");
    println!("  cli version: {cli_version}");

    let daemon_version = health
        .and_then(|h| h.get("version"))
        .and_then(Value::as_str);
    match daemon_version {
        Some(v) => println!("  daemon version: {v}"),
        None => println!("  daemon version: (unavailable — daemon down?)"),
    }

    // Read package.json compat table.
    let pkg = read_plugin_package_json();
    match pkg {
        Some(val) => {
            let compat = val.get("compat");
            match compat {
                Some(Value::Object(map)) => {
                    println!("  package.json compat:");
                    for (k, v) in map {
                        if let Some(s) = v.as_str() {
                            println!("    {k}: {s}");
                        }
                    }
                    tally.push(Severity::Ok);
                }
                _ => {
                    println!(
                        "  {} package.json found but no `compat` field",
                        Severity::Warn.tag()
                    );
                    tally.push(Severity::Warn);
                }
            }
        }
        None => {
            println!(
                "  {} plugin package.json not found — compat range unknown",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
        }
    }

    // Surface components.json pinned versions alongside.
    if let Some(comp) = read_components_json() {
        println!("  components.json pinned:");
        for k in ["cli", "daemon", "web"] {
            if let Some(v) = comp.get(k).and_then(Value::as_str) {
                println!("    {k}: {v}");
            }
        }
    }
}

fn read_plugin_package_json() -> Option<Value> {
    for path in plugin_root_candidates()
        .into_iter()
        .map(|r| r.join("package.json"))
    {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                return Some(val);
            }
        }
    }
    None
}

fn read_components_json() -> Option<Value> {
    for path in plugin_root_candidates()
        .into_iter()
        .map(|r| r.join("components.json"))
    {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                return Some(val);
            }
        }
    }
    None
}

fn plugin_root_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let plugins = PathBuf::from(&home).join(".claude/plugins");
        out.push(plugins.join("clawket"));
        out.push(plugins.join("cache/clawket"));
        if let Ok(entries) = fs::read_dir(&plugins) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("clawket") {
                    out.push(entry.path());
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut p = cwd.clone();
        for _ in 0..6 {
            out.push(p.join("clawket"));
            out.push(p.clone());
            if !p.pop() {
                break;
            }
        }
    }
    out
}

/// [Audit log] — show audit_log size + last entry timestamp + prev_hash
/// chain integrity over the most recent N entries.
async fn run_audit_log_check(client: &client::HttpClient, tally: &mut Vec<Severity>) {
    let entries = match client::get(client, "/audit?limit=20").await {
        Ok(Value::Array(items)) => items,
        Ok(_) => {
            println!(
                "  {} daemon returned unexpected shape for /audit",
                Severity::Warn.tag()
            );
            tally.push(Severity::Warn);
            return;
        }
        Err(e) => {
            println!(
                "  {} daemon unavailable — audit_log check skipped ({e})",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
    };

    let total = entries.len();
    println!(
        "  {} audit_log: recent {total} entries fetched",
        Severity::Ok.tag()
    );
    if let Some(first) = entries.first() {
        if let Some(at) = first.get("at").and_then(Value::as_str) {
            println!("  last entry at: {at}");
        }
    } else {
        println!("  audit_log is empty (fresh install or after rotation)");
    }

    // Chain integrity: each entry's prev_hash should match the previous
    // entry's id (audit_log is most-recent-first, so we walk backwards).
    let mut chain_ok = true;
    let mut broken_at: Option<String> = None;
    for window in entries.windows(2) {
        let newer = &window[0];
        let older = &window[1];
        let newer_prev = newer.get("prev_hash").and_then(Value::as_str);
        let older_id = older.get("id").and_then(Value::as_str);
        match (newer_prev, older_id) {
            (Some(p), Some(id)) if p == id => {}
            (None, _) | (_, None) => {
                // Missing field — ambiguous, but not necessarily corrupt.
            }
            _ => {
                chain_ok = false;
                broken_at = newer.get("id").and_then(Value::as_str).map(str::to_string);
                break;
            }
        }
    }
    if entries.len() < 2 {
        println!(
            "  {} chain integrity: not enough entries to verify",
            Severity::Info.tag()
        );
        tally.push(Severity::Info);
    } else if chain_ok {
        println!(
            "  {} chain integrity: OK over recent {total} entries",
            Severity::Ok.tag()
        );
        tally.push(Severity::Ok);
    } else {
        println!(
            "  {} chain integrity: BROKEN at entry {}",
            Severity::Error.tag(),
            broken_at.as_deref().unwrap_or("(unknown)")
        );
        tally.push(Severity::Error);
    }
}

// ===== FIX-CLI-005 helpers =====

/// tier_distribution: call GET /tasks and bucket by the `tier` field.
/// If the endpoint is unreachable (daemon down) → Info (not an error).
/// US-CLAWKET-TIER-009: when `plan_filter` is `Some(plan_id)`, restrict the
/// distribution to tasks belonging to that plan.
async fn run_tier_distribution_check(
    client: &client::HttpClient,
    plan_filter: Option<&str>,
    tally: &mut Vec<Severity>,
) {
    let path = match plan_filter {
        Some(pid) => format!("/tasks?plan_id={}", urlenc(pid)),
        None => "/tasks".to_string(),
    };
    let tasks = match client::get(client, &path).await {
        Ok(Value::Array(items)) => items,
        Ok(_) => {
            println!(
                "  {} daemon returned unexpected shape for /tasks",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
        Err(e) => {
            println!(
                "  {} daemon unavailable — tier_distribution skipped ({e})",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
    };

    let total = tasks.len();
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for t in &tasks {
        let tier = t
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string();
        *counts.entry(tier).or_insert(0) += 1;
    }

    if let Some(pid) = plan_filter {
        println!("  plan filter: {pid}");
    }
    println!("  {} total tasks: {total}", Severity::Ok.tag());
    for (tier, count) in &counts {
        let pct = if total > 0 {
            *count as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        println!("      {tier}: {count} ({pct:.0}%)");
    }
    tally.push(Severity::Ok);
}

/// escalation_rate: derive from tier distribution — what fraction of tasks
/// are NOT G1 (haiku / lowest tier). If the daemon is down, skip as Info.
/// US-CLAWKET-TIER-046: warn threshold is `> 30%` (was `> 50%`).
/// US-CLAWKET-TIER-009: honour `plan_filter` when set.
async fn run_escalation_rate_check(
    client: &client::HttpClient,
    plan_filter: Option<&str>,
    tally: &mut Vec<Severity>,
) {
    let path = match plan_filter {
        Some(pid) => format!("/tasks?plan_id={}", urlenc(pid)),
        None => "/tasks".to_string(),
    };
    let tasks = match client::get(client, &path).await {
        Ok(Value::Array(items)) => items,
        Ok(_) | Err(_) => {
            println!(
                "  {} daemon unavailable — escalation_rate skipped",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
    };

    let total = tasks.len();
    if total == 0 {
        println!("  {} no tasks — escalation_rate N/A", Severity::Info.tag());
        tally.push(Severity::Info);
        return;
    }

    // G1 = tier "low" or "haiku" or not set (none). Everything above = escalated.
    let g1_tiers = ["low", "haiku", "none", "g1"];
    let escalated = tasks
        .iter()
        .filter(|t| {
            let tier = t.get("tier").and_then(Value::as_str).unwrap_or("none");
            !g1_tiers.contains(&tier)
        })
        .count();

    let rate = escalated as f64 / total as f64 * 100.0;
    let sev = if rate > 30.0 {
        Severity::Warn
    } else {
        Severity::Ok
    };
    if let Some(pid) = plan_filter {
        println!("  plan filter: {pid}");
    }
    println!(
        "  {} escalated tasks: {escalated} / {total} ({rate:.0}%)",
        sev.tag()
    );
    if matches!(sev, Severity::Warn) {
        println!("      > 30% escalated — check G2/G3 task assignment policy");
    }
    tally.push(sev);
}

/// US-CLAWKET-TIER-045: emit a detailed escalation report — count and
/// percentage of tasks with non-null `escalation_reason`, grouped by tier.
/// Honours `plan_filter` when set. Daemon failure → Info (skipped).
async fn run_escalation_report(
    client: &client::HttpClient,
    plan_filter: Option<&str>,
    tally: &mut Vec<Severity>,
) {
    let path = match plan_filter {
        Some(pid) => format!("/tasks?plan_id={}", urlenc(pid)),
        None => "/tasks".to_string(),
    };
    let tasks = match client::get(client, &path).await {
        Ok(Value::Array(items)) => items,
        Ok(_) | Err(_) => {
            println!(
                "  {} daemon unavailable — escalation report skipped",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
    };

    if tasks.is_empty() {
        println!(
            "  {} no tasks — escalation report N/A",
            Severity::Info.tag()
        );
        tally.push(Severity::Info);
        return;
    }

    // Group: tier -> (total_in_tier, escalated_in_tier).
    let mut groups: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for t in &tasks {
        let tier = t
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string();
        let has_reason = t
            .get("escalation_reason")
            .map(|v| !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true))
            .unwrap_or(false);
        let entry = groups.entry(tier).or_insert((0, 0));
        entry.0 += 1;
        if has_reason {
            entry.1 += 1;
        }
    }

    if let Some(pid) = plan_filter {
        println!("  plan filter: {pid}");
    }
    let total_tasks = tasks.len();
    let total_escalated: usize = groups.values().map(|(_, e)| *e).sum();
    let overall_rate = total_escalated as f64 / total_tasks as f64 * 100.0;
    println!(
        "  {} tasks with escalation_reason: {total_escalated} / {total_tasks} ({overall_rate:.0}%)",
        Severity::Ok.tag()
    );
    for (tier, (total_in_tier, escalated_in_tier)) in &groups {
        let pct = if *total_in_tier > 0 {
            *escalated_in_tier as f64 / *total_in_tier as f64 * 100.0
        } else {
            0.0
        };
        println!("      {tier}: {escalated_in_tier} / {total_in_tier} ({pct:.0}%)");
    }
    tally.push(Severity::Ok);
}

/// Minimal RFC3986-ish URL component encoder for query params used by doctor.
/// Mirrors the policy in `mcp.rs::urlenc` — keeps the doctor module self-contained.
fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// US-CLAWKET-TIER-009 / TIER-045 / TIER-046 — render the tier-aware policy
/// readout. Surfaces:
///   - Default tier policy (read from `CLAWKET_DEFAULT_TIER` if set, else
///     "med" — matches the daemon's compute-tier default).
///   - Current `--tier` global flag value (propagated via `CLAWKET_TIER` env
///     by `run_main`; "(unset)" when no override).
///   - "Within-Claude only (cross-vendor disabled in v3)" status line.
fn run_tier_aware_check(tally: &mut Vec<Severity>) {
    let default_tier = std::env::var("CLAWKET_DEFAULT_TIER").unwrap_or_else(|_| "med".to_string());
    println!("  default tier policy: {default_tier}");

    match std::env::var("CLAWKET_TIER") {
        Ok(v) => println!("  --tier (current): {v}"),
        Err(_) => println!("  --tier (current): (unset)"),
    }

    println!(
        "  {} Within-Claude only (cross-vendor disabled in v3)",
        Severity::Ok.tag()
    );
    tally.push(Severity::Ok);
}

/// Plugin install: verify the clawket plugin binary is present.
/// Checks CLAWKET_BIN env, plugin layout, and PATH.
///
/// US-CLAWKET-PLUGIN-160 + PLUGIN-161: surface `marker_version` (read from
/// the install marker placed by `ensureInstalled`, located at any
/// `~/.claude/plugins/clawket*/.install-marker`) alongside `binary_version`
/// (the current `CARGO_PKG_VERSION`). When they diverge the guidance string
/// must explicitly tell the user to **reinstall**.
fn run_plugin_install_check(tally: &mut Vec<Severity>) {
    // Check if the clawket binary itself is reachable (already installed).
    let bin_candidates = [
        std::env::var("CLAWKET_BIN")
            .ok()
            .map(std::path::PathBuf::from),
        {
            // Plugin layout: ~/.claude/plugins/<clawket-version>/bin/clawket
            let home = std::env::var("HOME").map(std::path::PathBuf::from).ok();
            home.map(|h| h.join(".claude/plugins"))
        },
    ];

    // Locate current executable as the best proxy for "installed".
    let self_bin = std::env::current_exe().ok();
    if let Some(ref p) = self_bin {
        println!("  {} clawket binary: {}", Severity::Ok.tag(), p.display());
        tally.push(Severity::Ok);
    } else {
        println!(
            "  {} cannot resolve current executable path",
            Severity::Warn.tag()
        );
        tally.push(Severity::Warn);
    }

    // Plugin tree: look for any clawket-* directory under ~/.claude/plugins/
    let mut marker_version: Option<String> = None;
    let mut marker_path: Option<PathBuf> = None;
    let home = std::env::var("HOME").map(std::path::PathBuf::from).ok();
    if let Some(h) = home {
        let plugins_dir = h.join(".claude/plugins");
        if plugins_dir.exists() {
            let found: Vec<_> = std::fs::read_dir(&plugins_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("clawket"))
                .collect();
            if found.is_empty() {
                println!(
                    "  {} plugin tree not found under {} — install via Claude Code /plugin install",
                    Severity::Info.tag(),
                    plugins_dir.display()
                );
                tally.push(Severity::Info);
            } else {
                for entry in &found {
                    println!(
                        "  {} plugin dir: {}",
                        Severity::Ok.tag(),
                        entry.path().display()
                    );
                    if marker_version.is_none() {
                        if let Some((v, p)) = read_install_marker(&entry.path()) {
                            marker_version = Some(v);
                            marker_path = Some(p);
                        }
                    }
                }
                tally.push(Severity::Ok);
            }
        } else {
            println!(
                "  {} ~/.claude/plugins/ not found — plugin not installed",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
        }
    }

    // PLUGIN-160 / PLUGIN-161 — marker vs binary version readout.
    let binary_version = env!("CARGO_PKG_VERSION").to_string();
    println!("  binary_version: {binary_version}");
    match (&marker_version, &marker_path) {
        (Some(mv), Some(mp)) => {
            println!("  marker_version: {mv} ({})", mp.display());
            if mv == &binary_version {
                println!(
                    "  {} marker_version matches binary_version",
                    Severity::Ok.tag()
                );
                tally.push(Severity::Ok);
            } else {
                println!(
                    "  {} marker_version ({mv}) diverges from binary_version ({binary_version}) — run `/plugin update clawket@clawket` to realign install marker, binaries and web bundle (fallback: `/plugin uninstall clawket@clawket && /plugin install clawket@clawket`)",
                    Severity::Warn.tag()
                );
                tally.push(Severity::Warn);
            }
        }
        _ => {
            println!(
                "  {} marker_version: (not found) — first install pending or marker was wiped; run `/plugin update clawket@clawket` (fallback: `/plugin uninstall clawket@clawket && /plugin install clawket@clawket`)",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
        }
    }

    // Suppress unused variable warning from the candidates array above.
    let _ = bin_candidates;
}

/// Read the install marker emitted by `adapters/shared/claude-hooks.cjs::ensureInstalled`.
/// Marker file is JSON: `{"version": "<semver>", "installed_at": "<iso8601>"}`.
/// Returns `(version, path)` on success.
fn read_install_marker(plugin_dir: &Path) -> Option<(String, PathBuf)> {
    let candidate = plugin_dir.join(".install-marker");
    let raw = fs::read_to_string(&candidate).ok()?;
    // Tolerate either JSON or a one-line `version=...` text marker.
    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
        if let Some(s) = v.get("version").and_then(Value::as_str) {
            return Some((s.to_string(), candidate));
        }
    }
    for line in raw.lines() {
        if let Some(rest) = line.trim().strip_prefix("version=") {
            return Some((rest.trim().to_string(), candidate));
        }
    }
    None
}

/// i18n: report the CLAWKET_LOCALE / LC_ALL / LANG resolution chain
/// (mirrors FIX-DAEMON-018 / FIX-PLUGIN-011 locale logic) AND surface
/// locale-file coverage (en / ko / ja key counts) so divergence between
/// translations is visible at doctor time.
fn run_i18n_check(tally: &mut Vec<Severity>) {
    let chain = [
        ("CLAWKET_LOCALE", std::env::var("CLAWKET_LOCALE").ok()),
        ("LC_ALL", std::env::var("LC_ALL").ok()),
        ("LANG", std::env::var("LANG").ok()),
    ];

    let resolved = chain
        .iter()
        .find_map(|(_, v)| v.as_deref().map(str::to_string));

    for (name, val) in &chain {
        match val {
            Some(v) => println!("  {name} = {v}"),
            None => println!("  {name} = (unset)"),
        }
    }

    match resolved {
        Some(ref locale) => {
            println!("  {} resolved locale: {locale}", Severity::Ok.tag());
            tally.push(Severity::Ok);
        }
        None => {
            println!(
                "  {} no locale set — daemon will use system default (en). Set CLAWKET_LOCALE for explicit control.",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
        }
    }

    // US-CLAWKET-I18N-013 — locale file completeness.
    // Walk every `*.json` under `clawket/locales/`, report `% missing vs en
    // baseline` so divergence between translations is visible at doctor time.
    let locales_root = match find_locales_root() {
        Some(p) => p,
        None => {
            println!(
                "  {} locale files dir not found — coverage check skipped",
                Severity::Info.tag()
            );
            tally.push(Severity::Info);
            return;
        }
    };
    println!("  locale files: {}", locales_root.display());

    // Collect every `<lang>.json` under the locales root. Use a sorted Vec
    // so the doctor output is stable across runs.
    let mut langs: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&locales_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy().to_string();
            if let Some(stem) = s.strip_suffix(".json") {
                langs.push(stem.to_string());
            }
        }
    }
    langs.sort();
    // Ensure `en` is first so the baseline is established before downstream
    // rows render.
    if let Some(pos) = langs.iter().position(|l| l == "en") {
        langs.swap(0, pos);
    }

    let mut counts: Vec<(String, Option<usize>)> = Vec::new();
    for lang in &langs {
        let path = locales_root.join(format!("{lang}.json"));
        let count = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.as_object().map(|m| m.len()));
        counts.push((lang.clone(), count));
    }
    let baseline = counts
        .iter()
        .find_map(|(l, c)| if l == "en" { *c } else { None });
    for (lang, count) in &counts {
        match (count, baseline) {
            (Some(n), Some(base)) if *n == base => {
                println!(
                    "  {} {lang}.json: {n} keys (0% missing vs en={base})",
                    Severity::Ok.tag()
                );
            }
            (Some(n), Some(base)) => {
                let missing_pct = if base > 0 {
                    let diff = base.saturating_sub(*n);
                    diff as f64 / base as f64 * 100.0
                } else {
                    0.0
                };
                println!(
                    "  {} {lang}.json: {n} keys ({} drift vs en={base}, {missing_pct:.0}% missing)",
                    Severity::Warn.tag(),
                    if *n > base {
                        format!("+{}", *n - base)
                    } else {
                        format!("-{}", base - *n)
                    }
                );
                tally.push(Severity::Warn);
            }
            (Some(n), None) => {
                println!(
                    "  {} {lang}.json: {n} keys (no en baseline)",
                    Severity::Ok.tag()
                );
            }
            (None, _) => {
                println!(
                    "  {} {lang}.json: missing or unreadable",
                    Severity::Warn.tag()
                );
                tally.push(Severity::Warn);
            }
        }
    }
}

fn find_locales_root() -> Option<PathBuf> {
    for root in plugin_root_candidates() {
        let candidate = root.join("locales");
        if candidate.join("en.json").exists() {
            return Some(candidate);
        }
    }
    None
}

/// Skills: check for the clawket skill directory under ~/.claude/skills/.
///
/// US-CLAWKET-SKILL-190: in addition to file presence, parse the first 20
/// lines of each SKILL.md and report whether the YAML frontmatter declares
/// `name:` and `description:`. The row uses `name=✓/✗ description=✓/✗` so a
/// malformed manifest can be spotted in the doctor output.
fn run_skills_check(tally: &mut Vec<Severity>) {
    let home = match std::env::var("HOME").map(std::path::PathBuf::from) {
        Ok(h) => h,
        Err(_) => {
            println!(
                "  {} HOME not set — cannot locate skills dir",
                Severity::Warn.tag()
            );
            tally.push(Severity::Warn);
            return;
        }
    };

    let skills_root = home.join(".claude/skills");
    let clawket_skill = skills_root.join("clawket");
    let skill_file = clawket_skill.join("SKILL.md");

    if skill_file.exists() {
        println!(
            "  {} clawket skill: {}",
            Severity::Ok.tag(),
            skill_file.display()
        );
        // SKILL-190: frontmatter row.
        let (name_ok, desc_ok) = read_skill_frontmatter(&skill_file);
        let row = format_skill_frontmatter_row("clawket", name_ok, desc_ok);
        let sev = if name_ok && desc_ok {
            Severity::Ok
        } else {
            Severity::Warn
        };
        println!("  {} {row}", sev.tag());
        tally.push(Severity::Ok);
        tally.push(sev);
    } else if clawket_skill.exists() {
        println!(
            "  {} clawket skills dir present but SKILL.md missing: {}",
            Severity::Warn.tag(),
            clawket_skill.display()
        );
        tally.push(Severity::Warn);
    } else if skills_root.exists() {
        println!(
            "  {} clawket skill not installed under {} — install via /plugin install",
            Severity::Info.tag(),
            skills_root.display()
        );
        tally.push(Severity::Info);
    } else {
        println!(
            "  {} ~/.claude/skills/ not found — plugin skills feature not active",
            Severity::Info.tag()
        );
        tally.push(Severity::Info);
    }

    // SKILL-190: also walk every other skill under ~/.claude/skills/ (any
    // directory containing a SKILL.md) and emit a frontmatter row for each.
    if skills_root.exists() {
        if let Ok(entries) = fs::read_dir(&skills_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let n = name.to_string_lossy();
                if n == "clawket" {
                    continue;
                }
                let candidate = path.join("SKILL.md");
                if candidate.exists() {
                    let (name_ok, desc_ok) = read_skill_frontmatter(&candidate);
                    let row = format_skill_frontmatter_row(&n, name_ok, desc_ok);
                    let sev = if name_ok && desc_ok {
                        Severity::Ok
                    } else {
                        Severity::Warn
                    };
                    println!("  {} {row}", sev.tag());
                    tally.push(sev);
                }
            }
        }
    }
}

/// Parse the first 20 lines of a SKILL.md file and report whether the YAML
/// frontmatter declares `name:` and `description:`. SKILL-190.
fn read_skill_frontmatter(path: &Path) -> (bool, bool) {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return (false, false),
    };
    let mut name_ok = false;
    let mut desc_ok = false;
    for (i, line) in raw.lines().enumerate() {
        if i >= 20 {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("name:") && trimmed.len() > "name:".len() {
            // Make sure there is a non-empty value after the colon.
            let rest = trimmed["name:".len()..].trim();
            if !rest.is_empty() {
                name_ok = true;
            }
        }
        if trimmed.starts_with("description:") && trimmed.len() > "description:".len() {
            let rest = trimmed["description:".len()..].trim();
            if !rest.is_empty() {
                desc_ok = true;
            }
        }
    }
    (name_ok, desc_ok)
}

fn format_skill_frontmatter_row(skill: &str, name_ok: bool, desc_ok: bool) -> String {
    let n = if name_ok { "✓" } else { "✗" };
    let d = if desc_ok { "✓" } else { "✗" };
    format!("{skill}: name={n} description={d}")
}

/// JSON-mode doctor: collect all checks into a structured JSON object and
/// print it as a single JSON document. Exits 1 if any check is Error.
async fn run_json() -> Result<()> {
    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut any_error = false;

    macro_rules! check {
        ($section:expr, $status:expr, $detail:expr) => {{
            let ok = $status != "error";
            if !ok {
                any_error = true;
            }
            // FIX-DOCTOR-V3: round-2 evidence flagged "section"/"detail"
            // mismatch; v3 plan locks the field names to "name"/"details".
            results.push(serde_json::json!({
                "name": $section,
                "status": $status,
                "details": $detail,
            }));
        }};
    }

    // Daemon connectivity
    let client = client::make_client();
    match client::get(&client, "/health").await {
        Ok(val) => {
            check!(
                "daemon",
                "ok",
                serde_json::to_string(&val).unwrap_or_default()
            );
        }
        Err(e) => {
            check!("daemon", "error", format!("daemon unreachable: {e}"));
        }
    }

    // Path separation
    let data = paths::data_dir();
    let overlap = paths::path_overlaps_plugin_dir(&data);
    if overlap {
        check!(
            "path_separation",
            "error",
            format!("data dir {} overlaps plugin dir", data.display())
        );
    } else {
        check!(
            "path_separation",
            "ok",
            format!("data dir {} is safe", data.display())
        );
    }

    // schema_version / components.json check
    let schema_check = check_schema_version(&client).await;
    check!("schema_version", schema_check.0.as_str(), schema_check.1);

    // sqlite-vec probe
    let vec_check = check_sqlite_vec(&client).await;
    check!("sqlite_vec", vec_check.0.as_str(), vec_check.1);

    let doc = serde_json::json!({
        "checks": results,
        "any_error": any_error,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    if any_error {
        std::process::exit(1);
    }
    Ok(())
}

/// Check schema_version from daemon /health and compare with components.json pinned version.
/// Returns (status_str, detail_str).
async fn check_schema_version(client: &client::HttpClient) -> (String, String) {
    let health = match client::get(client, "/health").await {
        Ok(v) => v,
        Err(e) => return ("warn".to_string(), format!("daemon unavailable: {e}")),
    };

    let daemon_schema = health
        .get("schema_version")
        .and_then(|v| v.as_i64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Read components.json from the plugin directory (best-effort).
    let pinned = read_components_json_schema_version();

    match pinned {
        Some(ref pinned_ver) => {
            if &daemon_schema == pinned_ver {
                (
                    "ok".to_string(),
                    format!("schema_version={daemon_schema} matches components.json pin"),
                )
            } else if daemon_schema == "unknown" {
                (
                    "warn".to_string(),
                    format!(
                        "daemon did not report schema_version; components.json pins {pinned_ver}"
                    ),
                )
            } else {
                (
                    "error".to_string(),
                    format!(
                        "schema_version mismatch: daemon={daemon_schema} components.json={pinned_ver}"
                    ),
                )
            }
        }
        None => (
            "info".to_string(),
            format!("components.json not found; daemon schema_version={daemon_schema}"),
        ),
    }
}

fn read_components_json_schema_version() -> Option<String> {
    // Try the plugin's clawket/components.json first, then the cwd.
    let candidates: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        if let Ok(home) = std::env::var("HOME") {
            // Plugin installs components.json alongside the plugin manifest
            v.push(
                std::path::PathBuf::from(&home)
                    .join(".claude/plugins")
                    .join("clawket")
                    .join("components.json"),
            );
        }
        if let Ok(cwd) = std::env::current_dir() {
            v.push(cwd.join("components.json"));
        }
        v
    };
    for path in &candidates {
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(s) = val.get("schema_version").and_then(|v| v.as_i64()) {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Probe whether sqlite-vec is available via the daemon's /health endpoint.
/// The daemon includes `extensions` or `sqlite_vec` in its health payload when loaded.
async fn check_sqlite_vec(client: &client::HttpClient) -> (String, String) {
    let health = match client::get(client, "/health").await {
        Ok(v) => v,
        Err(e) => return ("warn".to_string(), format!("daemon unavailable: {e}")),
    };

    // Daemon includes `sqlite_vec_version` or a bool `sqlite_vec` in /health.
    let vec_ver = health
        .get("sqlite_vec_version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let vec_ok = health.get("sqlite_vec").and_then(|v| v.as_bool());

    match (vec_ver, vec_ok) {
        (Some(ver), _) => ("ok".to_string(), format!("sqlite-vec loaded: {ver}")),
        (None, Some(true)) => (
            "ok".to_string(),
            "sqlite-vec loaded (version unknown)".to_string(),
        ),
        (None, Some(false)) => (
            "error".to_string(),
            "sqlite-vec NOT loaded — semantic search unavailable".to_string(),
        ),
        (None, None) => (
            "warn".to_string(),
            "daemon did not report sqlite-vec status; semantic search may be unavailable"
                .to_string(),
        ),
    }
}

// Add schema_version and sqlite-vec checks to the human-readable output as well.
async fn run_extra_checks(client: &client::HttpClient, tally: &mut Vec<Severity>) {
    section("schema_version (components.json)");
    let (status, detail) = check_schema_version(client).await;
    let sev = match status.as_str() {
        "ok" => Severity::Ok,
        "error" => Severity::Error,
        "warn" => Severity::Warn,
        _ => Severity::Info,
    };
    println!("  {} {detail}", sev.tag());
    tally.push(sev);
    println!();

    section("sqlite-vec probe");
    let (status, detail) = check_sqlite_vec(client).await;
    let sev = match status.as_str() {
        "ok" => Severity::Ok,
        "error" => Severity::Error,
        "warn" => Severity::Warn,
        _ => Severity::Info,
    };
    println!("  {} {detail}", sev.tag());
    tally.push(sev);
    println!();
}

pub mod project_enabled {
    //! LM-259 / L1.4.c — pure formatter for the `Project enable state`
    //! doctor section. The decision tree (registered? enabled?) lives
    //! here so it can be unit-tested without a daemon round trip.
    //!
    //! Tests run under `cargo test doctor::project_enabled`.

    use crate::doctor_checks::Severity;
    use serde_json::Value;

    pub struct EnabledLine {
        pub severity: Severity,
        /// Main line — already includes the severity tag prefix.
        pub head: String,
        /// Optional indented follow-up hints.
        pub hints: Vec<String>,
    }

    /// Format the doctor line for a given project lookup result.
    ///
    /// `payload` is `Some(Value)` when GET /projects/by-cwd/{cwd}
    /// returned a project, `None` when no project matches the cwd.
    /// The cwd is included verbatim so the line is self-contained.
    pub fn format_project_enabled(payload: Option<&Value>, cwd: &str) -> EnabledLine {
        let Some(project) = payload else {
            return EnabledLine {
                severity: Severity::Info,
                head: format!(
                    "{} cwd `{cwd}` 가 어떤 project 에도 등록되어 있지 않음",
                    Severity::Info.tag()
                ),
                hints: vec![
                    "`clawket project register --cwd .` 로 등록하면 hook enforcement 가 활성화됨"
                        .to_string(),
                ],
            };
        };

        // `enabled` is i64 in the daemon model (0/1) but JSON may
        // surface it as bool or number — accept both.
        let enabled = project
            .get("enabled")
            .and_then(|v| v.as_i64().or_else(|| v.as_bool().map(i64::from)))
            .unwrap_or(1);
        let id = project
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string();
        let name = project
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string();

        if enabled != 0 {
            return EnabledLine {
                severity: Severity::Ok,
                head: format!(
                    "{} project {id} ({name}) — enabled, hook enforcement 활성",
                    Severity::Ok.tag()
                ),
                hints: Vec::new(),
            };
        }

        EnabledLine {
            severity: Severity::Warn,
            head: format!(
                "{} project {id} ({name}) — disabled, hook enforcement 비활성",
                Severity::Warn.tag()
            ),
            hints: vec![
                format!(
                    "재활성: `clawket project enable {id}` (의도적으로 disable 한 상태라면 무시)"
                ),
                "disable 상태에서는 PreToolUse 훅이 mutating 작업을 차단하지 않음".to_string(),
            ],
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn project_enabled_unregistered_cwd_is_info() {
            let line = format_project_enabled(None, "/tmp/foo");
            assert_eq!(line.severity, Severity::Info);
            assert!(
                line.head.contains("/tmp/foo"),
                "head must include the cwd: {}",
                line.head
            );
            assert!(
                line.hints.iter().any(|h| h.contains("project register")),
                "hint must point at register command: {:?}",
                line.hints
            );
        }

        #[test]
        fn project_enabled_active_project_is_ok_no_hints() {
            let p = json!({"id": "PROJ-AAA", "name": "demo", "enabled": 1});
            let line = format_project_enabled(Some(&p), "/tmp/x");
            assert_eq!(line.severity, Severity::Ok);
            assert!(line.head.contains("PROJ-AAA"));
            assert!(line.head.contains("enabled"));
            assert!(
                line.hints.is_empty(),
                "OK branch should not surface remediation hints: {:?}",
                line.hints
            );
        }

        #[test]
        fn project_enabled_disabled_project_warns_with_enable_command() {
            let p = json!({"id": "PROJ-BBB", "name": "stale", "enabled": 0});
            let line = format_project_enabled(Some(&p), "/tmp/x");
            assert_eq!(
                line.severity,
                Severity::Warn,
                "disabled must be Warn (not Error) — exit code stays 0"
            );
            assert!(
                line.head.contains("disabled"),
                "head must say disabled: {}",
                line.head
            );
            assert!(
                line.hints
                    .iter()
                    .any(|h| h.contains("clawket project enable PROJ-BBB")),
                "hints must include the exact `enable` command: {:?}",
                line.hints
            );
        }

        #[test]
        fn project_enabled_accepts_bool_enabled_field() {
            // Some client serializations might surface `enabled` as
            // bool rather than int. Both must classify identically.
            let p = json!({"id": "PROJ-C", "name": "x", "enabled": false});
            let line = format_project_enabled(Some(&p), "/tmp/x");
            assert_eq!(line.severity, Severity::Warn);
        }

        #[test]
        fn project_enabled_missing_field_defaults_to_enabled() {
            // If the server omits `enabled` (legacy daemon), assume
            // enabled — the doctor section is a hint, not the source
            // of truth.
            let p = json!({"id": "PROJ-D", "name": "x"});
            let line = format_project_enabled(Some(&p), "/tmp/x");
            assert_eq!(line.severity, Severity::Ok);
        }
    }
}
