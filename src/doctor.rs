// clawket doctor — diagnose daemon + install health.
//
// Prints a structured health snapshot so users (and the plugin's ensureDaemon
// hook) can identify why the daemon may have failed to start. Mirrors the
// check surface of the Node v2.2.1 equivalent.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::client;
use crate::paths;

pub async fn run() -> Result<()> {
    println!("Clawket doctor");
    println!("==============");
    println!();

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

    section("Resolved paths");
    let cache = paths::cache_dir();
    let socket = paths::socket_path();
    let pid = paths::pid_path();
    print_path("cache dir", &cache);
    print_path("socket", &socket);
    print_path("pid file", &pid);
    println!();

    section("Daemon binary discovery");
    let daemon_bin = resolve_daemon_bin();
    match daemon_bin {
        Some((p, reason)) => println!("  found: {} ({})", p.display(), reason),
        None => println!("  not found — set CLAWKET_DAEMON_BIN or install via plugin"),
    }
    println!();

    section("Daemon connectivity");
    let client = client::make_client();
    match client::get(&client, "/health").await {
        Ok(val) => {
            println!("  Unix socket /health → OK");
            println!(
                "  {}",
                serde_json::to_string_pretty(&val).unwrap_or_default()
            );
        }
        Err(e) => {
            println!("  Unix socket /health → FAIL: {e}");
            println!("  hint: run `clawket daemon start` (or restart)");
        }
    }
    println!();

    section("Legacy lattice data");
    let legacy = legacy_data_dir();
    if legacy.join("db.sqlite").exists() {
        println!(
            "  legacy DB present: {}",
            legacy.join("db.sqlite").display()
        );
        println!("  ⚠ Migration is NOT supported — schema changed too much across versions.");
        println!("  ⚠ Clawket treats every install as a fresh start.");
        println!(
            "  ⚠ If you no longer need the legacy data, remove {} manually.",
            legacy.display()
        );
    } else {
        println!("  no legacy lattice DB detected");
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
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let plugin = dir.join("..").join("daemon").join("bin").join("clawketd");
            if plugin.exists() {
                return Some((plugin, "plugin layout"));
            }
            let sibling = dir.join("clawketd");
            if sibling.exists() {
                return Some((sibling, "sibling"));
            }
        }
    }
    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        });
    if let Some(base) = data_home {
        let xdg = base.join("clawket").join("bin").join("clawketd");
        if xdg.exists() {
            return Some((xdg, "XDG install"));
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
