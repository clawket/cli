//! LM-264 / L1.2.d — plan export ↔ strict import round-trip.
//!
//! Success criteria: `clawket plan export <id> --format md` followed by
//! `clawket plan import --strict --file <path>` followed by another
//! `plan export` produces byte-identical markdown (modulo regenerated
//! IDs and project-prefixed ticket numbers).
//!
//! This is the end-to-end validator for the strict round-trip contract:
//!
//!   * The export renderer (`commands::plan::export::render_markdown`)
//!     emits exactly what the strict parser (`parse_plan_strict`) accepts.
//!   * The strict importer (`POST /plans/import/strict` →
//!     `import_parsed_plan`) persists envelopes + `depends_on` edges so
//!     re-export reproduces the full envelope bullet block and the
//!     mermaid `Dependency Graph`.
//!   * Pass 2/3 of `import_parsed_plan` (depends_on resolution + envelope
//!     signing) actually fire — without them the second export would
//!     drop the graph block and the envelope bullets.
//!
//! Test isolation:
//!
//!   * Each round writes into a separate project so `ticket_number`
//!     counters reset, keeping the comparison reproducible. The
//!     `ticket_number` and ULID columns are still regenerated, so the
//!     comparison normalizes them out (see `normalize_for_compare`).
//!
//! Subprocess strategy: the export logic lives in the `clawket` binary
//! (single-bin crate, no library). The test invokes `clawket plan
//! import --strict` and `clawket plan export` against the test daemon
//! by overriding `CLAWKET_CACHE_DIR` so the CLI's Unix-socket client
//! lands on the test daemon's socket.

use std::path::{Path, PathBuf};
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
            if let Ok(s) = std::fs::read_to_string(&port_file)
                && let Ok(p) = s.trim().parse::<u16>()
            {
                port = Some(p);
                break;
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
        // Also wait for the Unix socket — `clawket` CLI dispatches over
        // the socket, not the TCP port. The daemon writes both during
        // startup but the socket can lag the port by a few ms.
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

/// Run a `clawket` subcommand against the test daemon. Returns stdout on
/// success; panics with stderr on non-zero exit so test failures point
/// at the exact CLI error.
///
/// `cwd` lets each `plan import` round run from a distinct directory.
/// v3.0's `CWD_ALREADY_REGISTERED` enforcement means two consecutive
/// `plan import --project NAME_A/NAME_B` calls from the same cwd collide,
/// so the test seeds a per-round subdir under the daemon's tempdir.
fn run_cli(daemon: &Daemon, cwd: &Path, args: &[&str]) -> String {
    let bin = env!("CARGO_BIN_EXE_clawket");
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("CLAWKET_CACHE_DIR", &daemon.cache_dir)
        .output()
        .expect("spawn clawket");
    if !out.status.success() {
        panic!(
            "clawket {args:?} failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).expect("non-utf8 stdout")
}

/// Seed a project + plan + 1 unit + 2 tasks where task B depends on
/// task A. Both tasks carry full ADR-0001 required-tier envelopes. The
/// project key (`SEED`) is what determines the ticket prefix on export;
/// downstream rounds use different keys so ticket counters stay
/// independent.
async fn seed_plan(
    c: &reqwest::Client,
    base: &str,
    project_name: &str,
    project_key: &str,
) -> String {
    use serde_json::json;

    let project: serde_json::Value = c
        .post(format!("{base}/projects"))
        .json(&json!({"name": project_name, "key": project_key}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = project["id"].as_str().unwrap().to_string();

    let plan: serde_json::Value = c
        .post(format!("{base}/plans"))
        .json(&json!({
            "project_id": pid,
            "title": "Round-trip fixture",
        }))
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
        .json(&json!({"project_id": pid, "unit_id": unit_id, "title": "Cycle 1"}))
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

    let task_a: serde_json::Value = c
        .post(format!("{base}/tasks"))
        .json(&json!({
            "unit_id": unit_id,
            "cycle_id": cycle_id,
            "title": "Schema migration",
            "envelope": {
                "version": 1,
                "intent": "Add task_envelopes sidecar table",
                "prompt_template": "Add a sidecar table for task envelopes per ADR.",
                "target_repo": "daemon",
                "context_refs": [{"kind": "decision", "id": "DEC-1"}],
                "success_criteria": ["migration up/down passes"],
                "verification_cmd": "cargo test",
                "decomposition_policy": "atomic"
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // POST /tasks returns `TaskWithEnvelope`, which `#[serde(flatten)]`s
    // the task fields onto the top-level object — there's no nested
    // `task` wrapper.
    let task_a_id = task_a["id"]
        .as_str()
        .unwrap_or_else(|| panic!("task A response missing id: {task_a}"))
        .to_string();

    let _task_b: serde_json::Value = c
        .post(format!("{base}/tasks"))
        .json(&json!({
            "unit_id": unit_id,
            "cycle_id": cycle_id,
            "title": "Persist envelope",
            "depends_on": [task_a_id],
            "envelope": {
                "version": 1,
                "intent": "Wire sign_for_task into the importer",
                "prompt_template": "Wire sign_for_task through the importer per ADR.",
                "target_repo": "daemon",
                "context_refs": [{"kind": "decision", "id": "DEC-1"}],
                "success_criteria": ["round-trip parity"],
                "verification_cmd": "cargo test plan_export_roundtrip",
                "decomposition_policy": "atomic"
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    plan_id
}

/// Strip identifiers that the daemon regenerates on every import so the
/// markdown / JSON comparison only catches real shape drift. Targets:
///
///   * `PLAN-…`, `PROJ-…`, `UNIT-…`, `TASK-…`, `ENV-…`, `CYC-…` ULIDs
///     (all 26-char Crockford base32 — ULIDs are stable in length but
///     content is per-row, so we collapse to a sentinel)
///   * Project-prefixed tickets like `SEED-1`, `RT1-2`, `RT2-2` (any
///     uppercase prefix + dash + integer; the prefix differs per round
///     because each round seeds a fresh project)
///   * `created_at`/`updated_at` epoch millis (rendered into the meta
///     `source` line on imported plans)
///   * ISO-8601 timestamps (`2026-05-09T10:27:23.427Z`) emitted by the
///     JSON exporter — wall-clock differs between rounds.
///
/// The leftover string captures section ordering, headings, body text,
/// envelope bullets in canonical order, and the mermaid graph
/// structure — which is what we actually want to assert is preserved.
fn normalize_for_compare(md: &str) -> String {
    use regex::Regex;
    // PROJ-* IDs are slugified from the project name (`PROJ-rt-round-a`),
    // not ULIDs, so the body is `[a-z0-9-]+` rather than a fixed-length
    // base32 string. Other entity IDs (PLAN/UNIT/TASK/ENV/CYC) are 26-char
    // ULIDs but we use the same loose body pattern for uniformity.
    let ulid_re =
        Regex::new(r"\b(PLAN|PROJ|UNIT|TASK|ENV|CYC)-[0-9A-Za-z][0-9A-Za-z-]{0,63}\b").unwrap();
    let ticket_re = Regex::new(r"\b[A-Z][A-Z0-9]{0,7}-[0-9]+\b").unwrap();
    let epoch_re = Regex::new(r"\b1[0-9]{12}\b").unwrap();
    let iso_re = Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z").unwrap();
    let pass1 = ulid_re.replace_all(md, "<ULID>");
    // Ticket regex would also match `<ULID>` substrings if applied
    // first, so order matters. After ULID redaction, remaining
    // letter-dash-number tokens are tickets or stray references —
    // redact uniformly.
    let pass2 = ticket_re.replace_all(&pass1, "<TICKET>");
    let pass3 = epoch_re.replace_all(&pass2, "<EPOCH>");
    let pass4 = iso_re.replace_all(&pass3, "<TS>");
    pass4.into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_export_roundtrip_md_is_byte_stable() {
    let bin = match locate_clawketd() {
        Some(p) => p,
        None => {
            eprintln!(
                "skip: clawketd not found. Set CLAWKETD_BIN, or build it \
                 via `cargo build --manifest-path ../daemon/Cargo.toml`"
            );
            return;
        }
    };

    let d = Daemon::spawn(&bin).await;
    let c = reqwest::Client::new();

    // Round 1: HTTP-seed plan, export to capture canonical markdown.
    let plan_seed = seed_plan(&c, &d.base, "rt-seed", "RTS").await;
    let cwd_export = d._tmp.path().join("cwd-export");
    std::fs::create_dir_all(&cwd_export).unwrap();
    let md_seed = run_cli(
        &d,
        &cwd_export,
        &["plan", "export", &plan_seed, "--format", "md"],
    );
    assert!(
        md_seed.contains("**Envelope:**"),
        "seed export must include envelope bullets:\n{md_seed}"
    );
    assert!(
        md_seed.contains("```mermaid"),
        "seed export must include dependency graph fence:\n{md_seed}"
    );

    // Round 2: import the canonical markdown into a fresh project so
    // ticket counters restart, then re-export. Each round runs from its
    // own cwd because `plan import --project` registers the cwd → project
    // edge, and v3.0 rejects re-registering the same cwd to a different
    // project.
    let tmp_dir = tempfile::tempdir().expect("tmp for plan files");
    let seed_path = tmp_dir.path().join("seed.md");
    std::fs::write(&seed_path, &md_seed).unwrap();

    let cwd_a = d._tmp.path().join("cwd-round-a");
    std::fs::create_dir_all(&cwd_a).unwrap();
    let import_a_out = run_cli(
        &d,
        &cwd_a,
        &[
            "plan",
            "import",
            "--strict",
            "--project",
            "rt-round-a",
            seed_path.to_str().unwrap(),
        ],
    );
    let import_a: serde_json::Value =
        serde_json::from_str(import_a_out.trim()).expect("import-A response is JSON");
    let plan_a_id = import_a["plan"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("import-A missing plan.id: {import_a}"))
        .to_string();
    let md_a = run_cli(
        &d,
        &cwd_export,
        &["plan", "export", &plan_a_id, "--format", "md"],
    );

    // Round 3: import the round-2 export again into another fresh
    // project. Round-trip parity means MD_A == MD_B modulo regenerated
    // IDs/tickets.
    let md_a_path = tmp_dir.path().join("round-a.md");
    std::fs::write(&md_a_path, &md_a).unwrap();
    let cwd_b = d._tmp.path().join("cwd-round-b");
    std::fs::create_dir_all(&cwd_b).unwrap();
    let import_b_out = run_cli(
        &d,
        &cwd_b,
        &[
            "plan",
            "import",
            "--strict",
            "--project",
            "rt-round-b",
            md_a_path.to_str().unwrap(),
        ],
    );
    let import_b: serde_json::Value =
        serde_json::from_str(import_b_out.trim()).expect("import-B response is JSON");
    let plan_b_id = import_b["plan"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("import-B missing plan.id: {import_b}"))
        .to_string();
    let md_b = run_cli(
        &d,
        &cwd_export,
        &["plan", "export", &plan_b_id, "--format", "md"],
    );

    // Sanity: both exports carry the envelope + graph (i.e. neither
    // round dropped the strict-only bullets).
    assert!(
        md_a.contains("**Envelope:**") && md_a.contains("```mermaid"),
        "round-A export missing strict shape:\n{md_a}"
    );
    assert!(
        md_b.contains("**Envelope:**") && md_b.contains("```mermaid"),
        "round-B export missing strict shape:\n{md_b}"
    );

    let na = normalize_for_compare(&md_a);
    let nb = normalize_for_compare(&md_b);
    if na != nb {
        // Surface a unified-diff-ish hint so failures are debuggable
        // without rebuilding the test fixture by hand.
        let mut diff = String::new();
        for (i, (la, lb)) in na.lines().zip(nb.lines()).enumerate() {
            if la != lb {
                diff.push_str(&format!("L{}: A={la:?} B={lb:?}\n", i + 1));
            }
        }
        let len_a = na.lines().count();
        let len_b = nb.lines().count();
        if len_a != len_b {
            diff.push_str(&format!("line counts differ: A={len_a} B={len_b}\n"));
        }
        panic!(
            "round-trip markdown drift after normalization:\n{diff}\n\
             ===== A =====\n{na}\n===== B =====\n{nb}"
        );
    }
}

/// JSON-format counterpart to `plan_export_roundtrip_md_is_byte_stable`.
///
/// The strict importer is markdown-only — there's no `import --strict
/// --format json` path — so the round-trip still goes through markdown
/// at the import side. What this test pins down is the *export* side:
/// `clawket plan export --format json` against an imported plan must
/// produce byte-identical JSON across rounds (modulo regenerated
/// IDs/tickets/epochs). This is the canonical machine-readable shape
/// (`{plan, units: [{unit, tasks: [{task, envelope}]}], depends_on,
/// knowledge}`) that downstream tooling consumes; drift here would
/// silently break consumers even when the markdown happens to match.
///
/// The two tests run with separate daemons in parallel under
/// `cargo test`, so failure isolation is preserved without doubling
/// wall-clock cost in practice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_export_roundtrip_json_is_byte_stable() {
    let bin = match locate_clawketd() {
        Some(p) => p,
        None => {
            eprintln!(
                "skip: clawketd not found. Set CLAWKETD_BIN, or build it \
                 via `cargo build --manifest-path ../daemon/Cargo.toml`"
            );
            return;
        }
    };

    let d = Daemon::spawn(&bin).await;
    let c = reqwest::Client::new();

    let plan_seed = seed_plan(&c, &d.base, "rt-json-seed", "RTJ").await;
    // Round-trip through markdown (strict importer is md-only); json
    // export is what we're asserting parity on.
    let cwd_export = d._tmp.path().join("cwd-export");
    std::fs::create_dir_all(&cwd_export).unwrap();
    let md_seed = run_cli(
        &d,
        &cwd_export,
        &["plan", "export", &plan_seed, "--format", "md"],
    );

    let tmp_dir = tempfile::tempdir().expect("tmp for plan files");
    let seed_path = tmp_dir.path().join("seed.md");
    std::fs::write(&seed_path, &md_seed).unwrap();

    let cwd_a = d._tmp.path().join("cwd-round-a");
    std::fs::create_dir_all(&cwd_a).unwrap();
    let import_a_out = run_cli(
        &d,
        &cwd_a,
        &[
            "plan",
            "import",
            "--strict",
            "--project",
            "rt-json-round-a",
            seed_path.to_str().unwrap(),
        ],
    );
    let import_a: serde_json::Value =
        serde_json::from_str(import_a_out.trim()).expect("import-A response is JSON");
    let plan_a_id = import_a["plan"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("import-A missing plan.id: {import_a}"))
        .to_string();
    let md_a = run_cli(
        &d,
        &cwd_export,
        &["plan", "export", &plan_a_id, "--format", "md"],
    );
    let json_a = run_cli(
        &d,
        &cwd_export,
        &["plan", "export", &plan_a_id, "--format", "json"],
    );

    let md_a_path = tmp_dir.path().join("round-a.md");
    std::fs::write(&md_a_path, &md_a).unwrap();
    let cwd_b = d._tmp.path().join("cwd-round-b");
    std::fs::create_dir_all(&cwd_b).unwrap();
    let import_b_out = run_cli(
        &d,
        &cwd_b,
        &[
            "plan",
            "import",
            "--strict",
            "--project",
            "rt-json-round-b",
            md_a_path.to_str().unwrap(),
        ],
    );
    let import_b: serde_json::Value =
        serde_json::from_str(import_b_out.trim()).expect("import-B response is JSON");
    let plan_b_id = import_b["plan"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("import-B missing plan.id: {import_b}"))
        .to_string();
    let json_b = run_cli(
        &d,
        &cwd_export,
        &["plan", "export", &plan_b_id, "--format", "json"],
    );

    // Sanity: the JSON shape carries what strict-format requires (plan
    // header, units array, depends_on edges, envelope on each task).
    let parsed_a: serde_json::Value =
        serde_json::from_str(&json_a).expect("round-A export is valid JSON");
    assert!(
        parsed_a.get("plan").is_some()
            && parsed_a.get("units").and_then(|v| v.as_array()).is_some()
            && parsed_a
                .get("depends_on")
                .and_then(|v| v.as_array())
                .is_some(),
        "round-A JSON missing canonical top-level keys:\n{json_a}"
    );
    let edges_a = parsed_a["depends_on"].as_array().unwrap();
    assert!(
        !edges_a.is_empty(),
        "round-A depends_on must be non-empty (seed has task B → task A): {json_a}"
    );
    let units_a = parsed_a["units"].as_array().unwrap();
    let any_envelope = units_a
        .iter()
        .flat_map(|u| u["tasks"].as_array().cloned().unwrap_or_default())
        .any(|t| !t["envelope"].is_null());
    assert!(
        any_envelope,
        "round-A export must persist at least one envelope: {json_a}"
    );

    let na = normalize_for_compare(&json_a);
    let nb = normalize_for_compare(&json_b);
    if na != nb {
        let mut diff = String::new();
        for (i, (la, lb)) in na.lines().zip(nb.lines()).enumerate() {
            if la != lb {
                diff.push_str(&format!("L{}: A={la:?} B={lb:?}\n", i + 1));
            }
        }
        let len_a = na.lines().count();
        let len_b = nb.lines().count();
        if len_a != len_b {
            diff.push_str(&format!("line counts differ: A={len_a} B={len_b}\n"));
        }
        panic!(
            "round-trip JSON drift after normalization:\n{diff}\n\
             ===== A =====\n{na}\n===== B =====\n{nb}"
        );
    }
}
