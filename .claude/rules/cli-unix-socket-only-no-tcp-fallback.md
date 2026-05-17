# Rule: CLI client is Unix socket only — no TCP / token fallback

## Purpose

`cli/src/client.rs` 의 HTTP 클라이언트는 **오직 Unix domain socket** 으로만 데몬과 통신한다. TCP fallback, port-file 기반 dial, `~/.cache/clawket/clawketd.token` loading 을 CLI 클라이언트에 도입하지 않는다. 토큰은 데몬이 TCP listener 를 띄울 때만 발급/검증하며, CLI 는 UDS 만 사용하므로 토큰 책임이 없다 — 이 책임 경계가 단일 진실 공급원이다.

## Prevents

- "데몬이 안 뜨거나 socket 이 stale 할 때 TCP 로 fallback" 같은 "robustness" 명분의 추가 코드 경로 → 두 채널이 인증 모델이 다르고, CLI 가 토큰 lifecycle 책임을 떠안게 된다.
- `~/.cache/clawket/clawketd.token` 를 CLI 가 읽어 `Authorization: Bearer …` 헤더를 붙이는 코드 → 토큰 파일이 사라지거나 회전됐을 때 CLI 가 stale token 으로 401 을 받음.
- `clawketd.port` 파일을 읽어 `http://127.0.0.1:<port>` 로 dial 하는 fallback → 데몬이 TCP 를 disable 한 환경에서 silent failure.
- CLI 가 통신 채널을 두 종류 가지게 되어 socket-only invariant (`cli/CLAUDE.md:46`) 가 무너지는 것.

## Evidence

- `cli/src/client.rs:14-37` — `UnixConnector` 가 `tokio::net::UnixStream::connect(&*path)` 만 호출. TCP / hyper-util HttpConnector 등 다른 connector 없음.
- `cli/src/client.rs:41-45` — `make_client()` 가 `paths::socket_path()` 만으로 connector 생성. 토큰 / port 파일 / TCP 분기 부재.
- `cli/src/client.rs:47-65,67-119` — `get` / `request` / `request_raw` 가 `http://localhost{path}` 로만 URI 구성하고 host header / Authorization header 를 직접 다루지 않는다.
- `cli/CLAUDE.md:46` — "CLI 는 Unix socket only. … HTTP TCP fallback / 토큰 로딩이 없다 … `~/.cache/clawket/clawketd.token` 은 데몬 TCP 인증용으로 데몬이 발급하지만 CLI 클라이언트는 사용하지 않는다."
- `cli/src/paths.rs:46-60` — `socket_path()` / `pid_path()` / `port_path()` 가 분리되어 있고, `client.rs` 는 `socket_path()` 만 import.

## Why not global

채널 단일화는 daemon ↔ CLI ↔ plugin 셋의 신뢰 모델 분리 결정의 결과다 (UDS = OS 프로세스 신뢰, TCP = 토큰 신뢰). 글로벌 룰은 transport / 인증 책임 경계를 다루지 않는다. 이 invariant 는 cli sub-repo 의 client 레이어가 자기 자신을 좁게 유지한다는 약속이다.

## Enforcement gap

- 컴파일러는 `hyper-util::client::legacy::connect::HttpConnector` 를 CLI 가 추가로 import 하는 것을 막지 않는다.
- `cargo clippy` 는 도메인 의미를 모른다.
- `cli/tests/` 에 "client 가 TCP 로 dial 하지 않는다" 를 직접 강제하는 negative test 가 없다 (간접적으로 `mcp_compat.rs` 가 dead socket 시나리오를 검증할 뿐).
- "운영 안정성 향상" 명분으로 token loading 을 추가하는 PR 을 lint 가 거부하지 않는다.

## Rule body

### DO

- 모든 데몬 호출은 `client::make_client()` 가 만든 UDS 기반 client 를 통과시킨다.
- 새 엔드포인트를 호출해야 하면 `client::get` / `client::request` / `client::request_raw` 를 그대로 사용한다 (URI 는 `/path?query` 형태).
- 연결 실패 시 사용자에게 "is it running? (`clawket daemon start`)" 메시지를 보존한다 (`client.rs:52,110`).
- TCP / 토큰이 필요한 client (예: 외부 admin 도구) 가 미래에 필요해지면 별도 crate / 별도 binary 로 분리한다.

### DON'T

- `client.rs` 에 `HttpConnector` / `https_connector` / `TcpStream` 을 추가하지 않는다.
- `paths::port_path()` 를 `client.rs` 에서 import / 호출하지 않는다.
- `~/.cache/clawket/clawketd.token` 또는 `CLAWKET_TOKEN` env 를 CLI 클라이언트가 읽어 헤더에 붙이지 않는다.
- "UDS 실패 시 TCP fallback" 같은 retry 로직을 추가하지 않는다 — UDS 실패는 데몬 미기동 / 권한 문제이며, 사용자에게 정직하게 보고하는 것이 정답.
- 새 함수가 hyper `Request` 를 우회해 직접 `reqwest` / `ureq` 로 데몬에 dial 하는 path 를 만들지 않는다.

### Cross-reference

데몬 측의 TCP listener / 토큰 발급 정책은 `daemon` sub-repo 가 책임지며, plugin shell 의 install gate 가 socket path (`~/.cache/clawket/clawketd.sock`) 와 plugin layout 의 일관성을 책임진다. CLI 측은 채널 하나만 알고 있으면 된다는 것이 본 룰의 핵심.
