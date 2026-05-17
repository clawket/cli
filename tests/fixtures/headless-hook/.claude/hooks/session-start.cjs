#!/usr/bin/env node
// Test fixture for LM-147 / ADR-0002 evidence:
// emit a sentinel string both as additionalContext (LLM-visible) and as
// systemMessage (logged but not necessarily LLM-visible). The Rust harness
// reads stream-json output to confirm which slot lands inside the model's
// session context.
const sentinel = process.env.CLAWKET_HOOK_SENTINEL || "CLAWKET-HOOK-SENTINEL-DEFAULT";
process.stdout.write(
  JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "SessionStart",
      additionalContext: `[hook-additional-context] ${sentinel}`,
    },
    systemMessage: `[hook-system-message] ${sentinel}`,
  }) + "\n"
);
