// clawket verify — post-install smoke that exercises the daemon end-to-end.
//
// Where `doctor` is read-only diagnostics, `verify` actively writes: it creates
// a throwaway project, then deletes it, so the user gets a one-shot proof that
// the daemon's HTTP write path is wired up. Used as the success gate for
// `install.sh` (LM-101) — ran after the binaries land but before the user
// touches their real data.
//
// `--dry-run` walks the same step list and prints what each phase would do
// without contacting the daemon. Used in CI / docs / install scripts that want
// to verify the binary parses without spinning up state.

use anyhow::{Context as _, Result, bail};
use serde_json::json;
use std::time::SystemTime;

use crate::client;

const STEPS: &[&str] = &[
    "1. CLI version self-check (`clawket --version`)",
    "2. Daemon health probe (HTTP GET /health via Unix socket)",
    "3. Create throwaway project (POST /projects)",
    "4. Cleanup: delete throwaway project (DELETE /projects/:id, cascades plans/tasks)",
];

pub async fn run(dry_run: bool) -> Result<()> {
    println!("Clawket verify");
    println!("==============");
    println!();

    if dry_run {
        println!("Mode: DRY RUN (no daemon contact, no writes)");
        println!();
        println!("Would run:");
        for step in STEPS {
            println!("  {step}");
        }
        println!();
        println!("OK (dry-run)");
        return Ok(());
    }

    println!("Mode: LIVE (will create and delete a throwaway project)");
    println!();

    println!("[1/4] CLI version: {}", env!("CARGO_PKG_VERSION"));

    let c = client::make_client();
    let health = client::get(&c, "/health")
        .await
        .context("daemon /health probe failed — start it with `clawket daemon start`")?;
    let status = health
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("[2/4] Daemon /health: {status}");

    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("clawket-verify-{nonce}");
    let cwd = std::env::temp_dir()
        .join(&name)
        .to_string_lossy()
        .to_string();

    let project = client::request(
        &c,
        "POST",
        "/projects",
        Some(json!({ "name": &name, "cwd": &cwd })),
    )
    .await
    .context("failed to create throwaway project")?;
    let id = project
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon returned project without id"))?
        .to_string();
    println!("[3/4] Created project: {id}");

    let cleanup = client::request(&c, "DELETE", &format!("/projects/{id}"), None).await;
    match cleanup {
        Ok(_) => println!("[4/4] Deleted project: {id}"),
        Err(e) => bail!("cleanup failed (project {id} left behind): {e}"),
    }

    println!();
    println!("OK");
    Ok(())
}
