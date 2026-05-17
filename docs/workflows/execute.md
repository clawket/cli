# `clawket execute` — 5-minute workflow guide

A practical walkthrough for running a Clawket task as a `claude -p` spawn,
end to end. Every step here uses commands that ship in `clawket` 0.2.6+.

The goal: take a task that already has an active envelope and turn it
into a real Claude Code session — without footguns.

---

## 1. Set the envelope

The envelope is the contract: 19 ADR-0001 fields that decide what the
spawn sees as system prompt and user prompt. You only need a handful in
practice (`intent`, `prompt_template`, `success_criteria`,
`assigned_model`).

```bash
# Start from an existing task; one-shot edit:
clawket task envelope set TASK-ULID-OR-TICKET \
  --field intent="rewrite parser to handle escapes" \
  --field prompt_template="Modify cli/src/parser.rs ..." \
  --field 'success_criteria=["roundtrip test passes","no clippy warnings"]'

# Or open $EDITOR and sign the result as a new version:
clawket task envelope edit LM-XX
```

Each save signs a new version. History is replayable:

```bash
clawket task envelope show LM-XX --version 1
clawket task envelope history LM-XX
```

If the envelope inherits from a parent (unit / plan default), use
`--resolve` to see the merged view that `execute` will actually feed:

```bash
clawket task envelope show LM-XX --resolve
```

---

## 2. Dry-run the prompts before spending tokens

`--dry-run` prints both the assembled `--append-system-prompt` payload
and the `claude -p` argument, then exits. No daemon writes, no spawn,
no lease. Use it to sanity-check that the envelope renders as expected.

```bash
clawket execute LM-XX --dry-run

# Output shape:
#   # === clawket execute dry-run ===
#   # task: TASK-...
#   # model: claude-haiku-4-5
#   # cost-cap (USD): 1.00
#   #
#   # --- --append-system-prompt payload ---
#   <system prompt>
#   # --- claude -p prompt argument ---
#   <user prompt>
```

If the user prompt comes back empty, you forgot `prompt_template` (or
at least `intent`) on the envelope — `execute` would bail. Fix the
envelope and dry-run again until the rendered prompts read like a brief
you'd hand a teammate.

---

## 3. Spawn `claude -p`

Default invocation runs Claude Code headless against the rendered
prompts:

```bash
clawket execute LM-XX
```

Under the hood this:

1. Resolves the active envelope chain (`--resolve` semantics).
2. Runs the drift check (see §5) against the task's `target_repo`.
3. Acquires a session lease on the task (LM-180 / RL-U5-07b).
   A second concurrent `clawket execute` against the same task fails
   with HTTP 409 — your two terminals can't trample each other.
4. Opens a `runs` row (frozen `envelope_snapshot` for replay).
5. Spawns `claude -p` with `--append-system-prompt` + the rendered user
   prompt + the configured `--max-budget-usd`.
6. Closes the run with `succeeded` / `failed` / `cancelled`.
7. Releases the lease.

Common flags:

```bash
clawket execute LM-XX \
  --model stronger \         # haiku→sonnet, sonnet→opus
  --cost-cap 2.00            # USD budget for this spawn
```

To replay an old run byte-for-byte (frozen envelope, ignores drift):

```bash
clawket run list --task LM-XX
clawket execute LM-XX --resume RUN-...
```

---

## 4. Troubleshoot drift (`none` / `minor` / `major`)

When you sign an envelope, Clawket records the repo's `planned_sha`.
On every non-replay execute, the daemon compares it to the current
`target_repo` HEAD and classifies the change set:

| level   | behavior                                                          |
| ------- | ----------------------------------------------------------------- |
| `none`  | silent — happy path                                                |
| `minor` | one-line gray banner on stderr, proceeds                          |
| `major` | **hard-blocks** unless you pass `--trust-stale`                    |

When `major` fires without the flag:

```text
Error: task LM-XX: envelope drift is `major` (drift=major
in_scope=4/12 planned=abc1234 current=def5678). Re-sign the envelope
(`clawket task envelope edit LM-XX`) or opt in with `--trust-stale`
to override and record an audit row.
```

The two clean fixes:

```bash
# A. Re-sign — preferred. Updates planned_sha to current HEAD.
clawket task envelope edit LM-XX   # save with no changes is fine

# B. Override — records an audit row in activity_log.
clawket execute LM-XX --trust-stale
```

`--trust-stale` is intentionally noisy: stderr warns, and a row lands
in `activity_log` (`action=execute_trust_stale`,
`new_value=drift summary`, `actor=clawket-execute`) so the choice is
auditable later. Don't use it as a habit — re-sign instead.

To inspect drift without running anything:

```bash
clawket task view LM-XX        # shows the drift banner
```

---

## 5. Decompose when the task is too big

If the envelope's `success_criteria` has more than ~3 bullets, or the
estimated edits cross your repo budget, split before spawning. Clawket
proposes children from the criteria + decomposition_policy:

```bash
clawket task decompose LM-XX                  # preview only
clawket task decompose LM-XX --dry-run        # preview as JSON
clawket task decompose LM-XX --accept ALL     # create all children
clawket task decompose LM-XX --accept 1,3     # cherry-pick
```

Children inherit the parent's envelope (per ADR-0001 chain). After
decomposing, run each child through §1–4 above. The parent task is
typically marked `done` once all children land — `clawket task list
--parent LM-XX` to track.

If `decompose` returns nothing, the envelope's `success_criteria` is
too coarse to split mechanically; rewrite it as 2–5 testable bullets
and try again.

---

## Quick cheat-sheet

```bash
# 1. set envelope         clawket task envelope edit LM-XX
# 2. dry-run prompts      clawket execute LM-XX --dry-run
# 3. real spawn           clawket execute LM-XX
# 4. drift override       clawket execute LM-XX --trust-stale
# 5. split first          clawket task decompose LM-XX --accept ALL
# replay a run            clawket execute LM-XX --resume RUN-...
# inspect runs            clawket run list --task LM-XX
```

When in doubt: `clawket execute --help`. When something looks weird:
`clawket doctor`.
