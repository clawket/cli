# ADR-0002 (Evidence) — Headless Claude Code Hook Injection: Empirical Behavior

| Status | Owner task | Targets | Plan |
|---|---|---|---|
| **Accepted as evidence** (LM-147 / RL-U5-02a, captured 2026-04-28) | LM-147 | Feeds ADR-0002 strategy decision (LM-148) | v11 — Structured Task Contracts |

This document is the empirical companion to **ADR-0002** (LM-148). It does not pick a strategy; it records *what we observed when we actually ran `claude -p` with a SessionStart hook* so that LM-148 can decide between Strategy A (hook-based injection), Strategy B (`--append-system-prompt`), and Hybrid on evidence rather than guess.

## Question under test

> When `clawket execute <task>` spawns `claude -p` headlessly with a SessionStart hook that returns `hookSpecificOutput.additionalContext`, **does that string actually land inside the model's session context** — i.e. can the model read it?

This matters because Strategy A's entire premise is that hook-injected `additionalContext` is *equivalent in visibility* to a system prompt segment. If it isn't, Strategy A is dead and we need `--append-system-prompt`.

## Test harness

`cli/tests/headless_hook.rs` plus a self-contained fixture under `cli/tests/fixtures/headless-hook/`:

- `.claude/hooks/session-start.cjs` — emits a JSON payload with both `hookSpecificOutput.additionalContext` and `systemMessage`, each containing a unique sentinel (`CLAWKET-LM147-9D2F1E7B`) sourced from `CLAWKET_HOOK_SENTINEL` env var.
- `.claude/settings.json` — registers the hook for `SessionStart`.
- Spawn shape: `claude -p ... --model claude-haiku-4-5-20251001 --add-dir <fixture> --settings <fixture>/.claude/settings.json --include-hook-events --output-format stream-json --verbose --max-budget-usd 0.10 --no-session-persistence`, with `current_dir` set to the fixture so `$CLAUDE_PROJECT_DIR` resolves correctly.

Two ignored tests:

1. `hook_runs_and_emits_sentinel` — proves the hook actually fired by grepping the streamed `hook_response` event for the sentinel.
2. `additional_context_lands_in_model_visible_context` — sends a probe asking Claude to echo the 8-char suffix after `CLAWKET-LM147-` from anywhere in its context. A correct echo is positive proof of LLM visibility.

Run with `cargo test --test headless_hook -- --ignored --nocapture` from `cli/`. Both tests passed against `claude` 2.1.121 on 2026-04-28. Raw stream-json captured in `evidence/0002-headless-stream.jsonl` (13 lines, ~24 kB).

## Observation 1 — the hook fires

The fixture's `SessionStart` hook produces a `hook_response` event in the stream-json output:

```json
{"type":"system","subtype":"hook_response",
 "hook_name":"SessionStart:startup","hook_event":"SessionStart",
 "output":"{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",
            \"additionalContext\":\"[hook-additional-context] CLAWKET-LM147-9D2F1E7B\"},
           \"systemMessage\":\"[hook-system-message] CLAWKET-LM147-9D2F1E7B\"}\n",
 "exit_code":0,"outcome":"success"}
```

The sentinel appears 4× in the captured stream (fixture's hook fires twice — once nominally, once as `output`/`stdout` mirror — and then again because the global Clawket plugin's SessionStart hook also fires). In a real `clawket execute` we will spawn into a directory whose only registered hook is the one we want; the global plugin hook does not pollute the contract.

**Conclusion:** Claude Code's `--settings <file>` flag fully respects custom hooks in headless mode and surfaces them in `--include-hook-events --output-format stream-json`.

## Observation 2 — additionalContext is model-visible

Probe:

> "Reply with the exact 8-character token immediately after 'CLAWKET-LM147-' from any line in your context, and NOTHING else."

Result (text mode):

```
9D2F1E7B
```

That string only exists inside the hook's `additionalContext` payload. The model could not have produced it without seeing that field.

**Conclusion:** `hookSpecificOutput.additionalContext` from a SessionStart hook is **placed inside the model's session context** in headless `-p` mode. Strategy A's core premise is verified.

## Observation 3 — *how* additionalContext is exposed (bonus)

The model's thinking trace (captured in stream-json — useful but not load-bearing for the decision) literally says:

> Looking at the system-reminder tags:
>
> From the first system-reminder:
> ```
> SessionStart hook additional context: [hook-additional-context] CLAWKET-LM147-9D2F1E7B
> ```

So Claude sees the hook output **wrapped in a `<system-reminder>` block, prefixed with `SessionStart hook additional context:`**. Three implications:

1. The injected envelope is *not* indistinguishable from the system prompt — Claude can tell it came from a hook and reads it as a reminder.
2. The framing string `SessionStart hook additional context:` is fixed by Claude Code, not by us. We don't control whether it appears.
3. `systemMessage` does *not* show up in the model-visible context (only `additionalContext` did, even though both contained the sentinel). It is a UI-only / log-only slot — confirming the field semantics implied by the Claude Code docs but not previously verified empirically by us.

## Observation 4 — caching behaviour

The probe call reported:

```
cache_creation_input_tokens: 16958
cache_read_input_tokens:     42163
total_cost_usd:              0.0265488   (Haiku 4.5)
```

The hook payload (small) + the implicit Claude Code system prompt (large) are cacheable. A repeated `clawket execute` against the same envelope-shape will hit cache_read on subsequent calls. This is not a decision input for ADR-0002 by itself, but it tells us **Strategy A does not break caching** — the hook output is stable text, hashed identically across runs, and lives in the cacheable prefix.

## Observation 5 — what we did *not* test

- `--append-system-prompt` (Strategy B) was not exercised. LM-148 must run a parallel probe before locking in.
- Larger payloads — our sentinel envelope is ~50 bytes. We do not know whether a 16 kB envelope still gets surfaced verbatim or gets truncated/elided.
- Tool-call paths — only the assistant's text reply was probed. Whether `additionalContext` is visible inside a tool-driven reasoning loop (e.g. the model is calling Bash and reading the envelope mid-loop) is untested.
- Multiple SessionStart hooks — when both fixture and global Clawket hook fire, we observed both payloads in `additionalContext`. Ordering / concatenation rules were not characterised.

LM-148 will fill these gaps with focused probes before declaring Strategy A production-fit.

## Conclusion (for LM-148 to consume)

| Claim | Status |
|---|---|
| SessionStart hooks fire under `claude -p` headless. | **Verified.** |
| `hookSpecificOutput.additionalContext` lands in the model's session context. | **Verified** (model echoed the unique sentinel). |
| `systemMessage` lands in the model's session context. | **Refuted** — model only saw `additionalContext`. Treat `systemMessage` as UI/log-only. |
| Strategy A (hook-based envelope injection) is *technically viable*. | **Yes.** |
| Strategy A is *better than* Strategy B for our case. | **Open** — depends on the gap items in Observation 5 plus Strategy B's own probe. LM-148 decides. |

## Reproducing

```bash
# from cli/
cargo test --test headless_hook fixture_is_well_formed                # CI-safe
cargo test --test headless_hook -- --ignored --nocapture               # spawns claude, ~$0.05
```

Raw stream-json (kept as evidence): `cli/docs/adr/evidence/0002-headless-stream.jsonl`.
