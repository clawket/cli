use anyhow::{Result, bail};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::DaemonAction;
use crate::paths;

/// Returns (program, extra_args) for running clawketd.
/// Search order:
///   1. CLAWKET_DAEMON_BIN env (explicit override)
///   2. <cli-exe>/../daemon/bin/clawketd (plugin layout: pluginRoot/bin/clawket + pluginRoot/daemon/bin/clawketd)
///   3. <cli-exe>/clawketd (sibling layout: both binaries in same dir, e.g. ~/.cargo/bin/)
///   4. $XDG_DATA_HOME/clawket/bin/clawketd (user install)
///   5. PATH "clawketd"
fn clawketd_cmd() -> (String, Vec<String>) {
    // 1. Explicit env var
    if let Ok(bin) = std::env::var("CLAWKET_DAEMON_BIN") {
        let parts: Vec<String> = bin.split_whitespace().map(String::from).collect();
        if !parts.is_empty() {
            return (parts[0].clone(), parts[1..].to_vec());
        }
    }

    for candidate in search_candidates() {
        if candidate.exists() {
            let canonical = candidate.canonicalize().unwrap_or(candidate);
            return (canonical.to_string_lossy().into_owned(), vec![]);
        }
    }

    // Final fallback: PATH
    ("clawketd".to_string(), vec![])
}

fn search_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            // 2. Plugin layout: pluginRoot/bin/clawket + pluginRoot/daemon/bin/clawketd
            out.push(
                bin_dir
                    .join("..")
                    .join("daemon")
                    .join("bin")
                    .join("clawketd"),
            );
            // 3. Sibling layout
            out.push(bin_dir.join("clawketd"));
        }
    }

    // 4. XDG_DATA_HOME/clawket/bin/clawketd
    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        });
    if let Some(base) = data_home {
        out.push(base.join("clawket").join("bin").join("clawketd"));
    }

    out
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

fn cmd_start() -> Result<()> {
    if let Some(pid) = read_pid() {
        if is_running(pid) {
            println!("clawketd: already running (pid={pid})");
            return Ok(());
        }
    }
    let out = run_clawketd("start")?;
    print_output(&out);
    if !out.status.success() {
        bail!("clawketd start failed (exit code: {:?})", out.status.code());
    }
    Ok(())
}

fn cmd_stop() -> Result<()> {
    let out = run_clawketd("stop")?;
    print_output(&out);
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
