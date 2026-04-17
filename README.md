# clawket CLI

Rust CLI for [Clawket](https://github.com/clawket/clawket). Communicates with the local Clawket daemon over HTTP and manages the MCP stdio server.

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
clawket mcp                           # run @clawket/mcp stdio server (stub)
```

Full command reference: `clawket <command> --help`.

## Architecture

- HTTP client for the Clawket daemon (Hono server on auto-assigned port).
- Port discovery: `$XDG_CACHE_HOME/clawket/clawketd.port` (default `~/.cache/clawket/clawketd.port`).
- Override with `CLAWKET_DAEMON_URL=http://localhost:PORT`.
- `clawket mcp` subcommand spawns `node <mcp>/dist/index.js` as a child process. MCP resolution order:
  1. `CLAWKET_MCP_PATH` environment variable
  2. `<exe_dir>/../mcp/dist/index.js` (plugin-bundled)

## License

MIT
