//! LM-265 / L1.3.c — `clawket task decompose` enforces `atomic_size_hint`
//! and `decomposition_policy=manual`.
//!
//! Pure-helper coverage lives in `commands::task::decompose::tests`
//! (search `decompose_atomic_*`). This file exercises the full CLI
//! against a real daemon so the wiring (read task field → call gate →
//! exit code / cap suggestions / force dry-run) is locked in.
//!
//! Setup uses POST /tasks with `atomic_size_hint` + `decomposition_policy`
//! body fields (LM-265 made these accepted directly, mirroring the
//! strict-import propagation). Building strict-format markdown
//! fixtures by hand here would be ~100 LOC of error-prone overhead.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn locate_clawketd() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CLAWKETD_BIN") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release"] {
        let candidate = manifest
            .parent()?
            .join("daemon")
            .join("target")
            .join(profile)
            .join("clawketd");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

struct Daemon {
    child: Child,
    base: String,
    cache_dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Daemon {
    async fn spawn(bin: &PathBuf) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let child = Command::new(bin)
            .arg("--port")
            .arg("0")
            .arg("--db")
            .arg(tmp.path().join("test.sqlite"))
            .env("CLAWKET_DATA_DIR", tmp.path().join("data"))
            .env("CLAWKET_CACHE_DIR", &cache_dir)
            .env("CLAWKET_CONFIG_DIR", tmp.path().join("config"))
            .env("CLAWKET_STATE_DIR", tmp.path().join("state"))
            .env("CLAWKETD_LOG", "warn")
            .env("CLAWKET_TCP_AUTH", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn clawketd");

        let port_file = cache_dir.join("clawketd.port");
        let mut port: Option<u16> = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Ok(s) = std::fs::read_to_string(&port_file) {
                if let Ok(p) = s.trim().parse::<u16>() {
                    port = Some(p);
                    break;
                }
            }
        }
        let port = port.expect("clawketd did not write its port file in time");
        let base = format!("http://127.0.0.1:{port}");

        let c = reqwest::Client::new();
        for _ in 0..30 {
            if c.get(format!("{base}/health")).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let sock = cache_dir.join("clawketd.sock");
        for _ in 0..50 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Self {
            child,
            base,
            cache_dir,
            _tmp: tmp,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run `clawket` against the test daemon. Returns (status_success,
/// stdout, stderr) — non-zero exits are *not* a panic here because the
/// atomic gate intentionally exits non-zero.
fn run_cli_capture(daemon: &Daemon, args: &[&str]) -> (bool, String, String) {
    let bin = env!("CARGO_BIN_EXE_clawket");
    let out = Command::new(bin)
        .args(args)
        .env("CLAWKET_CACHE_DIR", &daemon.cache_dir)
        .output()
        .expect("spawn clawket");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Seed a project + approved plan + unit + active cycle, returning
/// `(unit_id, cycle_id)`. v3.0 invariant: tasks require an active cycle.
async fn seed_unit(c: &reqwest::Client, base: &str, project_name: &str) -> (String, String) {
    use serde_json::json;

    let project: serde_json::Value = c
        .post(format!("{base}/projects"))
        .json(&json!({"name": project_name, "key": "DA"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = project["id"].as_str().unwrap().to_string();

    let plan: serde_json::Value = c
        .post(format!("{base}/plans"))
        .json(&json!({"project_id": pid, "title": "Decompose-atomic fixture"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let plan_id = plan["id"].as_str().unwrap().to_string();
    let _ = c
        .post(format!("{base}/plans/{plan_id}/approve"))
        .send()
        .await
        .unwrap();

    let unit: serde_json::Value = c
        .post(format!("{base}/units"))
        .json(&json!({"plan_id": plan_id, "title": "Foundations"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let unit_id = unit["id"].as_str().unwrap().to_string();

    let cycle: serde_json::Value = c
        .post(format!("{base}/cycles"))
        .json(&json!({
            "project_id": pid,
            "unit_id": unit_id,
            "title": "Cycle 1",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cycle_id = cycle["id"].as_str().unwrap().to_string();
    let _ = c
        .post(format!("{base}/cycles/{cycle_id}/activate"))
        .send()
        .await
        .unwrap();

    (unit_id, cycle_id)
}

/// Create a task with the given `atomic_size_hint` /
/// `decomposition_policy` and N success_criteria entries. Returns
/// the new task id.
async fn make_task(
    c: &reqwest::Client,
    base: &str,
    unit_id: &str,
    cycle_id: &str,
    title: &str,
    atomic_size_hint: &str,
    decomposition_policy: &str,
    n_criteria: usize,
) -> String {
    use serde_json::json;

    let criteria: Vec<String> = (1..=n_criteria).map(|i| format!("criterion {i}")).collect();
    let resp: serde_json::Value = c
        .post(format!("{base}/tasks"))
        .json(&json!({
            "unit_id": unit_id,
            "cycle_id": cycle_id,
            "title": title,
            "atomic_size_hint": atomic_size_hint,
            "decomposition_policy": decomposition_policy,
            "envelope": {
                "version": 1,
                "intent": format!("test fixture for {title}"),
                "target_repo": "cli",
                "context_refs": [],
                "success_criteria": criteria,
                "verification_cmd": "true",
                "decomposition_policy": decomposition_policy,
                "atomic_size_hint": atomic_size_hint,
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    resp["id"]
        .as_str()
        .unwrap_or_else(|| panic!("POST /tasks missing id: {resp}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decompose_atomic_small_refuses() {
    let bin = match locate_clawketd() {
        Some(p) => p,
        None => {
            eprintln!("skip: clawketd not found");
            return;
        }
    };
    let d = Daemon::spawn(&bin).await;
    let c = reqwest::Client::new();
    let (unit, cycle) = seed_unit(&c, &d.base, "decomp-small").await;
    let id = make_task(&c, &d.base, &unit, &cycle, "Atomic task", "small", "auto", 5).await;

    let (ok, stdout, stderr) =
        run_cli_capture(&d, &["task", "decompose", &id, "--max-depth", "2"]);
    assert!(!ok, "atomic gate must exit non-zero. stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("already_atomic"),
        "stdout must surface error code `already_atomic`:\n{stdout}"
    );
    assert!(
        stdout.contains("single session"),
        "stdout must include the single-session suggestion:\n{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decompose_atomic_medium_caps_suggestions_at_3() {
    let bin = match locate_clawketd() {
        Some(p) => p,
        None => {
            eprintln!("skip: clawketd not found");
            return;
        }
    };
    let d = Daemon::spawn(&bin).await;
    let c = reqwest::Client::new();
    let (unit, cycle) = seed_unit(&c, &d.base, "decomp-medium").await;
    // 7 criteria — well over the medium cap of 3.
    let id = make_task(&c, &d.base, &unit, &cycle, "Medium task", "medium", "auto", 7).await;

    let (ok, stdout, _stderr) =
        run_cli_capture(&d, &["task", "decompose", &id, "--max-depth", "2"]);
    assert!(ok, "medium decompose must succeed (preview only):\n{stdout}");
    let preview: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("preview must be JSON");
    let suggestions = preview["suggestions"]
        .as_array()
        .unwrap_or_else(|| panic!("preview missing suggestions array: {preview}"));
    assert_eq!(
        suggestions.len(),
        3,
        "medium hint must cap suggestions at 3, got {} from preview:\n{preview}",
        suggestions.len()
    );
    let violations = preview["violations"].as_array().unwrap();
    assert!(
        violations
            .iter()
            .any(|v| v["field"] == "atomic_size_hint" && v["severity"] == "warning"),
        "expected an `atomic_size_hint` truncation warning: {preview}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decompose_atomic_large_caps_suggestions_at_5() {
    let bin = match locate_clawketd() {
        Some(p) => p,
        None => {
            eprintln!("skip: clawketd not found");
            return;
        }
    };
    let d = Daemon::spawn(&bin).await;
    let c = reqwest::Client::new();
    let (unit, cycle) = seed_unit(&c, &d.base, "decomp-large").await;
    // 9 criteria — over the large cap of 5.
    let id = make_task(&c, &d.base, &unit, &cycle, "Large task", "large", "auto", 9).await;

    let (ok, stdout, _stderr) =
        run_cli_capture(&d, &["task", "decompose", &id, "--max-depth", "2"]);
    assert!(ok, "large decompose must succeed:\n{stdout}");
    let preview: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("preview must be JSON");
    let suggestions = preview["suggestions"].as_array().unwrap();
    assert_eq!(
        suggestions.len(),
        5,
        "large hint must cap suggestions at 5: {preview}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decompose_atomic_manual_policy_forces_dry_run() {
    let bin = match locate_clawketd() {
        Some(p) => p,
        None => {
            eprintln!("skip: clawketd not found");
            return;
        }
    };
    let d = Daemon::spawn(&bin).await;
    let c = reqwest::Client::new();
    let (unit, cycle) = seed_unit(&c, &d.base, "decomp-manual").await;
    // medium so the atomic gate doesn't refuse first; manual policy is
    // what we're testing.
    let id = make_task(&c, &d.base, &unit, &cycle, "Manual task", "medium", "manual", 3).await;

    // Even with --accept ALL, manual policy must downgrade to dry-run.
    let (ok, stdout, stderr) = run_cli_capture(
        &d,
        &["task", "decompose", &id, "--max-depth", "2", "--accept", "ALL"],
    );
    assert!(ok, "manual decompose dry-run must succeed: {stdout}\n{stderr}");
    assert!(
        stderr.contains("manual") && stderr.contains("ignored"),
        "stderr must explain the dry-run downgrade:\n{stderr}"
    );

    // Verify no children were created.
    let descendants: serde_json::Value = c
        .get(format!(
            "{}/tasks/{}/descendants?depth=1&include_envelope=false&order=bfs",
            d.base, id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = descendants.as_array().expect("descendants is an array");
    assert!(
        arr.is_empty(),
        "manual policy must not create children, found {}: {descendants}",
        arr.len()
    );

    // The preview should also include the manual-policy violation.
    let preview: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("preview must be JSON");
    let violations = preview["violations"].as_array().unwrap();
    assert!(
        violations
            .iter()
            .any(|v| v["field"] == "decomposition_policy"
                && v["message"].as_str().unwrap_or("").contains("manual")),
        "expected a `decomposition_policy=manual` violation: {preview}"
    );
}
