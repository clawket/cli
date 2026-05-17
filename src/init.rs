use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use crate::client;

pub async fn run(tutorial: bool, cwd: Option<String>) -> Result<()> {
    if !tutorial {
        anyhow::bail!(
            "clawket init currently only supports --tutorial. Pass --tutorial to scaffold a 5-min onboarding project."
        );
    }

    let cwd_input = cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    });
    if !std::path::Path::new(&cwd_input).exists() {
        std::fs::create_dir_all(&cwd_input)
            .with_context(|| format!("failed to create --cwd path: {cwd_input}"))?;
    }
    let cwd = std::fs::canonicalize(&cwd_input)
        .map(|p| p.to_string_lossy().into_owned())
        .with_context(|| format!("--cwd path not found or unreadable: {cwd_input}"))?;

    println!("clawket init --tutorial");
    println!("  cwd: {cwd}\n");

    let c = client::make_client();

    let project = ensure_project(&c, &cwd).await?;
    let project_id = project["id"]
        .as_str()
        .context("daemon response missing project id")?
        .to_string();
    let project_name = project["name"].as_str().unwrap_or("Hello Clawket");
    println!("[1/6] project    {project_id}  ({project_name})");

    // Short-circuit if the project already has a plan — re-running init must
    // not pile on duplicate plan/unit/cycle/task. Print the existing scaffold
    // so the user can pick up where they were.
    let existing_plans = client::get(&c, &format!("/plans?project_id={project_id}")).await?;
    if let Some(arr) = existing_plans.as_array()
        && !arr.is_empty()
    {
        println!("\nTutorial already scaffolded for this cwd. Skipping create.\n");
        println!("Existing plans:");
        for p in arr {
            let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let title = p.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("");
            println!("  {id}  [{status}]  {title}");
        }
        println!("\nSee state:");
        println!("  clawket dashboard --cwd {cwd}");
        return Ok(());
    }

    let plan = client::request(
        &c,
        "POST",
        "/plans",
        Some(json!({
            "project_id": project_id,
            "title": "Hello Clawket",
            "description": "First plan created by `clawket init --tutorial`.",
        })),
    )
    .await?;
    let plan_id = plan["id"]
        .as_str()
        .context("daemon response missing plan id")?
        .to_string();
    client::request(&c, "POST", &format!("/plans/{plan_id}/approve"), None).await?;
    println!("[2/6] plan       {plan_id}  (approved)");

    let unit = client::request(
        &c,
        "POST",
        "/units",
        Some(json!({
            "plan_id": plan_id,
            "title": "Onboarding",
            "goal": "Walk the Clawket lifecycle once.",
        })),
    )
    .await?;
    let unit_id = unit["id"]
        .as_str()
        .context("daemon response missing unit id")?
        .to_string();
    println!("[3/6] unit       {unit_id}");

    let cycle = client::request(
        &c,
        "POST",
        "/cycles",
        Some(json!({
            "project_id": project_id,
            "title": "Sprint 0",
            "goal": "Close the first task end-to-end.",
        })),
    )
    .await?;
    let cycle_id = cycle["id"]
        .as_str()
        .context("daemon response missing cycle id")?
        .to_string();
    client::request(&c, "POST", &format!("/cycles/{cycle_id}/activate"), None).await?;
    println!("[4/6] cycle      {cycle_id}  (active)");

    let task = client::request(
        &c,
        "POST",
        "/tasks",
        Some(json!({
            "unit_id": unit_id,
            "cycle_id": cycle_id,
            "title": "Read the tutorial",
            "body": "Open https://clawket.dev/tutorial/ and walk the structured agent loop.",
            "priority": "high",
            "type": "docs",
            "cwd": cwd,
        })),
    )
    .await?;
    let task_id = task["id"]
        .as_str()
        .context("daemon response missing task id")?
        .to_string();
    let ticket = task["ticket_number"].as_str().unwrap_or("");
    println!("[5/6] task       {task_id}  ({ticket})");

    client::request(
        &c,
        "PATCH",
        &format!("/tasks/{task_id}"),
        Some(json!({"status": "in_progress"})),
    )
    .await?;
    println!("[6/6] started    {task_id} → in_progress\n");

    println!("Done. Tutorial scaffold ready.\n");
    println!("Next:");
    println!("  clawket dashboard --cwd {cwd}");
    println!("  clawket task update {task_id} --status done   # close it");
    println!("  open https://clawket.dev/tutorial/             # full walkthrough");

    Ok(())
}

async fn ensure_project(c: &client::HttpClient, cwd: &str) -> Result<Value> {
    let path = format!("/projects/by-cwd{cwd}");
    let (status, val) = client::request_raw(c, "GET", &path, None).await?;
    if status.is_success() {
        return Ok(val);
    }
    if status.as_u16() != 404 {
        anyhow::bail!(
            "project resolve failed (status {}): {}",
            status,
            val.get("error").and_then(|e| e.as_str()).unwrap_or("?")
        );
    }
    client::request(
        c,
        "POST",
        "/projects",
        Some(json!({
            "name": "Hello Clawket",
            "description": "Tutorial project created by `clawket init --tutorial`.",
            "cwd": cwd,
        })),
    )
    .await
}
