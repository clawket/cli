use anyhow::{Context as AnyhowCtx, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::paths;

// Unix socket connector for hyper 1.x
#[derive(Clone)]
pub struct UnixConnector {
    path: Arc<str>,
}

impl tower::Service<hyper::Uri> for UnixConnector {
    type Response = TokioIo<tokio::net::UnixStream>;
    type Error = std::io::Error;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _uri: hyper::Uri) -> Self::Future {
        let path = self.path.clone();
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(&*path).await?;
            Ok(TokioIo::new(stream))
        })
    }
}

pub type HttpClient = Client<UnixConnector, Full<Bytes>>;

pub fn make_client() -> HttpClient {
    let sock_path: Arc<str> = paths::socket_path().to_string_lossy().into_owned().into();
    let connector = UnixConnector { path: sock_path };
    Client::builder(TokioExecutor::new()).build(connector)
}

/// Render a daemon error response body into a human-facing message: the
/// top-level `error` string followed by every OTHER structured field
/// (e.g. `existing_plan_id` from a flat conflict, or a `details.violations`
/// array) as indented `key: value` lines.
///
/// #cli7: the daemon already ships these details on non-success responses, but
/// the three error paths below previously extracted only the `error` string and
/// dropped the rest — so `single_active_plan` lost the `existing_plan_id` that
/// tells the user which plan is active. Surfacing the remainder generically
/// means the user's next action is visible without a second `clawket … list`
/// round-trip, and any field the daemon adds later appears automatically with
/// no per-code handling. `code` and `stack` are skipped: `code` duplicates the
/// `CODE:` prefix already in the message, and `stack` is debug-only noise.
fn format_daemon_error(val: &serde_json::Value) -> String {
    let main = val
        .get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("unknown error")
        .to_string();
    let Some(obj) = val.as_object() else {
        return main;
    };
    let mut out = main;
    for (key, v) in obj {
        if matches!(key.as_str(), "error" | "code" | "stack") {
            continue;
        }
        let rendered = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.push_str(&format!("\n  {key}: {rendered}"));
    }
    out
}

/// Fetch a raw response body as bytes — used by callers that consume
/// non-JSON payloads such as the finite SSE stream from `/events/replay`.
/// Bails on connect failures and non-2xx responses (mirroring `get`).
pub async fn get_bytes(client: &HttpClient, path: &str) -> Result<Bytes> {
    let uri = format!("http://localhost{path}");
    let resp = client
        .get(uri.parse().context("invalid URI")?)
        .await
        .context("failed to connect to clawketd — is it running? (`clawket daemon start`)")?;
    let status = resp.status();
    let body = resp.into_body().collect().await?.to_bytes();
    if !status.is_success() {
        // Best-effort: try to surface a JSON error message if the body parses;
        // otherwise emit the status line so the user sees something useful.
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&body) {
            bail!("{}", format_daemon_error(&val));
        }
        bail!("HTTP {}", status);
    }
    Ok(body)
}

pub async fn get(client: &HttpClient, path: &str) -> Result<serde_json::Value> {
    let uri = format!("http://localhost{path}");
    let resp = client
        .get(uri.parse().context("invalid URI")?)
        .await
        .context("failed to connect to clawketd — is it running? (`clawket daemon start`)")?;
    let status = resp.status();
    let body = resp.into_body().collect().await?.to_bytes();
    let val: serde_json::Value = serde_json::from_slice(&body)?;
    if !status.is_success() {
        bail!("{}", format_daemon_error(&val));
    }
    Ok(val)
}

pub async fn request(
    client: &HttpClient,
    method: &str,
    path: &str,
    json_body: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let (status, val) = request_raw(client, method, path, json_body).await?;
    if !status.is_success() {
        bail!("{}", format_daemon_error(&val));
    }
    Ok(val)
}

/// Like `request` but returns the raw `(status, body)` pair so callers can
/// inspect structured `details` on non-success responses (e.g. lease 409
/// holder info). Connect-level failures still bail.
/// A single Server-Sent Event parsed from the daemon stream. `data` is the
/// joined `data:` lines (newline-separated per the SSE spec) — callers parse
/// it as JSON when the daemon emits a JSON payload.
pub struct SseEvent {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
}

/// Stream Server-Sent Events from `path` and invoke `on_event` for each event
/// as it arrives. Returns when the connection closes (daemon shutdown) or
/// `on_event` returns Err. Used by `clawket watch` to render the daemon's
/// infinite `/events` stream incrementally — `get` / `get_bytes` buffer the
/// whole body and so cannot be used for streams that never terminate.
pub async fn stream_sse<F>(client: &HttpClient, path: &str, mut on_event: F) -> Result<()>
where
    F: FnMut(&SseEvent) -> Result<()>,
{
    let uri: hyper::Uri = format!("http://localhost{path}")
        .parse()
        .context("invalid URI")?;
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("accept", "text/event-stream")
        .body(Full::new(Bytes::new()))
        .context("failed to build request")?;

    let resp = client
        .request(req)
        .await
        .context("failed to connect to clawketd — is it running? (`clawket daemon start`)")?;

    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {} from {}", status, path);
    }

    let mut body = resp.into_body();
    let mut pending: Vec<u8> = Vec::new();
    let mut cur_id: Option<String> = None;
    let mut cur_event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    while let Some(frame_result) = body.frame().await {
        let frame = frame_result.context("SSE stream error")?;
        let Ok(chunk) = frame.into_data() else {
            continue;
        };
        pending.extend_from_slice(&chunk);

        // Consume complete lines (LF-terminated, optional CR before LF).
        while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = pending.drain(..=nl).collect();
            let mut end = line_bytes.len() - 1;
            if end > 0 && line_bytes[end - 1] == b'\r' {
                end -= 1;
            }
            let line = std::str::from_utf8(&line_bytes[..end]).unwrap_or("");

            if line.is_empty() {
                // Blank line — dispatch the accumulated event.
                if cur_id.is_some() || cur_event.is_some() || !data_lines.is_empty() {
                    let ev = SseEvent {
                        id: cur_id.take(),
                        event: cur_event.take(),
                        data: data_lines.join("\n"),
                    };
                    data_lines.clear();
                    on_event(&ev)?;
                }
            } else if line.starts_with(':') {
                // SSE comment, ignore.
            } else if let Some(val) = line.strip_prefix("id:") {
                cur_id = Some(val.trim_start().to_string());
            } else if let Some(val) = line.strip_prefix("event:") {
                cur_event = Some(val.trim_start().to_string());
            } else if let Some(val) = line.strip_prefix("data:") {
                data_lines.push(val.trim_start().to_string());
            }
        }
    }
    Ok(())
}

pub async fn request_raw(
    client: &HttpClient,
    method: &str,
    path: &str,
    json_body: Option<serde_json::Value>,
) -> Result<(hyper::StatusCode, serde_json::Value)> {
    let uri: hyper::Uri = format!("http://localhost{path}")
        .parse()
        .context("invalid URI")?;
    let mut builder = Request::builder().method(method).uri(uri);

    let body = if let Some(json) = json_body {
        builder = builder.header("content-type", "application/json");
        Full::new(Bytes::from(serde_json::to_vec(&json)?))
    } else {
        Full::new(Bytes::new())
    };

    let req = builder.body(body).context("failed to build request")?;
    let resp = client
        .request(req)
        .await
        .context("failed to connect to clawketd — is it running? (`clawket daemon start`)")?;
    let status = resp.status();
    let body_bytes = resp.into_body().collect().await?.to_bytes();

    if body_bytes.is_empty() {
        return Ok((status, serde_json::json!({})));
    }
    let val: serde_json::Value = serde_json::from_slice(&body_bytes)?;
    Ok((status, val))
}

#[cfg(test)]
mod tests {
    use super::format_daemon_error;
    use serde_json::json;

    #[test]
    fn flat_conflict_detail_is_surfaced() {
        // #cli7: single_active_plan ships existing_plan_id flat on the body;
        // the old extractor dropped it, forcing a separate `plan list`.
        let body = json!({
            "error": "single_active_plan",
            "existing_plan_id": "PLAN-01KSF8FKKME2K892PCJCF1AS7W"
        });
        let out = format_daemon_error(&body);
        assert!(out.starts_with("single_active_plan"), "got: {out}");
        assert!(
            out.contains("existing_plan_id: PLAN-01KSF8FKKME2K892PCJCF1AS7W"),
            "got: {out}"
        );
    }

    #[test]
    fn code_and_stack_are_omitted_but_other_details_kept() {
        let body = json!({
            "error": "MISSING_CYCLE_ID: cycle_id is required",
            "code": "MISSING_CYCLE_ID",
            "stack": "…debug…",
            "details": { "violations": [{ "code": "MISSING_CYCLE_ID" }] }
        });
        let out = format_daemon_error(&body);
        assert!(!out.contains("code:"), "code should be omitted: {out}");
        assert!(!out.contains("stack:"), "stack should be omitted: {out}");
        assert!(
            out.contains("details:"),
            "details should be surfaced: {out}"
        );
        assert!(out.contains("violations"), "got: {out}");
    }

    #[test]
    fn plain_error_is_unchanged() {
        let body = json!({ "error": "boom" });
        assert_eq!(format_daemon_error(&body), "boom");
    }

    #[test]
    fn non_object_body_falls_back_to_unknown() {
        let body = json!("just a string");
        assert_eq!(format_daemon_error(&body), "unknown error");
    }
}
