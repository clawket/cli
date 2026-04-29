# Contributing to `clawket/cli`

The Clawket CLI — `clawket` binary plus the embedded `clawket mcp` stdio
server (rmcp 1.5). Talks to the daemon ([`clawket/daemon`](https://github.com/clawket/daemon))
over a Unix socket; users get a single binary install path via Homebrew or the
`install.sh` one-liner.

## Local setup

```bash
git clone https://github.com/clawket/cli
cd cli
rustup toolchain install stable
cargo build                    # debug build at target/debug/clawket
cargo build --release          # for performance work
```

The CLI auto-spawns the daemon if one isn't running, so for end-to-end work
you also need `clawketd` on `PATH` (or set `CLAWKET_DAEMON_BIN`). Build it
from the daemon repo or `brew install clawket/tap/clawketd`.

## Run tests

```bash
cargo test                     # unit + integration
cargo clippy -- -D warnings    # lint (CI gate)
cargo fmt --check              # formatting (CI gate)
./target/debug/clawket verify --dry-run  # post-install smoke
```

The `tests/` directory contains end-to-end suites that talk to a freshly
spawned daemon over a temp socket — they are slow but catch protocol
regressions. Run them via `cargo test --test '*'`.

## Pull requests

- Branch off `main`. The release workflow (`.github/workflows/release.yml`)
  auto-bumps SemVer from `feat:` / `fix:` / `perf:` commits on push to main —
  do not bump `Cargo.toml` by hand.
- Add a test for every behavior change. Bug fixes get a regression test that
  fails before the fix.
- Run `cargo clippy` and `cargo fmt` before pushing. CI is strict.
- New subcommands need a doctring on the `Command` enum variant — `clap`
  surfaces it as `--help` text.

## Commit convention

Conventional Commits. The release workflow's bump rule:

| Prefix | Bump |
|---|---|
| `feat:` | minor |
| `fix:` / `perf:` | patch |
| `feat!:` / `BREAKING CHANGE:` in body | major |
| `chore:` / `docs:` / `ci:` / `test:` / `refactor:` / `style:` / `build:` | no release |

## Roadmap

See [`ROADMAP.md`](./ROADMAP.md) (cross-repo) for the milestone plan. CLI-
specific work-in-progress lives in the v11 plan in the Clawket workspace.
