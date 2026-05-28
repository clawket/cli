# clawket/cli

`clawket` 바이너리 — Clawket 데몬에 Unix socket 으로 말하는 Rust CLI. 동일 바이너리에 `clawket mcp` 서브커맨드로 MCP stdio 서버 (rmcp 1.5) 가 내장되어 있다. 사용자에게는 단일 바이너리 배포 (GitHub Releases, `install.sh`).

> 본 파일은 **이 sub-repo (cli)** 의 AI 컨텍스트 정본이다. Cross-repo 좌표 (compatibility matrix, release order, plugin install gate) 는 wrapper 인 `github.com/clawket/clawket` 의 `CLAUDE.md` + `docs/COMPATIBILITY.md` + `docs/RELEASING.md` 가 단일 진실 공급원 — 여기에 옮기지 않는다.

## Tech stack

| 항목 | 버전 / 비고 |
|---|---|
| 언어 | Rust 2024 edition |
| Crate | `clawket` v0.5.0 (`Cargo.toml:2-3`) |
| CLI 파서 | `clap` 4 (derive + env) |
| MCP | `rmcp` 1.5 (`server`, `transport-io`) — stdio only |
| HTTP 클라 | `hyper` 1 + `hyper-util` (Unix socket 전용; TCP fallback 없음) |
| 비동기 | `tokio` 1 (rt, net, macros, io-std) |
| 스키마 | `schemars` 1.1 (MCP tool 인자) |
| Dev | `reqwest` 0.12, `tempfile` 3 |
| 릴리즈 빌드 | `strip = true`, `lto = true` |

## Module layout (`src/`)

| 파일 | 책임 |
|---|---|
| `main.rs` | clap enum, 라우팅 (대형 — 220KB). 모든 서브커맨드 분기. |
| `mcp.rs` | rmcp stdio 서버 + 5 개 read-only tool 핸들러. |
| `daemon.rs` | `daemon start/stop/restart/status/log` 구현 + spawn 로직. |
| `daemon_autostart.rs` | CLI 진입 시 데몬 자동 기동 (flock 기반, `CLAWKET_NO_AUTOSPAWN` 으로 비활성). |
| `client.rs` | Unix socket `hyper` 클라이언트, GET / request / request_raw. |
| `paths.rs` | XDG 경로 + LM-8 plugin-overlap 가드 + 데몬 바이너리 후보 검색. |
| `doctor.rs` | 진단 — 환경/경로/데몬/DB/훅/MCP/플러그인/호환성/i18n/audit/tier/skills/sqlite-vec/LM-8. |
| `doctor_checks.rs` | doctor 보조 헬퍼. |
| `init.rs` | `clawket init [--tutorial]` 온보딩 스캐폴드. |
| `verify.rs` | `clawket verify [--dry-run]` post-install smoke. |
| `error.rs` | 공용 에러 변환. |

## Critical contracts (with file:line evidence)

| 계약 | 위치 |
|---|---|
| `task complete` 는 `--evidence` 가 **String (필수)** — `Option` 아님. 데몬이 `EVIDENCE_REQUIRED` 로 HTTP 400 강제, CLI 는 clap level 에서 한 번 더 차단. | `src/main.rs:894-900` (Complete variant) → `src/main.rs:4426-4436` (Complete handler) |
| `clawket mcp` 서브커맨드 → 동일 바이너리에 내장된 stdio MCP 서버 진입점. `mcp::run().await`. | `src/main.rs:70-75` (enum) + `src/main.rs:3737-3738` (dispatch) + `src/mcp.rs:517-542` (`run()`) |
| 데몬 자동 기동 + `CLAWKET_DAEMON_BIN` env override (1순위), 그 다음 `paths::daemon_bin_candidates()` (plugin layout → sibling → XDG), 마지막 `PATH` 의 `clawketd`. | `src/daemon_autostart.rs:120-137` (`resolve_daemon_bin`) + `src/daemon.rs:11-31` (`clawketd_cmd`) |
| Cycle 생성에 `--unit` **필수** — `unit: String` (positional 의미상 required). v3.0 invariant: "every cycle belongs to exactly one unit". | `src/main.rs:630-649` (CycleAction::Create) |

> CLI 는 Unix socket only. `client.rs::make_client()` 가 `paths::socket_path()` 만으로 연결을 만들고 HTTP TCP fallback / 토큰 로딩이 없다 (`src/client.rs:41-45`). `~/.cache/clawket/clawketd.token` 은 데몬 TCP 인증용으로 데몬이 발급하지만 CLI 클라이언트는 사용하지 않는다.

## MCP read-only tools

CLI 가 내장 stdio 서버로 노출하는 5 개 — 모두 `/knowledge/*` 또는 `/tasks/*` GET 엔드포인트만 호출한다 (mutation 없음). CLI 는 데몬 응답을 그대로 중계하며 자체 필터링을 하지 않는다.

| Tool | 데몬 엔드포인트 | `src/mcp.rs` line |
|---|---|---|
| `clawket_search_knowledge` | `GET /knowledge/search?q=&mode=&limit=` | `123-176` |
| `clawket_search_tasks` | `GET /tasks/search?...` | `178-213` |
| `clawket_find_similar_tasks` | semantic search variant | `215-310` |
| `clawket_get_task_context` | `GET /knowledge?task_id=` + tasks/comments/history | `312-438` |
| `clawket_get_recent_decisions` | `GET /knowledge?type=decision` | `440-493` |

응답 크기는 `MCP_RESPONSE_MAX_BYTES = 100 KB` 로 cap (Claude Code 의 50 KB silent truncation 보다 큰 안전 마진).

## Build / test / run

| 명령 | 용도 |
|---|---|
| `cargo build --release` | 릴리즈 빌드. 산출물: `target/release/clawket`. |
| `cargo build` | 디버그 빌드: `target/debug/clawket`. |
| `cargo test --all` | 유닛 + `tests/` 통합 (실제 데몬 spawn). |
| `cargo clippy --all-targets` | CI gate (`.github/workflows/ci.yml`). |
| `cargo fmt --all -- --check` | CI gate. |
| `./target/release/clawket mcp` | MCP stdio 서버 수동 기동 (디버깅 시 stdio 에 JSON-RPC). |
| `CLAWKET_DAEMON_BIN=/path/to/clawketd ./target/release/clawket ...` | 개발용 데몬 경로 오버라이드. |
| `CLAWKET_NO_AUTOSPAWN=1 clawket ...` | 자동 기동 비활성 (이미 띄운 데몬에 붙고 싶을 때). |
| `clawket doctor` | 전체 진단, 실패 시 exit code ≠ 0. |
| `clawket verify --dry-run` | post-install smoke (write-path round-trip). |

CI workflow (`.github/workflows/ci.yml`): `cargo fmt --all -- --check` → `cargo clippy --all-targets` → `cargo build --release` → `cargo test --all`.

## Help-text discipline

모든 clap variant 는 `///` doc-comment (clap `about`/`long_about`) 를 carry 한다. 최상위 `about` (`src/main.rs:20`) 가 v3.0 invariant 와 quick-start 를 박는다. v3.0 baseline 의 핵심을 help text 에 그대로 반영해야 한다:

- `task complete` 의 `--evidence` 는 mandatory 로 명시 (`src/main.rs:891-893, 897-898`).
- `cycle create --unit` 은 required 로 명시 (`src/main.rs:632-633, 640`).
- `--tier low|med|high` 는 task scope advisory.

## Local AI guardrails

1. **사용자가 명시적으로 지시하지 않는 한 commit / push 금지** (wrapper 규칙과 동일).
2. **변경 후 `cargo check` (또는 가능하면 `cargo clippy --all-targets`) 통과 확인 전에는 done 보고 금지.** 활성 task 에 `--evidence` 로 file:line 또는 reasoning summary 를 항상 동봉.
3. **`--evidence` 우회 금지.** CLI 레벨에서 skip 하도록 default 값을 주거나 Option 으로 바꾸지 말 것. 데몬도 `EVIDENCE_REQUIRED` 로 거부하므로 우회는 양쪽 모두 깨야 한다.
4. **MCP 서버는 stdio 전용 유지.** HTTP transport / SSE transport 추가는 별도 RFC 없이는 거부 — 현재 contract 는 Claude Code 의 `.mcp.json` 에서 직접 `clawket mcp` 를 spawn 하는 것.
5. **파일 변경 직전 reread.** `main.rs` 가 220KB / 5000+ LOC 이므로 stale context 위험이 크다 (`mechanical-overrides.md` §9).
