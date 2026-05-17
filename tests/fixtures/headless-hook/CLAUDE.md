# Headless hook test fixture

Used by `cli/tests/headless_hook.rs` (LM-147) to empirically verify whether a
SessionStart hook's `additionalContext` lands in the LLM's session context
when running `claude -p` headless.
