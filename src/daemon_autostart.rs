// ensureDaemon — standalone auto-spawn for CLI commands that need the daemon.
//
// Mirrors the logic in `clawket/adapters/shared/claude-hooks.cjs::ensureDaemon`
// but implemented in Rust so the CLI binary can auto-start clawketd without
// depending on Node. Used by `clawket mcp` and by any future CLI command that
// requires daemon connectivity (FIX-CLI-007).
//
// Spawn strategy:
//   1. Check Unix socket. If reachable → return Ok(()).
//   2. Check pid file. If pid is alive → socket not yet bound, wait briefly.
//   3. Acquire a filesystem spawn lock (flock on a lock file in cache dir) to
//      prevent two concurrent CLI invocations from both trying to spawn.
//   4. Re-check socket under the lock. If a sibling won the race → return Ok(()).
//   5. Spawn clawketd detached (new process group, /dev/null stdin, log file
//      for stdout+stderr). Drop child handle immediately.
//   6. Poll socket + pid for up to SPAWN_TIMEOUT_SECS. On timeout, return a
//      warning (don't hard-fail — the daemon might still be loading).

use anyhow::Result;
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::paths;

const SPAWN_TIMEOUT_SECS: u64 = 8;
const POLL_INTERVAL_MS: u64 = 100;

// ===== Public entry point =====

/// Ensure clawketd is running, auto-starting it if needed.
///
/// Returns `Ok(())` when the daemon is reachable or was successfully started.
/// Returns `Err` only when the lock cannot be acquired (which would be a very
/// unusual OS condition) — a failed spawn is reported as a warning on stderr
/// rather than propagated as an error, so the caller can still attempt the
/// command.
pub fn ensure_daemon() -> Result<()> {
    // Respect CLAWKET_NO_AUTOSPAWN: if set to any non-empty value,
    // skip auto-spawn entirely (US-CLAWKET-CLI-ENV-002).
    if std::env::var("CLAWKET_NO_AUTOSPAWN")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }

    // Fast path: socket already reachable.
    if socket_reachable() {
        return Ok(());
    }

    // Acquire a flock-based spawn lock so parallel CLI invocations don't all
    // try to spawn simultaneously. `fs2` would give a nicer API but we want
    // zero extra deps — use raw libc flock instead.
    let lock_path = paths::cache_dir().join("clawketd.spawn.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path);

    let lock_fd = match lock_file {
        Ok(f) => f,
        Err(e) => {
            // Can't create the lock file (permissions?). Log and try to spawn
            // anyway — worst case two processes race, and the second one's
            // spawn will be rejected by the daemon itself.
            eprintln!(
                "clawket: warning: cannot open spawn lock {}: {e}",
                lock_path.display()
            );
            return try_spawn_and_wait();
        }
    };

    // Non-blocking flock: if another process holds it we skip the spawn and
    // just wait for the socket.
    let flock_result = flock_exclusive_nb(lock_fd.as_raw_fd());
    if flock_result != 0 {
        // Another process is spawning. Wait for the socket.
        return wait_for_socket("waiting for daemon (another spawn in progress)");
    }

    // We hold the lock. Re-check socket before spawning.
    if socket_reachable() {
        // A sibling process already started the daemon.
        return Ok(());
    }

    let result = try_spawn_and_wait();

    // Lock is released when lock_fd is dropped here.
    drop(lock_fd);
    result
}

// ===== Internal helpers =====

fn socket_reachable() -> bool {
    use std::os::unix::net::UnixStream;
    UnixStream::connect(paths::socket_path()).is_ok()
}

fn read_pid() -> Option<u32> {
    fs::read_to_string(paths::pid_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn is_running(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn resolve_daemon_bin() -> String {
    if let Ok(bin) = std::env::var("CLAWKET_DAEMON_BIN") {
        let parts: Vec<&str> = bin.split_whitespace().collect();
        if !parts.is_empty() {
            return parts[0].to_string();
        }
    }
    for (candidate, _) in paths::daemon_bin_candidates() {
        if candidate.exists() {
            return candidate
                .canonicalize()
                .unwrap_or(candidate)
                .to_string_lossy()
                .into_owned();
        }
    }
    "clawketd".to_string()
}

fn log_file_path() -> Option<PathBuf> {
    if let Ok(state) = std::env::var("CLAWKET_STATE_DIR") {
        return Some(PathBuf::from(state).join("clawketd.log"));
    }
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("clawket").join("clawketd.log"))
}

fn try_spawn_and_wait() -> Result<()> {
    let program = resolve_daemon_bin();

    let (stdout_stdio, stderr_stdio) = match log_file_path() {
        Some(ref path) => {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::OpenOptions::new().create(true).append(true).open(path) {
                Ok(file) => match file.try_clone() {
                    Ok(clone) => (Stdio::from(file), Stdio::from(clone)),
                    Err(_) => (Stdio::null(), Stdio::null()),
                },
                Err(_) => (Stdio::null(), Stdio::null()),
            }
        }
        None => (Stdio::null(), Stdio::null()),
    };

    let mut cmd = Command::new(&program);
    cmd.arg("start")
        .stdin(Stdio::null())
        .stdout(stdout_stdio)
        .stderr(stderr_stdio);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            drop(child);
            eprintln!("clawket: auto-started clawketd (pid={pid})");
        }
        Err(e) => {
            eprintln!(
                "clawket: warning: failed to auto-start clawketd ({program}): {e}\n\
                 Hint: run `clawket daemon start` manually or set CLAWKET_DAEMON_BIN."
            );
            // Don't return Err — the caller will hit a connection error which
            // is more informative than a spawn error.
            return Ok(());
        }
    }

    wait_for_socket("auto-starting daemon")
}

fn wait_for_socket(reason: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(SPAWN_TIMEOUT_SECS);
    while Instant::now() < deadline {
        if socket_reachable() {
            return Ok(());
        }
        // If pid file exists and the process is alive, keep waiting.
        if let Some(pid) = read_pid()
            && is_running(pid)
        {
            std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
            continue;
        }
        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }

    if socket_reachable() {
        return Ok(());
    }

    // Timeout — don't hard-fail. The daemon might still be loading the
    // embedding model on first run (which can take >8 s). The caller will get
    // a daemon-connection error and can retry with `clawket daemon status`.
    eprintln!(
        "clawket: warning: {reason}: socket not reachable after {SPAWN_TIMEOUT_SECS}s.\n\
         Run `clawket daemon status` to diagnose."
    );
    Ok(())
}

// ===== flock wrapper =====

use std::os::unix::io::AsRawFd;

fn flock_exclusive_nb(fd: std::os::unix::io::RawFd) -> i32 {
    unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) }
}
