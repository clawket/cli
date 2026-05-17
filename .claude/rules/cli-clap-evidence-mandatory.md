# Rule: `task complete --evidence` is a mandatory `String`

## Purpose

`task complete` 서브커맨드의 `--evidence` 인자는 **clap level 에서 `String` (필수)** 으로 유지한다. `Option<String>` 으로 완화하거나 기본값 / `unwrap_or_default()` 로 우회하지 않는다. 데몬이 `EVIDENCE_REQUIRED` 로 HTTP 400 을 던지는 것과 CLI 의 clap level 차단은 **이중 방어선**이며, 한쪽이라도 무력화되면 v3.0 done-transition invariant 가 깨진다.

## Prevents

- `evidence: Option<String>` 로 시그니처가 바뀌어 `clawket task complete TASK-…` 가 빈 evidence 로 통과 → 데몬이 거부하긴 하지만 사용자는 invalid input 을 "evidence 깜빡함" 이 아니라 "데몬 오류" 로 오해.
- `#[arg(default_value = "")]` 같은 기본값 추가로 빈 문자열이 데몬까지 전달 → 데몬 prefix-매칭에 따라 거부되지만 CLI 단의 정직한 UX 가 사라진다.
- 새 서브커맨드 (e.g., `task batch-complete`, `task close-as-cancelled-but-done`) 가 evidence 우회 경로를 만드는 것.
- handler 단에서 `unwrap_or_default()` 로 evidence 를 빈 문자열로 만들어 데몬에 보내는 회피.

## Evidence

- `cli/src/main.rs:894-906` — `Complete { id: String, evidence: String, comment: Option<String>, agent: String }`. `evidence` 만 `Option` 없이 String. `comment` 와 대비.
- `cli/src/main.rs:891-893` — doc comment 가 "daemon enforces EVIDENCE_REQUIRED on the done transition, so `--evidence` is mandatory here." 로 의도를 박아둔다.
- `cli/src/main.rs:4426-4436` — handler 가 `Some(evidence.as_str())` 로 그대로 전달 (`.as_deref()` 같은 Option-flatten 없음).
- `cli/CLAUDE.md:41` + `cli/CLAUDE.md:91` — Critical contract 표 + Local AI guardrail §3 가 "Option 으로 바꾸지 말 것" 을 명시.

## Why not global

글로벌 룰은 evidence 정책을 가지지 않는다 (clawket 도메인 특화). `product-quality-first.md` 의 PRE-RESPONSE GATE 는 "out-of-scope skip / silent band-aid" 등을 막지만, 이 시그니처를 "편의를 위해 Option 으로 바꿔도 데몬이 막아주니까 괜찮다" 식으로 합리화하는 것은 도메인 지식 없이 자동으로 잡히지 않는다.

## Enforcement gap

- clap derive 는 시그니처 변경 자체를 거부하지 않는다 — `Option<String>` 으로 바꿔도 컴파일 통과.
- `cargo clippy` 는 도메인 invariant 를 알지 못한다.
- `tests/` 아래에 "evidence 누락 시 clap 이 거부한다" 를 직접 강제하는 negative test 가 없다 (`mcp_compat.rs` 는 MCP 경로만 다룬다).
- 데몬 측 HTTP 400 가드는 정상 동작하지만, CLI 단 우회는 **사용자에게 도달하는 에러 메시지의 품질**을 결정한다 — 이것이 룰의 본질.

## Rule body

### DO

- `Complete` variant 의 `evidence` 필드를 `String` 으로 유지한다.
- 비슷한 done-transition 을 가진 새 서브커맨드 (e.g., bulk close, batch update) 를 추가할 때도 evidence 인자를 `String` 으로 박는다.
- handler 에서 evidence 를 `Some(evidence.as_str())` 형태로 그대로 데몬에 중계한다 (변환 / trim / fallback 금지).
- evidence 값이 빈 문자열이 아닌지 검증할 필요가 생기면 clap `value_parser!` 로 강제하지 데몬에 떠넘기지 않는다.

### DON'T

- `evidence: Option<String>` 로 시그니처를 바꾸지 않는다.
- `#[arg(default_value = "...")]` / `default_value_t` 로 evidence 기본값을 주지 않는다.
- handler 에서 `evidence.unwrap_or_default()` / `evidence.unwrap_or("")` / `if evidence.is_empty()` 회피를 추가하지 않는다.
- `--no-evidence` 같은 escape hatch 플래그를 추가하지 않는다 (운영 장애 시에도 데몬이 거부하므로 CLI 가 거짓말하지 않게 한다).
- 데몬 거부 에러를 "evidence 없어도 통과시키는" wrapper 로 감싸지 않는다.

### Cross-reference

데몬 측 enforcement (`EVIDENCE_REQUIRED` HTTP 400) 는 `daemon` sub-repo 의 책임이다. CLI 룰은 사용자에게 도달하는 첫 번째 방어선을 보장한다 — 데몬 enforcement 와 함께 양쪽이 모두 살아 있어야 invariant 가 성립한다.
