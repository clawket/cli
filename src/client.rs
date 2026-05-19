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
            bail!(
                "{}",
                val.get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown error")
            );
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
        bail!(
            "{}",
            val.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error")
        );
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
        bail!(
            "{}",
            val.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error")
        );
    }
    Ok(val)
}

/// Like `request` but returns the raw `(status, body)` pair so callers can
/// inspect structured `details` on non-success responses (e.g. lease 409
/// holder info). Connect-level failures still bail.
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
