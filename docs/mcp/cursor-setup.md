# Clawket MCP — Cursor manual verification

Status: manual smoke test for cross-client MCP compatibility (LM-77 / RL-U4-12).
Pair: automated harness lives at `cli/tests/mcp_compat.rs` and covers Inspector-grade
contract assertions. Cursor cannot be driven from CI today, so this doc captures the
steps and expected output for a human-run verification before each release.

## Prerequisite

Cursor 0.45+ (Settings → Features → MCP visible) and a built `clawket` binary on
`PATH`:

```bash
which clawket && clawket --version
```

If `clawket daemon status` reports it is down, start it once — the MCP server will
auto-start it on first tool call too, but starting up front gives cleaner first-run
latency:

```bash
clawket daemon start
```

## 1. Register the server

Cursor reads MCP servers from `~/.cursor/mcp.json` (global) or
`<repo>/.cursor/mcp.json` (per-workspace). Add:

```json
{
  "mcpServers": {
    "clawket": {
      "command": "clawket",
      "args": ["mcp"]
    }
  }
}
```

No env vars are required — the binary picks up XDG paths and the daemon socket
itself. If the daemon binary lives outside `PATH`, set `CLAWKET_DAEMON_BIN` here.

Reload Cursor (Cmd+Shift+P → "Developer: Reload Window") so the MCP client
re-spawns.

## 2. Confirm the handshake

Open Settings → Features → MCP. The `clawket` entry must show:

- Status: **green dot / "Running"**
- Tools count: **5**

If it stays yellow, click the entry to open Cursor's MCP log and look for a
JSON-RPC error from `clawket mcp` on stderr.

## 3. Verify all 5 tools are listed

Expected names (alphabetical UI sort, but order doesn't matter):

- `clawket_find_similar_tasks`
- `clawket_get_recent_decisions`
- `clawket_get_task_context`
- `clawket_search_knowledge`
- `clawket_search_tasks`

Every entry must have a non-empty description (Cursor renders it inline). If any
description is blank, treat it as a regression — the same assertion is enforced by
`every_tool_has_description_and_input_schema` in `mcp_compat.rs`.

## 4. Drive a read-only call from chat

In a Cursor chat panel, ask:

> Use clawket_search_knowledge to find the latest `decision` knowledge entry about MCP.

Expect: a tool-call card titled `clawket_search_knowledge`, an `arguments` JSON
preview with `query`, and a result card containing one or more knowledge entries as a
JSON list. No retries, no schema-validation error popup.

If the daemon is down, the result card will display structured JSON with an
`error` key (mirroring the harness assertion in
`daemon_missing_returns_error_not_panic`). That is a feature — surface it to the
user and stop.

## 5. Drive a validation error path

Ask:

> Use clawket_find_similar_tasks with no arguments.

Expect: tool-call card returns text containing `INVALID_INPUT` (matches
`invalid_input_returns_validation_error` in the harness). Cursor should NOT mark
the call as a transport failure — it is an in-band validation error from the
tool body and the client should render it as a successful tool call with an
error payload.

## What to record before sign-off

When closing any release that touches `cli/src/mcp.rs`, capture:

1. Cursor version (`Cursor → About`)
2. `clawket --version` and `clawketd --version`
3. Output of `cargo test --test mcp_compat -- --nocapture` (paste the `5 passed` line)
4. Screenshots:
   - Settings → Features → MCP showing all 5 tools
   - One successful tool-call card from §4
   - One INVALID_INPUT card from §5

Store evidence as a Clawket knowledge entry tagged `decision` referencing the task ID,
not a loose file in the repo.

## Why both an automated harness and a manual checklist

The harness (`cli/tests/mcp_compat.rs`) drives `clawket mcp` directly over
JSON-RPC 2.0 stdio, asserts the wire contract, and runs in CI. It guarantees the
**server side** of MCP. It cannot prove a real client renders the tool list,
respects `inputSchema`, or correctly forwards arguments — those are properties of
the client. Cursor (and Claude Code, Inspector) each interpret the protocol with
their own quirks. This doc is the human-in-the-loop check that the contract
asserted by the harness actually shows up in the UI a user sees.

If a Cursor regression appears that the harness misses, add a new test to
`mcp_compat.rs` reproducing the underlying wire-level deviation and link the
test name in this doc.
