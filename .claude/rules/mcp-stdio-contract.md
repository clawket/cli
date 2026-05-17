# Rule: MCP stdio-only transport contract

## Purpose

`clawket mcp` 서브커맨드가 노출하는 MCP 서버는 **stdio 전용**이다. HTTP / SSE / WebSocket transport 를 추가하지 않는다. Claude Code 의 `.mcp.json` 이 `clawket` 바이너리를 직접 spawn 하여 stdio 로 JSON-RPC 를 주고받는 구조가 v3.0 invariant 이다.

## Prevents

- `rmcp` feature flag 에 `transport-sse` / `transport-streamable-http` 등을 추가 → 두 번째 진입 코드 경로가 생기고, `.mcp.json` 의 `clawket mcp` spawn 가정이 silently 깨진다.
- `mcp::run()` 에 stdio 외 transport branch 가 추가되어 운영 시 stdio MCP client (Claude Code) 가 다른 transport 로 fallthrough.
- CLI 사용자가 `clawket mcp --port 8080` 같은 옵션을 기대하게 만들고, 그 경로가 인증/CORS 책임을 떠안게 됨 (현재 stdio 는 OS 프로세스 경계로 신뢰 모델이 닫혀 있다).

## Evidence

- `cli/src/mcp.rs:537` — `.serve(stdio())` 만 사용. transport 분기 없음.
- `cli/Cargo.toml:25` — `rmcp = { version = "1.5", features = ["server", "transport-io"] }`. stdio 한 종류만 enable.
- `cli/tests/mcp_compat.rs:34-62` — 통합 테스트가 `Command::new(bin).arg("mcp")` + stdin/stdout pipe 로 spawn 한다. 어떤 client 도 다른 transport 를 사용하지 않는다는 contract.
- `cli/CLAUDE.md:42` — Critical contract 표에 stdio MCP 서버 단일 진입점 명시.

## Why not global

`mechanical-overrides.md` / `product-quality-first.md` / `clawket-context-management.md` 는 transport 선택을 다루지 않는다. 이 invariant 는 (a) v3.0 plugin manifest 의 `.mcp.json` spawn 계약, (b) `rmcp` 의 feature 조합, (c) CLI 단일 바이너리 배포 정책 셋이 결합된 cli sub-repo 특화 룰이다.

## Enforcement gap

- `Cargo.toml` 에 새 `transport-*` feature 가 추가되어도 컴파일은 통과한다. cargo lint 가 feature 조합을 강제하지 않는다.
- `mcp::run()` 에 transport branch 를 끼워넣는 PR 을 차단하는 grep gate / CI rule 이 없다.
- 코드 리뷰가 유일한 방어선 — agent 가 "robustness 를 위해" 같은 명분으로 추가 transport 를 제안하지 못하게 룰로 박는다.

## Rule body

### DO

- MCP 서버 진입은 `cli/src/mcp.rs:run()` 의 `.serve(stdio())` 단일 경로 유지.
- `Cargo.toml` 의 rmcp feature 집합 = `["server", "transport-io"]` 고정.
- 새 MCP 도구는 `read-only` 5-tool 집합을 확장하더라도 `tools/call` 처리만 추가하고 transport layer 는 건드리지 않는다.
- 디버깅 시 외부 MCP client 가 필요하면 stdio 어댑터를 client 측에 두고 서버 측 transport 는 그대로 둔다.

### DON'T

- `rmcp` 의 `transport-sse` / `transport-streamable-http` / `transport-worker` feature 를 enable 하지 않는다.
- `clawket mcp --port` / `--http` / `--listen` 같은 CLI 플래그를 추가하지 않는다.
- "HTTP transport 가 더 표준" 같은 이유로 transport 분기를 도입하지 않는다 — RFC 없이 변경 금지.
- `.serve(stdio())` 외 다른 transport 진입점을 `mcp::run()` 에 추가하지 않는다.
- 별도 MCP 서버 바이너리 (예: `clawketd-mcp`) 를 분리하지 않는다 — 단일 바이너리 배포가 정본이다.

### Change protocol

새 transport 가 정말 필요한 경우:
1. wrapper `clawket/docs/` 에 RFC (`rfc-mcp-<transport>.md`) 작성.
2. plugin shell 의 `.mcp.json` 호환성 영향 분석 (`clawket/docs/COMPATIBILITY.md` 업데이트 필요 여부).
3. 인증 / scope filtering / 응답 크기 cap 등 stdio 의 닫힌 신뢰 모델이 깨지는 지점 명시.
4. 사용자 승인 후에만 코드 변경.
