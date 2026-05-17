# ADR-0002 — Hook Injection Strategy for `clawket execute`

| Status | Owner task | Targets | Plan |
|---|---|---|---|
| **Accepted** (2026-04-28) | LM-148 / RL-U5-02b | `clawket execute <task>` headless spawn path; downstream `clawket spawn` batch path | v11 — Structured Task Contracts |

## Context

`clawket execute <task>` (LM-149) spawns `claude -p` headlessly to run an envelope-bound task. For the spawned Claude Code session to do the right work, **the envelope and its surrounding context must be inside the model's session context** when the first user prompt fires.

We have two technically distinct mechanisms by which Claude Code populates that context:

- **Strategy A — SessionStart hook returning `hookSpecificOutput.additionalContext`.** Claude Code runs the registered hook, captures stdout, and injects the `additionalContext` field into the model context wrapped in a `<system-reminder>` block. This is what the global Clawket plugin already does for interactive sessions (dashboard injection).

- **Strategy B — `--append-system-prompt <text>` flag.** Claude Code appends the supplied string verbatim to the system prompt. No hook required; the spawner controls everything, including framing.

Empirical visibility for both was verified in **ADR-0002 (Evidence)** (LM-147, 2026-04-28):

| Probe | Strategy A sentinel echoed? | Strategy B sentinel echoed? |
|---|---|---|
| Headless `claude -p`, Haiku 4.5 | ✓ `9D2F1E7B` | ✓ `7A3C92E5` |
| Source of truth | hook stdout JSON | CLI flag string |

Both land in the model. The decision is therefore not "which one works" but "which one is right for *spawning a single envelope-bound task headlessly under CLI control*".

## Decision

Decision: **Hybrid, with Strategy B (`--append-system-prompt`) as the primary mechanism for `clawket execute`.**

Concretely:

1. **`clawket execute <task>` (and its batch sibling `clawket spawn`) inject the envelope payload via `--append-system-prompt`.** The CLI computes the full payload (envelope + ancestors + cycle siblings + recent decisions, per ADR-0003 priority stack) before spawn and passes it as a single `--append-system-prompt` argument. The CLI owns the framing string (`# Clawket Execution Envelope` + structured sections) and is responsible for size budgeting.

2. **Interactive Claude Code sessions keep using Strategy A** (the existing `SessionStart` hook in the Clawket plugin). That path is for general dashboard context, runs continuously, and benefits from being dynamic. Nothing about ADR-0002 changes the plugin's hook.

3. **The two paths do not stack on the same spawn.** When `clawket execute` runs `claude -p`, it explicitly passes `--settings` pointing at a *minimal settings file that registers no hooks* (or, equivalently, sets `--setting-sources` to exclude `user`/`project` so the global plugin hook does not fire in the spawned process). The envelope is the single source of context for that spawn.

4. **Fallback:** If the spawner cannot compute the full payload (e.g. clawketd is unreachable per ADR-0003 F1), it falls back to a minimal envelope-only `--append-system-prompt` and emits a `clawket execute` warning. There is no further fallback to Strategy A — a missing payload is a hard failure mode the user must see, not silently masked by a parallel injection path.

## Rationale

The two strategies map onto different runtime shapes:

| Aspect | Strategy A (hook) | Strategy B (append) |
|---|---|---|
| **Spawner controls framing** | No — Claude Code wraps in `<system-reminder>` with a fixed prefix `SessionStart hook additional context:`. | Yes — verbatim append. |
| **Multi-source collision** | Yes — when the global Clawket plugin's `SessionStart` hook also fires, both payloads land and ordering is implementation-defined (observed in LM-147 evidence). | No — `--append-system-prompt` is a single flag. |
| **Setup dependency** | Hook must be installed and discoverable; settings file must register it. | Zero setup. CLI flag works on a clean Claude Code install. |
| **Refresh model** | Re-evaluated every session start. Useful for interactive sessions whose context drifts. | Frozen at spawn time. Correct for `execute`, where the envelope is by definition the immutable input contract. |
| **Failure mode** | Hook stderr is logged but the session continues without the context. Silent partial injection. | If `--append-system-prompt` is absent, the model has no envelope and we want to know — the CLI controls this branch. |
| **Payload size ceiling** | Bounded by hook stdout (Claude Code does not document a hard cap; large payloads have not been characterised). | Bounded by system prompt size (effectively very large). |
| **Latency** | Hook exec adds ~50ms per spawn. | None. |

For `clawket execute`'s contract — *"run this envelope, fail loudly if you can't"* — every row above prefers B:

- **Spawner controls framing** matters because the envelope-following prompt template (`prompt_template` field of ADR-0001) needs a known anchor in the context to reference. Strategy A's wrapping prefix is not under our control and could change across Claude Code versions; Strategy B gives us a stable contract.
- **Multi-source collision** is the actual reason A is unsafe here: if a user happens to have the global plugin installed at the time of `clawket execute`, the spawned session would see *both* the dashboard dump and the envelope, with undefined precedence. Forcing a hook-free spawn (point 3) sidesteps that entirely — and once we are forcing a hook-free spawn, Strategy B is the only remaining mechanism.
- **Frozen at spawn time** is a feature, not a limitation, for `execute`. Replayability (ADR-0001 invariant) requires that the prompt the agent saw can be reconstructed; a payload computed once by the CLI before spawn is reproducible from the envelope ID, while a hook re-evaluating its data sources at spawn time is not.

## Pros / Cons

### Pros (this decision)

- **Single source of context per spawn** — no race between multiple `SessionStart` hooks.
- **Replayable** — the `--append-system-prompt` payload is logged with the run record (ADR-0010 activity log) and can be reconstructed from the envelope.
- **No install dependency** — `clawket execute` works even if the user has not installed the Clawket plugin in Claude Code, because the CLI is the spawner.
- **Stable framing** — we own the section headers, so future `clawket replay` and `clawket execute --resume` can parse the payload back out.
- **Interactive flow unchanged** — the existing plugin hook keeps working for daily Claude Code use.

### Cons (this decision)

- **Two code paths** for "context injection" exist in the codebase: the plugin's `SessionStart` hook (interactive) and the CLI's `--append-system-prompt` builder (`execute`). Both must remain in sync with ADR-0003's priority stack.
- **No dynamic refresh inside a spawn** — but this is intended; see Rationale.
- **CLI must compute the payload before spawn** — adds one clawketd round-trip on the critical path. Mitigated by ADR-0003 fallback F3 (750ms cap on partial-endpoint waits).
- **`--setting-sources` / hook-free-settings handling is environment-sensitive** — verified working in LM-147 fixture but must be re-verified when the executing user has aggressive global settings. Tracked as a follow-up probe.

## Alternatives considered

### Alternative 1 — Strategy A only

Reuse the existing plugin `SessionStart` hook (or install a per-spawn hook) to inject the envelope.

Rejected because:

- Multi-source collision (above) is unsolvable as long as the user might have the global plugin installed.
- Hook framing is not under our control. The `<system-reminder>` prefix `SessionStart hook additional context:` is set by Claude Code; future versions could change it, breaking any prompt template that anchors on it.
- Hook-based injection makes replay harder: reconstructing what the agent saw requires re-running the hook against the historical data store, not just re-reading the envelope.
- Adds a hook install/discovery dependency on the spawn path. A user running `clawket execute` from a clean checkout shouldn't need any plugin set up beyond having `clawket` and `claude` on PATH.

### Alternative 2 — pure Hybrid (both A and B fire on the same spawn)

Let the global hook fire *and* append `--append-system-prompt` for the envelope on top.

Rejected because:

- Doubles the context surface for one task; cache_creation_tokens roughly doubles per `execute`, costing real money on Opus.
- Ordering between hook output and appended system prompt is implementation-defined.
- The dashboard-style dump from the plugin hook is *intentionally not what an executing agent should focus on* — it lists other tasks, recent activity, etc. Surfacing it during `execute` invites the agent to drift.

### Alternative 3 — `--system-prompt` (replace, not append)

Throw away Claude Code's default system prompt entirely and supply our own.

Rejected because:

- We lose all the built-in tool guidance, agent norms, and hook event documentation that ship with Claude Code's default prompt. The envelope is *additive context*, not a replacement for "how to be Claude Code".
- Brittle across Claude Code version upgrades — every default-prompt change would require us to re-author our replacement.

## Implementation contract for LM-149 (`clawket execute`)

LM-149 must, at minimum:

1. Resolve the active envelope for the task (per LM-146's `clawket task envelope show --resolve`).
2. Build the priority-stack payload per ADR-0003 (envelope → ancestors → cycle co-tasks → descendants → recent decisions → similar tasks → recent runs → comments), respecting per-item caps.
3. Spawn `claude -p <prompt-from-envelope.prompt_template> --append-system-prompt <payload> --setting-sources <minimal> --output-format stream-json --include-hook-events --no-session-persistence --max-budget-usd <envelope.cost_cap_usd>` (model and max-turns also derived from envelope).
4. Capture the run record (ADR-0010) including the exact `--append-system-prompt` string used. Replay reads from this.
5. On clawketd unreachable (F1), spawn with envelope-only payload and log a warning to the run record's `warnings` array.

The exact CLI shape, lease handling, and error surfaces are LM-149's job. ADR-0002 only fixes the *injection mechanism*.

## Re-open triggers

This ADR should be revisited if **any** of the following becomes true:

- Claude Code introduces a `setup` field in plugin manifest (or equivalent) that lets the plugin register *which* settings file applies to which spawn — at that point, Strategy A becomes safe for `execute` and may simplify the architecture.
- `--append-system-prompt` adds an effective size cap that is below realistic envelope payloads (we have not stress-tested above ~2 kB; LM-148's evidence used ~50 bytes).
- Empirical evidence emerges that `<system-reminder>`-wrapped content (Strategy A) and direct-system-prompt content (Strategy B) produce *materially different model behaviour* on identical task envelopes. None observed in LM-147; treat as an open watch item.
- A future ADR mandates dynamic, mid-spawn context refresh (e.g. progress reports from a long-running task back into the model context) — that case fundamentally needs the hook channel and would force a re-design.

## References

- ADR-0001 — Execution Envelope (`clawket/docs/adr/0001-execution-envelope.md`)
- ADR-0002 (Evidence) — Headless Hook Empirical Behavior (`cli/docs/adr/0002-hook-injection-strategy-evidence.md`)
- ADR-0003 — Session-Start Strategy (`clawket/docs/adr/0003-session-start-strategy.md`)
- ADR-0010 — Activity Log Retention (`clawket/docs/adr/0010-activity-log-retention.md`)
- LM-146 — `clawket task envelope` subcommand (envelope CRUD that `execute` consumes)
- LM-147 — Headless hook empirical evidence (this ADR's evidence base)
- LM-149 — `clawket execute <task>` (consumer of this decision)
