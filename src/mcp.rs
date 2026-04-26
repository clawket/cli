use anyhow::{Context as _, Result};
use regex::Regex;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::io::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::OnceLock;

use crate::client::{self, HttpClient};

const SNIPPET_MAX: usize = 300;
const DECISION_SNIPPET_MAX: usize = 500;
const LIMIT_MAX: u32 = 30;

// ========== Input schemas ==========

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArtifactsArgs {
    #[schemars(description = "검색 쿼리 (자연어 또는 키워드)")]
    pub query: String,
    #[schemars(description = "검색 모드. keyword=FTS5, semantic=벡터, hybrid=병합(기본)")]
    #[serde(default)]
    pub mode: Option<String>,
    #[schemars(description = "반환 개수 (1~30, 기본 10)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(
        description = "아티팩트 타입 필터 (decision|design|architecture|spec|note|doc|reference)"
    )]
    #[serde(default)]
    pub type_filter: Option<String>,
    #[schemars(description = "특정 Plan에 속한 아티팩트만 (선택)")]
    #[serde(default)]
    pub plan_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchTasksArgs {
    #[schemars(description = "검색 쿼리")]
    pub query: String,
    #[schemars(description = "검색 모드. keyword|semantic|hybrid (기본 hybrid)")]
    #[serde(default)]
    pub mode: Option<String>,
    #[schemars(description = "반환 개수 (1~30, 기본 10)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "상태 필터 (todo|in_progress|done|cancelled|blocked)")]
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindSimilarTasksArgs {
    #[schemars(description = "시드 Task ID(TASK-ULID 또는 CK-xxx). 지정 시 query 무시.")]
    #[serde(default)]
    pub task_id: Option<String>,
    #[schemars(description = "자유 쿼리 (task_id 미지정 시 필수)")]
    #[serde(default)]
    pub query: Option<String>,
    #[schemars(description = "반환 개수 (1~30, 기본 5)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "상태 필터 (선택)")]
    #[serde(default)]
    pub status: Option<String>,
    #[schemars(description = "코멘트에서 decisions/issues 추출 여부 (기본 true)")]
    #[serde(default)]
    pub include_extracted: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetTaskContextArgs {
    #[schemars(description = "Task ID (TASK-ULID 또는 CK-xxx)")]
    pub task_id: String,
    #[schemars(
        description = "포함할 섹션 [artifacts, relations, comments, history]. 기본 [artifacts, relations]"
    )]
    #[serde(default)]
    pub include: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRecentDecisionsArgs {
    #[schemars(description = "특정 Plan에 속한 결정만 (선택)")]
    #[serde(default)]
    pub plan_id: Option<String>,
    #[schemars(description = "반환 개수 (1~30, 기본 10)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Unix ms 이후 생성된 결정만 (선택)")]
    #[serde(default)]
    pub since_ts: Option<i64>,
}

// ========== Handler ==========

#[derive(Clone)]
pub struct ClawketMcp {
    http: HttpClient,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl ClawketMcp {
    pub fn new(http: HttpClient) -> Self {
        Self {
            http,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl ClawketMcp {
    #[tool(
        name = "clawket_search_artifacts",
        description = "프로젝트의 설계 문서·결정사항·스펙(아티팩트 scope=rag)을 시맨틱/키워드 하이브리드로 검색합니다. 이전 세션의 결정사항을 찾거나, 특정 주제의 문서를 탐색할 때 사용하세요. 반환: 제목, 타입, 스니펫(300자), 유사도. archive·reference 스코프는 접근 불가 — rag 스코프만 반환합니다."
    )]
    async fn clawket_search_artifacts(
        &self,
        Parameters(args): Parameters<SearchArtifactsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mode = args.mode.as_deref().unwrap_or("hybrid");
        let limit = args.limit.unwrap_or(10).min(LIMIT_MAX);
        let path = format!(
            "/artifacts/search?q={}&mode={}&scope=rag&limit={}",
            urlenc(&args.query),
            urlenc(mode),
            limit
        );
        match client::get(&self.http, &path).await {
            Ok(val) => {
                let arr = val.as_array().cloned().unwrap_or_default();
                let filtered: Vec<Value> = arr
                    .into_iter()
                    .filter(|a| {
                        if let Some(tf) = &args.type_filter
                            && a.get("type").and_then(|v| v.as_str()) != Some(tf)
                        {
                            return false;
                        }
                        if let Some(pid) = &args.plan_id
                            && a.get("plan_id").and_then(|v| v.as_str()) != Some(pid.as_str())
                        {
                            return false;
                        }
                        true
                    })
                    .map(|a| {
                        json!({
                            "id": a.get("id"),
                            "title": a.get("title"),
                            "type": a.get("type"),
                            "plan_id": a.get("plan_id"),
                            "unit_id": a.get("unit_id"),
                            "snippet": snippet(
                                a.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                                SNIPPET_MAX,
                            ),
                            "distance": a.get("_distance").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect();
                Ok(success_json(&Value::Array(filtered)))
            }
            Err(e) => Ok(error_json(&e.to_string())),
        }
    }

    #[tool(
        name = "clawket_search_tasks",
        description = "Clawket의 Task(작업 티켓)를 시맨틱/키워드 하이브리드로 검색합니다. 과거에 비슷한 작업을 했는지, 관련 티켓이 있는지 확인할 때 사용하세요. 더 풍부한 패턴/결정 추출이 필요하면 find_similar_tasks를 사용하세요. 반환: ticket_number(CK-xxx), title, status, priority, unit_id, 유사도."
    )]
    async fn clawket_search_tasks(
        &self,
        Parameters(args): Parameters<SearchTasksArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mode = args.mode.as_deref().unwrap_or("hybrid");
        let limit = args.limit.unwrap_or(10).min(LIMIT_MAX);
        let path = format!(
            "/tasks/search?q={}&mode={}&limit={}",
            urlenc(&args.query),
            urlenc(mode),
            limit
        );
        match client::get(&self.http, &path).await {
            Ok(val) => {
                let arr = val.as_array().cloned().unwrap_or_default();
                let filtered: Vec<Value> = arr
                    .into_iter()
                    .filter(|t| {
                        if let Some(st) = &args.status
                            && t.get("status").and_then(|v| v.as_str()) != Some(st.as_str())
                        {
                            return false;
                        }
                        true
                    })
                    .map(task_summary)
                    .collect();
                Ok(success_json(&Value::Array(filtered)))
            }
            Err(e) => Ok(error_json(&e.to_string())),
        }
    }

    #[tool(
        name = "clawket_find_similar_tasks",
        description = "특정 Task와 의미적으로 유사한 과거 Task들을 찾습니다. \"이 작업 전에 비슷한 이슈를 해결한 적 있는지?\" 확인용. task_id 제공 시 KNN 검색, query 제공 시 자유 쿼리. 결정·이슈 패턴을 코멘트에서 추출해 extracted 필드에 포함합니다."
    )]
    async fn clawket_find_similar_tasks(
        &self,
        Parameters(args): Parameters<FindSimilarTasksArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(5).min(LIMIT_MAX);
        if args.task_id.is_none() && args.query.is_none() {
            return Ok(error_json_code(
                "INVALID_INPUT",
                "task_id 또는 query 중 하나는 필수입니다.",
            ));
        }

        let tasks_res: Result<Vec<Value>, String> = if let Some(tid) = &args.task_id {
            let mut path = format!("/tasks/{}/similar?limit={}", urlenc(tid), limit);
            if let Some(st) = &args.status {
                path.push_str(&format!("&status={}", urlenc(st)));
            }
            client::get(&self.http, &path)
                .await
                .map(|v| v.as_array().cloned().unwrap_or_default())
                .map_err(|e| e.to_string())
        } else {
            let q = args.query.as_deref().unwrap_or("");
            let path = format!(
                "/tasks/search?q={}&mode=semantic&limit={}",
                urlenc(q),
                limit
            );
            client::get(&self.http, &path)
                .await
                .map(|v| {
                    let arr = v.as_array().cloned().unwrap_or_default();
                    if let Some(st) = &args.status {
                        arr.into_iter()
                            .filter(|t| {
                                t.get("status").and_then(|v| v.as_str()) == Some(st.as_str())
                            })
                            .collect()
                    } else {
                        arr
                    }
                })
                .map_err(|e| e.to_string())
        };

        let tasks = match tasks_res {
            Ok(v) => v,
            Err(e) => return Ok(error_json(&e)),
        };

        let include_extracted = args.include_extracted.unwrap_or(true);
        let mut enriched: Vec<Value> = Vec::with_capacity(tasks.len());
        for t in tasks {
            let mut base = task_summary(t.clone());
            if include_extracted {
                let task_id = t.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let body = t.get("body").and_then(|v| v.as_str()).unwrap_or("");
                let comments_path = format!("/tasks/{}/comments", urlenc(task_id));
                let comment_texts: Vec<String> = match client::get(&self.http, &comments_path).await
                {
                    Ok(v) => v
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|c| c.get("body").and_then(|v| v.as_str()).map(String::from))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                let mut joined = String::new();
                if !body.is_empty() {
                    joined.push_str(body);
                }
                for c in &comment_texts {
                    if !joined.is_empty() {
                        joined.push_str("\n\n");
                    }
                    joined.push_str(c);
                }
                let decisions = extract_markers(&joined, decision_regex(), 5);
                let issues = extract_markers(&joined, issue_regex(), 5);
                if let Some(obj) = base.as_object_mut() {
                    obj.insert(
                        "extracted".to_string(),
                        json!({ "decisions": decisions, "issues": issues }),
                    );
                }
            }
            enriched.push(base);
        }
        Ok(success_json(&Value::Array(enriched)))
    }

    #[tool(
        name = "clawket_get_task_context",
        description = "특정 Task의 주변 맥락(관련 아티팩트, 관계, 코멘트, 이력)을 일괄 조회합니다. \"이 티켓이 무슨 배경으로 만들어졌는지\" 파악할 때 사용. 기본 include: artifacts, relations. comments/history는 명시적으로 추가. 아티팩트는 scope=rag만 스니펫 반환."
    )]
    async fn clawket_get_task_context(
        &self,
        Parameters(args): Parameters<GetTaskContextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let include_set: HashSet<String> = args
            .include
            .unwrap_or_else(|| vec!["artifacts".into(), "relations".into()])
            .into_iter()
            .collect();

        let task = match client::get(&self.http, &format!("/tasks/{}", urlenc(&args.task_id))).await
        {
            Ok(v) => v,
            Err(e) => return Ok(error_json(&e.to_string())),
        };
        let task_id = task
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(&args.task_id)
            .to_string();

        let want_art = include_set.contains("artifacts");
        let want_rel = include_set.contains("relations");
        let want_cmt = include_set.contains("comments");
        let want_hst = include_set.contains("history");

        let art_path = format!("/artifacts?task_id={}", urlenc(&task_id));
        let rel_path = format!("/tasks/{}/relations", urlenc(&task_id));
        let cmt_path = format!("/tasks/{}/comments", urlenc(&task_id));
        let hst_path = format!(
            "/activity?entity_type=task&entity_id={}&limit=50",
            urlenc(&task_id)
        );

        let (arts, rels, cmts, hsts) = tokio::join!(
            fetch_optional(&self.http, want_art, &art_path),
            fetch_optional(&self.http, want_rel, &rel_path),
            fetch_optional(&self.http, want_cmt, &cmt_path),
            fetch_optional(&self.http, want_hst, &hst_path),
        );

        let mut response = serde_json::Map::new();
        response.insert(
            "task".to_string(),
            json!({
                "id": task.get("id"),
                "ticket_number": task.get("ticket_number"),
                "title": task.get("title"),
                "status": task.get("status"),
                "priority": task.get("priority"),
                "type": task.get("type"),
                "unit_id": task.get("unit_id"),
                "cycle_id": task.get("cycle_id"),
                "body": task.get("body"),
                "created_at": task.get("created_at"),
                "started_at": task.get("started_at").cloned().unwrap_or(Value::Null),
                "completed_at": task.get("completed_at").cloned().unwrap_or(Value::Null),
            }),
        );

        if want_art {
            let arr = arts
                .unwrap_or_default()
                .into_iter()
                .filter(|a| a.get("scope").and_then(|v| v.as_str()) == Some("rag"))
                .map(|a| {
                    json!({
                        "id": a.get("id"),
                        "title": a.get("title"),
                        "type": a.get("type"),
                        "snippet": snippet(
                            a.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                            SNIPPET_MAX,
                        ),
                    })
                })
                .collect::<Vec<_>>();
            response.insert("artifacts".to_string(), Value::Array(arr));
        }
        if want_rel {
            let mut groups = serde_json::Map::new();
            groups.insert("blocks".to_string(), Value::Array(vec![]));
            groups.insert("blocked_by".to_string(), Value::Array(vec![]));
            groups.insert("relates_to".to_string(), Value::Array(vec![]));
            groups.insert("duplicates".to_string(), Value::Array(vec![]));
            for r in rels.unwrap_or_default() {
                let kind = r
                    .get("relation_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if let Some(bucket) = groups.get_mut(kind).and_then(|v| v.as_array_mut()) {
                    bucket.push(json!({
                        "id": r.get("id"),
                        "source_task_id": r.get("source_task_id"),
                        "target_task_id": r.get("target_task_id"),
                    }));
                }
            }
            response.insert("relations".to_string(), Value::Object(groups));
        }
        if want_cmt {
            let arr = cmts
                .unwrap_or_default()
                .into_iter()
                .map(|c| {
                    json!({
                        "id": c.get("id"),
                        "author": c.get("author"),
                        "created_at": c.get("created_at"),
                        "body": c.get("body"),
                    })
                })
                .collect::<Vec<_>>();
            response.insert("comments".to_string(), Value::Array(arr));
        }
        if want_hst {
            response.insert(
                "history".to_string(),
                Value::Array(hsts.unwrap_or_default()),
            );
        }

        Ok(success_json(&Value::Object(response)))
    }

    #[tool(
        name = "clawket_get_recent_decisions",
        description = "최근 결정(Artifact type=decision, scope=rag)을 목록 조회합니다. 세션 시작 시 \"지난 회차에서 어떤 결정이 있었지?\" 확인용. 자연어 검색이 아닌 타입 기반 필터 — 키워드 탐색은 clawket_search_artifacts를 사용하세요."
    )]
    async fn clawket_get_recent_decisions(
        &self,
        Parameters(args): Parameters<GetRecentDecisionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(10).min(LIMIT_MAX) as usize;
        let mut path = String::from("/artifacts?type=decision");
        if let Some(pid) = &args.plan_id {
            path.push_str(&format!("&plan_id={}", urlenc(pid)));
        }
        match client::get(&self.http, &path).await {
            Ok(val) => {
                let mut arr: Vec<Value> = val.as_array().cloned().unwrap_or_default();
                arr.retain(|a| a.get("scope").and_then(|v| v.as_str()) == Some("rag"));
                if let Some(since) = args.since_ts {
                    arr.retain(|a| {
                        a.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0) >= since
                    });
                }
                arr.sort_by(|a, b| {
                    let ba = b.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
                    let aa = a.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
                    ba.cmp(&aa)
                });
                arr.truncate(limit);
                let mapped: Vec<Value> = arr
                    .into_iter()
                    .map(|a| {
                        json!({
                            "id": a.get("id"),
                            "title": a.get("title"),
                            "plan_id": a.get("plan_id"),
                            "unit_id": a.get("unit_id"),
                            "created_at": a.get("created_at"),
                            "snippet": snippet(
                                a.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                                DECISION_SNIPPET_MAX,
                            ),
                        })
                    })
                    .collect();
                Ok(success_json(&Value::Array(mapped)))
            }
            Err(e) => Ok(error_json(&e.to_string())),
        }
    }
}

#[tool_handler]
impl ServerHandler for ClawketMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Clawket read-only RAG tools. Requires clawketd running (`clawket daemon start`). \
                 Tools: clawket_search_artifacts, clawket_search_tasks, clawket_find_similar_tasks, \
                 clawket_get_task_context, clawket_get_recent_decisions.",
            )
            .with_server_info(Implementation::new("clawket", env!("CARGO_PKG_VERSION")))
    }
}

// ========== Entry ==========

pub async fn run() -> Result<()> {
    let http = client::make_client();
    let handler = ClawketMcp::new(http);
    let service = handler
        .serve(stdio())
        .await
        .context("failed to start MCP stdio server")?;
    service.waiting().await.context("MCP server error")?;
    Ok(())
}

// ========== Helpers ==========

fn success_json(v: &Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| "[]".to_string());
    CallToolResult::success(vec![Content::text(text)])
}

fn error_json(msg: &str) -> CallToolResult {
    error_json_code("DAEMON_ERROR", msg)
}

fn error_json_code(code: &str, msg: &str) -> CallToolResult {
    let body = json!({ "error": { "code": code, "message": msg } });
    let text = serde_json::to_string_pretty(&body).unwrap_or_default();
    CallToolResult::error(vec![Content::text(text)])
}

fn task_summary(t: Value) -> Value {
    json!({
        "id": t.get("id"),
        "ticket_number": t.get("ticket_number"),
        "title": t.get("title"),
        "status": t.get("status"),
        "priority": t.get("priority"),
        "type": t.get("type"),
        "unit_id": t.get("unit_id"),
        "distance": t.get("_distance").cloned().unwrap_or(Value::Null),
    })
}

fn snippet(text: &str, max: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = collapsed.chars().count();
    if count > max {
        let truncated: String = collapsed.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

fn decision_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?m)^\s*(?:[-*]\s*)?(?:결정|확정|Decision|DECISION|결론|선택)\s*[:：\-]\s*(.+)",
        )
        .expect("decision regex")
    })
}

fn issue_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?m)^\s*(?:[-*]\s*)?(?:이슈|문제|Issue|ISSUE|원인|Root cause)\s*[:：\-]\s*(.+)",
        )
        .expect("issue regex")
    })
}

fn extract_markers(text: &str, re: &Regex, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in re.captures_iter(text) {
        if let Some(line) = m.get(1).map(|s| s.as_str().trim().to_string())
            && !line.is_empty()
            && !out.contains(&line)
        {
            out.push(line);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

async fn fetch_optional(client: &HttpClient, want: bool, path: &str) -> Option<Vec<Value>> {
    if !want {
        return None;
    }
    match client::get(client, path).await {
        Ok(v) => Some(v.as_array().cloned().unwrap_or_default()),
        Err(_) => Some(Vec::new()),
    }
}

fn urlenc(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
}
