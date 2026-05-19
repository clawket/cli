# Contributing to `clawket/cli`

The Clawket CLI — `clawket` binary plus the embedded `clawket mcp` stdio
server (rmcp 1.5). Talks to the daemon ([`clawket/daemon`](https://github.com/clawket/daemon))
over a Unix socket; users get a single binary install path via the
`install.sh` one-liner.

## Cross-repo workflow

The cross-repo contribution model (decompose → contract → execute, active-task
gate, PR / commit conventions, Conventional Commits bump policy, Code of Conduct)
is canonical in the meta repo:

- [`clawket/clawket` › `docs/CONTRIBUTING.md`](https://github.com/clawket/clawket/blob/main/docs/CONTRIBUTING.md) — workflow + repo layout + submission rules
- [`clawket/clawket` › `docs/RELEASING.md`](https://github.com/clawket/clawket/blob/main/docs/RELEASING.md) — release order across the five repos
- [`clawket/clawket` › `CODE_OF_CONDUCT.md`](https://github.com/clawket/clawket/blob/main/CODE_OF_CONDUCT.md) — Contributor Covenant v2.1; reports go to **conduct@clawket.dev**

Do not duplicate those rules here — they live in one place to avoid drift.

## Local setup

```bash
git clone https://github.com/clawket/cli
cd cli
rustup toolchain install stable
cargo build                    # debug build at target/debug/clawket
cargo build --release          # release build for performance work
```

The CLI auto-spawns the daemon if one isn't running, so for end-to-end work
you also need `clawketd` on `PATH` (or set `CLAWKET_DAEMON_BIN`). Build it
from the daemon repo.

## Run tests

```bash
cargo test                     # unit + integration (spawns real daemon on temp socket)
cargo clippy --all-targets -- -D warnings   # CI gate
cargo fmt --all -- --check     # CI gate
./target/debug/clawket verify --dry-run     # post-install smoke
```

The `tests/` directory contains end-to-end suites that talk to a freshly
spawned daemon over a temp socket — they are slow but catch protocol
regressions. Run them via `cargo test --test '*'`.

## Repo-specific PR rules

- Branch off `main`. The release workflow (`.github/workflows/release.yml`)
  auto-bumps the crate version via `cargo set-version --bump` based on
  Conventional Commit subjects since the last tag — **do not edit
  `Cargo.toml#version` by hand**.
- Add a regression test for every behavior change. Bug fixes get a test that
  fails before the fix lands.
- New `clap` variants need a `///` doc-comment — it surfaces as `--help` text
  and is part of the user-facing contract (see `src/main.rs:20` for the
  top-level `about` invariant).
- The MCP server is **stdio-only** (`rmcp` features `["server", "transport-io"]`).
  Adding HTTP / SSE transport requires an RFC in the meta repo first
  (`.claude/rules/mcp-stdio-contract.md`).
- `task complete --evidence` must remain a mandatory `String` at the clap
  level (`.claude/rules/cli-clap-evidence-mandatory.md`).
