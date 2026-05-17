//! LM-147 / RL-U5-02a — empirical evidence harness for ADR-0002.
//!
//! Scenario: a SessionStart hook returns a payload with both
//!   `hookSpecificOutput.additionalContext` and `systemMessage`. We spawn
//!   `claude -p` headless against the fixture and:
//!     1. confirm the hook actually ran (proof: stream-json includes a hook
//!        event whose output contains the sentinel)
//!     2. probe whether the additionalContext landed inside the model's
//!        session context (proof: the assistant's reply quotes the sentinel
//!        when explicitly asked)
//!
//! Both tests are `#[ignore]` by default — running them spawns the real
//! `claude` CLI, which requires Anthropic credentials and incurs a
//! micro-cost. Use `cargo test --test headless_hook -- --ignored` from a
//! shell that already has credentials. Output is appended to
//! `cli/docs/adr/0002-hook-injection-strategy-evidence.md` by hand after
//! review (the test does not write the ADR — that's a human sign-off step
//! per `interaction_model: agent+human-approval`).

use std::path::PathBuf;
use std::process::Command;

const SENTINEL: &str = "CLAWKET-LM147-9D2F1E7B";

fn fixture_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest)
        .join("tests")
        .join("fixtures")
        .join("headless-hook")
}

fn settings_path() -> PathBuf {
    fixture_dir().join(".claude").join("settings.json")
}

fn claude_available() -> bool {
    Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn fixture_is_well_formed() {
    // Lightweight sanity check that runs in CI without spawning claude.
    let hook = fixture_dir()
        .join(".claude")
        .join("hooks")
        .join("session-start.cjs");
    assert!(hook.exists(), "missing session-start.cjs fixture: {:?}", hook);
    let body = std::fs::read_to_string(&hook).unwrap();
    assert!(
        body.contains("hookSpecificOutput"),
        "fixture must emit hookSpecificOutput envelope"
    );
    assert!(
        body.contains("additionalContext"),
        "fixture must emit additionalContext"
    );

    let settings = std::fs::read_to_string(settings_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&settings).unwrap();
    let registered = parsed["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap_or("");
    assert!(
        registered.contains("session-start.cjs"),
        "settings.json must register the hook by name, got: {registered}"
    );
}

#[test]
#[ignore = "spawns real `claude` CLI; requires credentials and incurs micro-cost"]
fn hook_runs_and_emits_sentinel() {
    if !claude_available() {
        eprintln!("skip: `claude` not on PATH");
        return;
    }
    let dir = fixture_dir();
    let settings = settings_path();
    let out = Command::new("claude")
        .current_dir(&dir)
        .arg("-p")
        .arg("Reply with exactly the word OK and nothing else.")
        .arg("--model")
        .arg("claude-haiku-4-5-20251001")
        .arg("--add-dir")
        .arg(&dir)
        .arg("--settings")
        .arg(&settings)
        .arg("--include-hook-events")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--max-budget-usd")
        .arg("0.10")
        .arg("--no-session-persistence")
        .env("CLAWKET_HOOK_SENTINEL", SENTINEL)
        .output()
        .expect("failed to spawn claude");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Check sentinel FIRST: even if claude later exits non-zero (e.g. budget),
    // a SessionStart hook event in stream-json with the sentinel is the proof
    // we want — the hook ran and the additionalContext envelope was captured.
    assert!(
        stdout.contains(SENTINEL),
        "hook event with sentinel not found in stream-json. \
         status={:?}\nstdout snippet:\n{}\n---\nstderr:\n{}",
        out.status,
        stdout.chars().take(2000).collect::<String>(),
        stderr.chars().take(500).collect::<String>(),
    );
}

#[test]
#[ignore = "spawns real `claude` CLI; requires credentials and incurs micro-cost"]
fn additional_context_lands_in_model_visible_context() {
    if !claude_available() {
        eprintln!("skip: `claude` not on PATH");
        return;
    }
    let dir = fixture_dir();
    let settings = settings_path();
    let probe = format!(
        "Search your full context (system, additional context, hook output, \
         everything) for any line that begins with '[hook-additional-context]'. \
         Reply with the exact 8-character token immediately after \
         'CLAWKET-LM147-' and NOTHING else. If you cannot find such a line, \
         reply with the single word NOT_FOUND."
    );
    let out = Command::new("claude")
        .current_dir(&dir)
        .arg("-p")
        .arg(probe)
        .arg("--model")
        .arg("claude-haiku-4-5-20251001")
        .arg("--add-dir")
        .arg(&dir)
        .arg("--settings")
        .arg(&settings)
        .arg("--output-format")
        .arg("text")
        .arg("--max-budget-usd")
        .arg("0.10")
        .arg("--no-session-persistence")
        .env("CLAWKET_HOOK_SENTINEL", SENTINEL)
        .output()
        .expect("failed to spawn claude");

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("model reply: {stdout}\n---stderr---\n{stderr}");
    let token = SENTINEL.trim_start_matches("CLAWKET-LM147-");
    // Probe purpose: did additionalContext land in the model's session context?
    // A model echo of the 8-char token is positive proof. Budget exits are OK
    // as long as the model already produced its single-token reply.
    assert!(
        stdout.contains(token),
        "model did not echo the sentinel suffix `{token}` — \
         additionalContext likely did NOT land in model-visible context. \
         status={:?} reply={stdout}",
        out.status,
    );
}
