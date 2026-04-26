use anyhow::{Result, bail};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::DaemonAction;
use crate::paths;

/// Returns (program, extra_args) for running clawketd.
/// Search order:
///   1. CLAWKET_DAEMON_BIN env (explicit override)
///   2..N. paths::daemon_bin_candidates() — shared with `clawket doctor`
///   N+1. PATH "clawketd"
fn clawketd_cmd() -> (String, Vec<String>) {
    if let Ok(bin) = std::env::var("CLAWKET_DAEMON_BIN") {
        let parts: Vec<String> = bin.split_whitespace().map(String::from).collect();
        if !parts.is_empty() {
            return (parts[0].clone(), parts[1..].to_vec());
        }
    }

    for (candidate, _label) in paths::daemon_bin_candidates() {
        if candidate.exists() {
            let canonical = candidate.canonicalize().unwrap_or(candidate);
            return (canonical.to_string_lossy().into_owned(), vec![]);
        }
    }

    ("clawketd".to_string(), vec![])
}

fn read_pid() -> Option<u32> {
    fs::read_to_string(paths::pid_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn is_running(pid: u32) -> bool {
    // kill -0 으로 프로세스 존재 확인
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Resolve clawketd log file path. Mirrors daemon/src/paths.rs:
/// CLAWKET_STATE_DIR → $XDG_STATE_HOME/clawket → $HOME/.local/state/clawket.
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

pub async fn run(action: DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start => cmd_start(),
        DaemonAction::Stop => cmd_stop(),
        DaemonAction::Status => cmd_status(),
        DaemonAction::Restart => {
            cmd_stop()?;
            cmd_start()
        }
    }
}

/// Run clawketd with `subcmd` to completion and collect output.
/// Only used for stop/status — NOT for start (which would block).
fn run_clawketd(subcmd: &str) -> Result<std::process::Output> {
    let (program, extra_args) = clawketd_cmd();
    let output = Command::new(&program)
        .args(&extra_args)
        .arg(subcmd)
        .output();
    match output {
        Ok(out) => Ok(out),
        Err(e) => bail!(
            "failed to run '{program}': {e}\nMake sure clawketd is in your PATH or set CLAWKET_DAEMON_BIN"
        ),
    }
}

fn print_output(out: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }
}

/// Start clawketd in the background.
///
/// `clawketd start` runs the HTTP server in the foreground (see
/// daemon/src/main.rs::run_daemon). This wrapper must NOT wait on it, or the
/// SessionStart hook (which invokes `clawket daemon start` synchronously) would
/// block for the entire daemon lifetime.
///
/// Detach strategy:
///   - stdin:  /dev/null
///   - stdout+stderr: appended to $XDG_STATE_HOME/clawket/clawketd.log
///   - new process group (process_group(0)) so SIGHUP/SIGINT on the caller
///     doesn't propagate to the daemon
///   - spawn() only; never wait()
///
/// After spawn, poll the pid file + liveness for up to 5s so the caller
/// gets a synchronous "ready" signal, but fall back to "starting" rather
/// than timing out the hook.
fn cmd_start() -> Result<()> {
    if let Some(pid) = read_pid() {
        if is_running(pid) {
            println!("clawketd: already running (pid={pid})");
            return Ok(());
        }
    }

    let (program, extra_args) = clawketd_cmd();

    let (stdout_target, stderr_target, log_path) = match log_file_path() {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => {
                    let clone = match file.try_clone() {
                        Ok(c) => c,
                        Err(_) => {
                            // Should not happen in practice; fall back to null.
                            return spawn_null_detached(&program, &extra_args);
                        }
                    };
                    (Stdio::from(file), Stdio::from(clone), Some(path))
                }
                Err(_) => (Stdio::null(), Stdio::null(), None),
            }
        }
        None => (Stdio::null(), Stdio::null(), None),
    };

    let mut cmd = Command::new(&program);
    cmd.args(&extra_args)
        .arg("start")
        .stdin(Stdio::null())
        .stdout(stdout_target)
        .stderr(stderr_target);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group: SIGHUP/SIGINT delivered to the parent's group
        // (e.g. the hook's shell) doesn't reach the daemon.
        cmd.process_group(0);
    }

    let child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "failed to spawn '{program} start': {e}\nMake sure clawketd is in your PATH or set CLAWKET_DAEMON_BIN"
        )
    })?;
    let child_pid = child.id();
    // Intentionally drop `child` without wait() — clawketd runs in its own
    // process group with stdio redirected to the log file, so it becomes a
    // reparented background process when the CLI exits. No zombie: clawketd
    // becomes a child of init (pid 1) when this process exits.
    drop(child);

    // Poll for readiness: pid file appears AND process is alive.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(pid) = read_pid() {
            if is_running(pid) {
                if let Some(path) = &log_path {
                    println!("clawketd: started (pid={pid}, log={})", path.display());
                } else {
                    println!("clawketd: started (pid={pid})");
                }
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            // Don't fail — just report "starting". Status/health will confirm
            // later. Avoids false-positive hook timeouts when the daemon is
            // doing a first-time migration or embedding model load.
            let hint = match &log_path {
                Some(p) => format!(" (tail log: {})", p.display()),
                None => String::new(),
            };
            println!("clawketd: starting (spawned pid={child_pid}){hint}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Fallback when log file can't be opened: spawn with /dev/null stdio.
fn spawn_null_detached(program: &str, extra_args: &[String]) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(extra_args)
        .arg("start")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn '{program} start': {e}"))?;
    let pid = child.id();
    drop(child);
    println!("clawketd: starting (spawned pid={pid}, log=/dev/null)");
    Ok(())
}

/// Stop clawketd, escalating to SIGKILL if graceful SIGTERM times out.
///
/// The daemon's own `stop` subcommand sends SIGTERM then polls for up to 10s.
/// When the daemon is wedged (e.g. a shutdown-race bug in an older build where
/// the signal handler fires before the server's graceful_shutdown future has
/// registered its waiter), it never exits. In that case the child `clawketd
/// stop` returns with a non-zero exit and a "did not exit within 10s" error.
///
/// Silent-failure symptom: callers of `clawket daemon restart` would then see
/// `cmd_start` report "already running" on the old pid, with no indication the
/// stop actually failed. Fix: detect non-zero exit, SIGKILL the pid, and clean
/// the stale pid/port files so `cmd_start` can spawn a fresh daemon.
fn cmd_stop() -> Result<()> {
    let out = run_clawketd("stop")?;
    print_output(&out);
    if out.status.success() {
        return Ok(());
    }

    let Some(pid) = read_pid() else {
        // No pid file left — nothing to escalate against.
        return Ok(());
    };
    if !is_running(pid) {
        let _ = fs::remove_file(paths::pid_path());
        let _ = fs::remove_file(paths::port_path());
        return Ok(());
    }

    #[cfg(unix)]
    {
        eprintln!("clawketd: graceful SIGTERM timed out; escalating to SIGKILL on pid {pid}");
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        // Poll briefly; kill -9 should take effect immediately but the kernel
        // still needs a tick to reap.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !is_running(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if is_running(pid) {
            bail!("clawketd pid {pid} still alive after SIGKILL");
        }
        let _ = fs::remove_file(paths::pid_path());
        let _ = fs::remove_file(paths::port_path());
        eprintln!("clawketd: pid {pid} force-killed");
    }
    #[cfg(not(unix))]
    {
        bail!("cannot escalate stop on non-unix platform");
    }
    Ok(())
}

fn cmd_status() -> Result<()> {
    let out = run_clawketd("status")?;
    print_output(&out);
    if !out.status.success() {
        std::process::exit(1);
    }
    Ok(())
}
