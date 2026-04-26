# clawket CLI

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

## Usage

```sh
clawket dashboard                     # current project overview
clawket task create "Fix login bug"   # create a task under the active plan/cycle
clawket task update <ID> --status in_progress
clawket plan approve <PLAN_ID>        # draft → active
clawket cycle activate <CYCLE_ID>     # planning → active
clawket mcp                           # run the embedded MCP stdio server (rmcp 1.5)
clawket daemon start                  # launch the local clawketd if not already running
clawket doctor                        # diagnose install / daemon / port issues
```

Full command reference: `clawket <command> --help`.

## Architecture

- HTTP client for the Clawket daemon (Rust `axum` server on an auto-assigned port).
- Port discovery: `$XDG_CACHE_HOME/clawket/clawketd.port` (default `~/.cache/clawket/clawketd.port`).
- Override with `CLAWKET_DAEMON_URL=http://localhost:PORT`.
- `clawket mcp` runs the **embedded** MCP stdio server in-process (`rmcp` 1.5) — no Node child process, no `@clawket/mcp` dependency. The legacy `@clawket/mcp` Node server is deprecated and was removed from the plugin's dependencies in plugin v2.3.2.
- 5 read-only RAG tools are exposed: `clawket_search_artifacts`, `clawket_search_tasks`, `clawket_find_similar_tasks`, `clawket_get_task_context`, `clawket_get_recent_decisions`. Only `scope=rag` artifacts are returned.

## License

MIT
