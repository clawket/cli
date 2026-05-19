# clawket CLI

> **Structured task contracts for LLM coding agents.**

Rust CLI for [Clawket](https://github.com/clawket/clawket). Communicates with the local Clawket daemon over HTTP and embeds the MCP stdio server (`rmcp` 1.5) under the `clawket mcp` subcommand.

## Install

### Prebuilt binary (recommended)

Download the platform-appropriate archive from [Releases](https://github.com/clawket/cli/releases):

- `clawket-<version>-x86_64-apple-darwin.tar.gz`
- `clawket-<version>-aarch64-apple-darwin.tar.gz`
- `clawket-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `clawket-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `clawket-<version>-x86_64-pc-windows-msvc.zip`

Extract and place the `clawket` binary on your `PATH`.

### From source

```sh
cargo build --release
./target/release/clawket --version
```

## Plugin v3.0 invariants

The invariants below trace to the **plugin contract version** ([`clawket/clawket`](https://github.com/clawket/clawket)) that this CLI implements — not the CLI binary's own version (see `Cargo.toml`).


- **One active plan per project.** Approve a draft (`draft → active`) before starting tasks.
- **Cycles belong to a unit** (`cycle create --unit UNIT-…`) and run `planning → active → completed`.
- **One active cycle per unit.** Completed cycles cannot be restarted — create a new one.
- **Unit is a pure grouping entity** (no status, no approval).
- **Task is the only entity managed directly**: `todo → in_progress → done | cancelled` (`blocked` for external dependencies).
- **`task complete` requires `--evidence`** (file:line or reasoning summary). The daemon enforces `EVIDENCE_REQUIRED` and returns HTTP 400 if missing.

## Quick start

```sh
clawket project create "my-app" --cwd .
clawket plan create --project PROJ-my-app "MVP"
clawket plan approve PLAN-xxx
clawket unit create --plan PLAN-xxx "Unit 1"
clawket cycle create --project PROJ-my-app --unit UNIT-xxx "Sprint 1"
clawket cycle activate CYC-xxx
clawket task create "Build login" --cycle CYC-xxx
clawket task update TASK-xxx --status in_progress
clawket task complete TASK-xxx --evidence "src/login.rs:42 — hash verified"
```

## Command surface

Global flags (apply to all commands): `--format json|table|yaml`, `--quiet`, `--no-color`, `--locale en|ko|ja`, `--tier low|med|high`.

### Lifecycle

| Command | Purpose |
|---|---|
| `dashboard` | Active-project work summary (active plan, units, cycles, in-progress tasks). |
| `init [--tutorial]` | Onboarding scaffold — fresh user closes their first task in ~5 min. |
| `verify [--dry-run]` | Post-install smoke check: daemon health + write-path round-trip. |
| `doctor [--json] [--plan PLAN] [--escalation]` | Full local diagnostic. Sections (in order): Environment overrides, Paths, Daemon, Database, Hooks, MCP, Plugin install, Compatibility, i18n, Audit log, Data loss risk diagnostics (LM-9), activity_log retention (LM-69), Project enable state (LM-8), Legacy lattice data, Tier-Aware, tier_distribution, escalation_rate (+ escalation report with `--escalation`), Skills, schema_version (components.json), sqlite-vec probe, Path separation invariant (LM-8). Exits non-zero on any failure. |

### Entities

| Command | Subcommands |
|---|---|
| `project` (alias `proj`) | `create`, `view`, `list`, `update`, `delete`, `disable`, `enable`, `resolve`, `cwd {add,remove,list}` |
| `plan` (alias `pl`) | `create`, `view`, `list`, `update`, `delete`, `approve`, `complete`, `import`, `export` |
| `unit` (alias `u`) | `create`, `view`, `list`, `update`, `delete` |
| `cycle` (alias `cy`) | `create`, `view`, `list`, `update`, `delete`, `activate`, `complete` |
| `task` (alias `t`) | `create`, `view`, `list`, `update`, `delete`, `append-body`, `search`, `complete`, `cancel`, `block`, `unblock`, `decompose`, `tree`, `ancestors`, `stats`, `descendants` |
| `knowledge` | `create`, `view`, `list`, `update`, `delete`, `search`, `import`, `export` — wiki content the LLM retrieves via MCP. |
| `run` (alias `r`) | `start`, `finish`, `view`, `list` |
| `comment` (alias `c`) | `create`, `list`, `update`, `delete` |
| `question` (alias `q`) | `create`, `answer`, `view`, `list` |

### Daemon

| Command | Purpose |
|---|---|
| `daemon start` | Launch local `clawketd` (HTTP on `localhost:19400` + Unix socket). |
| `daemon stop` / `restart` / `status` | Lifecycle control + PID/uptime/version readout. |
| `daemon log [--lines N] [-f]` | Show or tail daemon logs from `~/.local/state/clawket/`. |

### Dashboard view launchers

`timeline`, `board`, `wiki`, `summary` — open the respective web view in the dashboard. All accept `--project`.

### Live streams + replay

| Command | Purpose |
|---|---|
| `watch [--project / --task / --cycle] [--format text\|json]` | SSE stream of task/cycle/run events; runs until Ctrl-C. |
| `replay TASK [--limit N]` | Print the run history of a task in order. |

### Knowledge shortcuts (top-level)

| Command | Purpose |
|---|---|
| `find-similar QUERY [--limit N] [--project P]` | Top-level vector search over task title + body. Equivalent surface to `task search --mode semantic`. |
| `get-task-context TASK_ID` | Task body + runs + comments + attached knowledge in one JSON payload — pipe into an LLM prompt. |
| `get-recent-decisions [--project P] [--limit N]` | Recent `type=decision` knowledge entries, newest first. |

### MCP

| Command | Purpose |
|---|---|
| `mcp` | Embedded MCP stdio server (`rmcp` 1.5). Wired into Claude Code via `.mcp.json`. |

The MCP server exposes five read-only knowledge tools — `clawket_search_knowledge`, `clawket_search_tasks`, `clawket_find_similar_tasks`, `clawket_get_task_context`, `clawket_get_recent_decisions`.

### Backup / restore / migrate

| Command | Purpose |
|---|---|
| `backup [--output PATH] [--project P]` | Export DB + attached knowledge to a portable `.tar.gz`. |
| `restore INPUT [--merge] [--dry-run]` | Replace (default) or overlay a backup archive. |
| `migrate [--dry-run]` | Apply pending schema migrations out-of-band (normally automatic at daemon startup). |

### Config

| Command | Purpose |
|---|---|
| `config get \| set \| unset \| list` | Manage values under `~/.config/clawket/`. |

### Self-update

| Command | Purpose |
|---|---|
| `update [--dry-run] [--version VER]` | Download + atomically swap CLI + daemon binaries from GitHub Releases. |
| `version-check` | Compare local against latest release without installing. |

### Discover-loop (alias `dl`)

`discover-loop` — round-by-round QA dispatch, TSV evidence sync, and 3-way convergence query. Subcommands cover plan/cycle/unit auto-generation, batch dispatch manifests, TSV schema validation, bulk transcription, and last-2-rounds-zero convergence checks.

### Shell completions

```sh
clawket completions bash >> ~/.bash_completion
clawket completions zsh > ~/.zfunc/_clawket
clawket completions fish > ~/.config/fish/completions/clawket.fish
clawket completions powershell >> $PROFILE
clawket completions elvish > ~/.elvish/lib/clawket.elv
```

Full reference for any command: `clawket <command> --help`.

## Daemon discovery

- Port file: `$XDG_CACHE_HOME/clawket/clawketd.port` (default `~/.cache/clawket/clawketd.port`).
- Unix socket: `~/.cache/clawket/clawketd.sock`.
- Override: `CLAWKET_DAEMON_URL=http://localhost:PORT`.
- Explicit daemon binary path for hooks: `CLAWKET_DAEMON_BIN`.

If the daemon is not running, most commands attempt an autostart. Diagnose with `clawket doctor`.

## Output

- Default `--format json` — machine-readable entity payloads.
- `--quiet` — entity ID only (no surrounding JSON). Composable with shell pipelines.
- `--no-color` — disable ANSI (also auto-disabled when stdout is not a TTY).

## Contributing

> *Decompose, contract, execute — the structured agent loop.*

Every contribution to Clawket — including this CLI — moves through three steps in order: **decompose** the work into a task tree, **sign each leaf with a contract** (the 19-field execution envelope), then **execute against the contract**. The `PreToolUse` hook in the plugin shell hard-blocks step 3 if steps 1–2 weren't done.

Full guide: [clawket/clawket → docs/CONTRIBUTING.md](https://github.com/clawket/clawket/blob/main/docs/CONTRIBUTING.md).

## License

MIT
