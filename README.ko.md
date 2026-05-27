<!-- 번역 상태: 정본은 README.md (영문). 영문이 갱신되면 docs/i18n-policy.md 의 14d/21d drift 윈도우 안에 본 파일을 동기화한다. -->

[English](README.md)

# clawket CLI

> **LLM 코딩 에이전트를 위한 구조화된 태스크 계약.**

[Clawket](https://github.com/clawket/clawket) 의 Rust CLI. 로컬 Clawket 데몬과 HTTP 로 통신하며, `clawket mcp` 서브커맨드 아래에 MCP stdio 서버(`rmcp` 1.5)를 내장한다.

## 설치

### 프리빌드 바이너리 (권장)

[Releases](https://github.com/clawket/cli/releases) 에서 플랫폼에 맞는 아카이브를 다운로드한다:

- `clawket-<version>-x86_64-apple-darwin.tar.gz`
- `clawket-<version>-aarch64-apple-darwin.tar.gz`
- `clawket-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `clawket-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `clawket-<version>-x86_64-pc-windows-msvc.zip`

압축을 풀고 `clawket` 바이너리를 `PATH` 에 둔다.

### 소스 빌드

```sh
cargo build --release
./target/release/clawket --version
```

## v3.0 invariants

- **프로젝트당 active plan 은 하나.** 태스크를 시작하기 전 draft 를 `draft → active` 로 approve.
- **Cycle 은 unit 에 속한다** (`cycle create --unit UNIT-…`). 상태는 `planning → active → completed`.
- **Unit 당 active cycle 은 하나.** 완료된 cycle 은 재시작 불가 — 새로 만든다.
- **Unit 은 순수 grouping** (status 없음, approval 없음).
- **Task 만 직접 관리**: `todo → in_progress → done | cancelled` (외부 의존이면 `blocked`).
- **`task complete` 는 `--evidence` 필수** (file:line 또는 추론 요약). 데몬이 `EVIDENCE_REQUIRED` 를 강제해 누락 시 HTTP 400.

## 빠른 시작

```sh
clawket project create "my-app" --cwd .
clawket plan create --project PROJ-my-app "MVP"
clawket plan approve PLAN-xxx
clawket unit create --plan PLAN-xxx "Unit 1"
clawket cycle create --project PROJ-my-app --unit UNIT-xxx "Sprint 1"
clawket cycle activate CYC-xxx
clawket task create "Build login" --cycle CYC-xxx \
  --intent "로그인 화면 구현 및 인증 호출 연결" \
  --prompt-template "로그인 폼을 만들고 인증 엔드포인트에 연결" \
  --success-criteria "유효 자격증명은 /home 으로 리다이렉트,무효 자격증명은 에러 표시"
clawket task update TASK-xxx --status in_progress
clawket task complete TASK-xxx --evidence "src/login.rs:42 — hash verified"
```

## 명령어 표면

전역 플래그 (모든 명령어 공통): `--format json|table|yaml`, `--quiet`, `--no-color`, `--locale en|ko|ja`, `--tier low|med|high`.

### 라이프사이클

| 명령어 | 용도 |
|---|---|
| `dashboard` | active 프로젝트 작업 요약 (active plan, units, cycles, in-progress tasks). |
| `init [--tutorial]` | 온보딩 스캐폴드 — 신규 사용자가 약 5분 안에 첫 태스크 완료. |
| `verify [--dry-run]` | 설치 후 스모크 체크: 데몬 헬스 + 쓰기 경로 왕복 검증. |
| `doctor [--json] [--plan PLAN] [--escalation]` | 로컬 전체 진단. 섹션(실행 순서): Environment overrides, Paths, Daemon, Database, Hooks, MCP, Plugin install, Compatibility, i18n, Audit log, Data loss risk diagnostics (LM-9), activity_log retention (LM-69), Project enable state (LM-8), Legacy lattice data, Tier-Aware, tier_distribution, escalation_rate (`--escalation` 시 escalation report 추가), Skills, schema_version (components.json), sqlite-vec probe, Path separation invariant (LM-8). 실패 시 exit non-zero. |

### 엔티티

| 명령어 | 서브커맨드 |
|---|---|
| `project` (alias `proj`) | `create`, `view`, `list`, `update`, `delete`, `disable`, `enable`, `resolve`, `cwd {add,remove,list}` |
| `plan` (alias `pl`) | `create`, `view`, `list`, `update`, `delete`, `approve`, `complete`, `import`, `export` |
| `unit` (alias `u`) | `create`, `view`, `list`, `update`, `delete` |
| `cycle` (alias `cy`) | `create`, `view`, `list`, `update`, `delete`, `activate`, `complete`, `counts` |
| `task` (alias `t`) | `create`, `view`, `list`, `update`, `delete`, `append-body`, `search`, `complete`, `cancel`, `block`, `unblock`, `decompose`, `tree`, `ancestors`, `stats`, `descendants` |
| `knowledge` | `create`, `view`, `list`, `update`, `delete`, `search`, `import`, `export`, `wiki-tree` — LLM 이 MCP 로 가져갈 wiki 컨텐츠. |
| `run` (alias `r`) | `start`, `finish`, `view`, `list` |
| `comment` (alias `c`) | `create`, `list`, `update`, `delete` |
| `question` (alias `q`) | `create`, `answer`, `view`, `list` |

### 데몬

| 명령어 | 용도 |
|---|---|
| `daemon start` | 로컬 `clawketd` 기동 (`localhost:19400` HTTP + Unix socket). |
| `daemon stop` / `restart` / `status` | 라이프사이클 + PID / uptime / 버전 표시. |
| `daemon log [--lines N] [-f]` | `~/.local/state/clawket/` 의 로그 출력/tail. |

### 대시보드 뷰 런처

`timeline`, `board`, `wiki`, `summary` — 해당 뷰를 웹 대시보드에서 연다. 모두 `--project` 수용.

### 라이브 스트림 + 리플레이

| 명령어 | 용도 |
|---|---|
| `watch [--project / --task / --cycle] [--format text\|json]` | task/cycle/run SSE 스트림. Ctrl-C 까지 지속. |
| `replay TASK [--limit N]` | 태스크의 run 이력을 순서대로 출력. |
| `events replay [필터]` | 감사 로그(audit-log) 항목을 유한 SSE 스트림으로 리플레이(`/events/replay`); 행마다 JSON 하나 출력 후 종료. |

### Knowledge 단축 (top-level)

| 명령어 | 용도 |
|---|---|
| `find-similar QUERY [--limit N] [--project P]` | task title+body 에 대한 top-level 벡터 검색. `task search --mode semantic` 와 동등한 surface. |
| `get-task-context TASK_ID` | task body + runs + comments + attached knowledge 를 하나의 JSON 으로 — LLM 프롬프트에 파이프. |
| `get-recent-decisions [--project P] [--limit N]` | 최근 `type=decision` knowledge 를 최신순. |

### MCP

| 명령어 | 용도 |
|---|---|
| `mcp` | 내장 MCP stdio 서버 (`rmcp` 1.5). 플러그인 `.mcp.json` 으로 Claude Code 에 연결. |

MCP 서버는 5 개의 read-only knowledge 도구를 노출한다 — `clawket_search_knowledge`, `clawket_search_tasks`, `clawket_find_similar_tasks`, `clawket_get_task_context`, `clawket_get_recent_decisions`.

### 백업 / 복원 / 마이그레이션

| 명령어 | 용도 |
|---|---|
| `backup [--output PATH] [--project P]` | DB + 첨부 knowledge 를 portable `.tar.gz` 로 내보내기. |
| `restore INPUT [--merge] [--dry-run]` | 교체 (default) 또는 overlay 로 백업 적용. |
| `migrate [--dry-run]` | pending 스키마 마이그레이션을 out-of-band 로 적용 (보통 데몬 startup 에서 자동). |

### 설정

| 명령어 | 용도 |
|---|---|
| `config get \| set \| unset \| list` | `~/.config/clawket/` 의 값을 관리. |

### 자체 업데이트

| 명령어 | 용도 |
|---|---|
| `update [--dry-run] [--version VER]` | CLI + 데몬 바이너리를 GitHub Releases 에서 받아 atomic swap. |
| `version-check` | 설치 없이 로컬 vs 최신 릴리즈 비교. |

### Discover-loop (alias `dl`)

`discover-loop` — 라운드 단위 QA dispatch, TSV evidence sync, 3-way 수렴 query. 서브커맨드는 plan/cycle/unit 자동 생성, batch dispatch manifest, TSV 스키마 검증, bulk transcription, last-2-rounds-zero 수렴 체크.

### 셸 자동완성

```sh
clawket completions bash >> ~/.bash_completion
clawket completions zsh > ~/.zfunc/_clawket
clawket completions fish > ~/.config/fish/completions/clawket.fish
clawket completions powershell >> $PROFILE
clawket completions elvish > ~/.elvish/lib/clawket.elv
```

각 명령어의 전체 레퍼런스: `clawket <command> --help`.

## 데몬 디스커버리

- 포트 파일: `$XDG_CACHE_HOME/clawket/clawketd.port` (기본 `~/.cache/clawket/clawketd.port`).
- Unix socket: `~/.cache/clawket/clawketd.sock`.
- 수동 지정: `CLAWKET_DAEMON_URL=http://localhost:PORT`.
- 훅용 데몬 바이너리 경로 명시: `CLAWKET_DAEMON_BIN`.

데몬이 실행 중이 아니면 대부분의 명령어는 autostart 를 시도한다. `clawket doctor` 로 진단.

## 출력

- 기본 `--format json` — 머신 친화 엔티티 페이로드.
- `--quiet` — 엔티티 ID 만 (JSON 래퍼 없음). 셸 파이프라인 조합용.
- `--no-color` — ANSI 비활성 (stdout 이 TTY 가 아니면 자동 비활성).

## 기여

> *분해, 계약, 실행 — 구조화된 에이전트 루프.*

Clawket 에 기여하는 모든 작업 (이 CLI 포함) 은 세 단계를 순서대로 거친다: **분해** (작업을 태스크 트리로 쪼갬), **각 leaf 에 계약 서명** (19 필드 실행 envelope), **계약 대비 실행**. 플러그인 shell 의 `PreToolUse` 훅이 1–2 단계를 거치지 않은 3 단계를 하드 블록한다.

전체 가이드: [clawket/clawket → docs/CONTRIBUTING.md](https://github.com/clawket/clawket/blob/main/docs/CONTRIBUTING.md).

## 라이선스

MIT
