# cli/.claude/rules

`clawket/cli` sub-repo 특화 AI 가드레일 룰. 글로벌 룰 (`~/.claude/rules/*.md`) 이 잡지 못하는 cli 도메인 invariant 만 모은다.

## 적용 대상

`clawket/cli` 안에서 동작하는 모든 agent / 사용자가 trigger 하는 모든 코드 변경. wrapper 디렉토리에서 cli 파일을 편집하는 경우에도 동일하게 적용된다.

## 룰 목록

| 파일 | 보호 대상 invariant |
|---|---|
| `mcp-stdio-contract.md` | `clawket mcp` 는 stdio 전용. HTTP/SSE/WebSocket transport 추가 금지. |
| `cli-clap-evidence-mandatory.md` | `task complete --evidence` 는 `String` (필수). `Option` / `default_value` 우회 금지. |
| `cli-daemon-bin-resolution-order.md` | 데몬 바이너리 후보 순서 (`CLAWKET_DAEMON_BIN` → plugin layout → sibling → XDG → PATH) 고정. |
| `cli-unix-socket-only-no-tcp-fallback.md` | CLI client 는 UDS 전용. TCP fallback / 토큰 로딩 금지. |

## 글로벌 룰과의 경계

- 코드 품질 / scope discipline / band-aid 금지 / 문서 snapshot-only → 글로벌 룰이 정본.
- cli 도메인 (배포 layout, MCP transport, evidence invariant, client 채널) → 본 디렉토리가 정본.

## 변경 / 추가 절차

새 룰을 추가하거나 기존 룰을 완화하려면 (a) 동일한 패턴이 ≥3 sub-repo 에서 반복되는지 확인 (그렇다면 글로벌 룰 후보), (b) `cli/CLAUDE.md` 의 Critical contracts / Local AI guardrails 와 cross-reference 가 깨지지 않는지 확인, (c) evidence (file:line) 를 본문에 박아둔다.
