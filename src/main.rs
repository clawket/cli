mod client;
mod daemon;
mod daemon_autostart;
mod doctor;
mod doctor_checks;
mod error;
mod init;
mod mcp;
mod paths;
mod verify;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "clawket",
    version,
    about = "LLM-native work management CLI for Claude Code (v3.0).\n\nWorkflow: Project → Plan (approve) → Unit → Cycle (--unit required, activate) → Task\n\nInvariants (v3.0):\n  - One active plan per project. Approve a draft (draft → active) before starting tasks.\n  - Cycles belong to a unit (`cycle create --unit UNIT-…`) and run planning → active → completed.\n  - One active cycle per unit. Completed cycles cannot be restarted — create a new one.\n  - Unit is a pure grouping entity (no status, no approval).\n  - Task is the only entity managed directly: todo → in_progress → done/cancelled (blocked for external dependencies).\n  - Transitioning a task to `done` requires `--evidence` (file:line or reasoning summary).\n\nQuick start:\n  clawket project create \"my-app\" --cwd .\n  clawket plan create --project PROJ-my-app \"MVP\"\n  clawket plan approve PLAN-xxx\n  clawket unit create --plan PLAN-xxx \"Unit 1\"\n  clawket cycle create --project PROJ-my-app --unit UNIT-xxx \"Sprint 1\"\n  clawket cycle activate CYC-xxx\n  clawket task create \"Build login\" --cycle CYC-xxx\n  clawket task update TASK-xxx --status in_progress\n  clawket task complete TASK-xxx --evidence \"src/login.rs:42 — hash verified\""
)]
struct Cli {
    /// Output format: json (default) | table | yaml. Applies to commands
    /// that emit entity payloads. Ignored by commands that emit plain text
    /// or open external views (daemon lifecycle, migrate, update,
    /// completions, mcp, timeline/board/wiki/summary).
    #[arg(long, global = true, default_value = "json")]
    format: String,
    /// Quiet mode. For entity-emitting commands, prints only the entity ID
    /// (no surrounding JSON). On commands without entity output, suppresses
    /// decorative chrome only.
    #[arg(short, long, global = true)]
    quiet: bool,
    /// Disable ANSI color output (also auto-disabled when stdout is not a TTY).
    #[arg(long, global = true)]
    no_color: bool,
    /// Override locale. Supported: en | ko | ja. Propagates to subcommands
    /// and the daemon via the `CLAWKET_LOCALE` env var. Accepts BCP-47 tags.
    #[arg(long, global = true)]
    locale: Option<String>,
    /// Default compute tier (low | med | high) propagated to subcommands
    /// and to subagents spawned through the plugin hooks (env
    /// `CLAWKET_TIER`). `task create`/`task update` consume it as the tier
    /// default; per-subcommand `--tier` overrides it.
    #[arg(long, global = true, value_parser = ["low", "med", "high"])]
    tier: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render the active-project work summary (active plan, units, cycles,
    /// in-progress tasks). Used by Claude Code's SessionStart hook to seed
    /// per-session context, and by humans to ask "where am I?".
    Dashboard {
        /// Working directory to resolve the project from (defaults to cwd).
        #[arg(long)]
        cwd: Option<String>,
        /// Filter: active | next | all (default).
        #[arg(long, default_value = "all")]
        show: String,
    },
    /// Manage the local clawketd daemon (start/stop/status/restart/log).
    #[command(alias = "d")]
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Run the Clawket MCP stdio server (wired into Claude Code via
    /// `.mcp.json`). Exposes five read-only knowledge tools:
    /// `clawket_search_knowledge`, `clawket_search_tasks`,
    /// `clawket_find_similar_tasks`, `clawket_get_task_context`,
    /// `clawket_get_recent_decisions`. Requires the daemon to be running.
    Mcp,
    /// Diagnose the local Clawket installation. v3.0 sections: daemon health,
    /// binaries, path-separation invariant, connectivity, tier distribution,
    /// escalation rate, plugin install, i18n locale chain, skills. Exits
    /// non-zero on any failure.
    Doctor {
        /// Emit results as JSON (machine-readable). Default is human-readable text.
        #[arg(long)]
        json: bool,
        /// Filter tier / escalation sections to tasks belonging to the given plan.
        #[arg(long)]
        plan: Option<String>,
        /// Emit the escalation-rate report: count + percentage of tasks with
        /// a non-null escalation_reason, grouped by tier.
        #[arg(long)]
        escalation: bool,
    },
    /// Post-install smoke check: probe daemon health, then create and delete
    /// a throwaway project to confirm the full write path. `--dry-run` skips
    /// the daemon contact and prints the step list only.
    Verify {
        /// Print the planned steps without contacting the daemon.
        #[arg(long)]
        dry_run: bool,
    },
    /// Onboarding scaffold so a fresh user can close their first task in
    /// about five minutes. With `--tutorial`, creates project + approved
    /// plan + unit + active cycle + first in-progress task in one shot.
    /// Idempotent: re-running on a registered cwd reuses the project.
    Init {
        /// Run the 5-minute onboarding scaffold.
        #[arg(long)]
        tutorial: bool,
        /// Working directory to register as the tutorial project (defaults
        /// to cwd). Created if it does not exist.
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Manage projects (workspaces bound to one or more working directories).
    #[command(alias = "proj")]
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Manage plans (approved intent containers — one active per project).
    #[command(alias = "pl")]
    Plan {
        #[command(subcommand)]
        action: PlanAction,
    },
    /// Manage units (pure grouping inside a plan; no status, no approval).
    #[command(alias = "u")]
    Unit {
        #[command(subcommand)]
        action: UnitAction,
    },
    /// Manage cycles (time-boxed iterations bound to a single unit; one
    /// active cycle per unit).
    #[command(alias = "cy")]
    Cycle {
        #[command(subcommand)]
        action: CycleAction,
    },
    /// Manage tasks (atomic work items — the only entity worked directly).
    #[command(alias = "t")]
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Manage knowledge entries — wiki content the LLM retrieves via MCP.
    Knowledge {
        #[command(subcommand)]
        action: ArtifactAction,
    },
    /// Manage runs (per-task execution records — usually auto-created by hooks).
    #[command(alias = "r")]
    Run {
        #[command(subcommand)]
        action: RunAction,
    },
    /// Manage comments attached to a task, unit, or plan.
    #[command(alias = "c")]
    Comment {
        #[command(subcommand)]
        action: CommentAction,
    },
    /// Manage questions (human clarification / decision requests).
    #[command(alias = "q")]
    Question {
        #[command(subcommand)]
        action: QuestionAction,
    },
    /// Generate shell completion scripts.
    /// Usage: `clawket completions bash >> ~/.bash_completion`
    ///        `clawket completions zsh > ~/.zfunc/_clawket`
    ///        `clawket completions fish > ~/.config/fish/completions/clawket.fish`
    ///        `clawket completions powershell >> $PROFILE`
    ///        `clawket completions elvish > ~/.elvish/lib/clawket.elv`
    Completions {
        /// Shell: bash | zsh | fish | powershell | elvish
        shell: String,
    },

    // ===== Dashboard views =====
    /// Open the Timeline view (chronological task/cycle ribbon) in the web dashboard.
    Timeline {
        /// Project ID (defaults to cwd project)
        #[arg(long)]
        project: Option<String>,
    },

    /// Open the Board view (kanban by task status) in the web dashboard.
    Board {
        /// Project ID (defaults to cwd project)
        #[arg(long)]
        project: Option<String>,
    },

    /// Open the Wiki view (knowledge entries) in the web dashboard.
    Wiki {
        /// Project ID (defaults to cwd project)
        #[arg(long)]
        project: Option<String>,
    },

    /// Open the Summary view (active plan + KPI overview) in the web dashboard.
    Summary {
        /// Project ID (defaults to cwd project)
        #[arg(long)]
        project: Option<String>,
    },

    // ===== Watch =====
    /// Stream live task / cycle / run events from the daemon (Server-Sent
    /// Events). Filter by project, task, or cycle. Runs until Ctrl-C.
    Watch {
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
        /// Filter by task ID
        #[arg(long)]
        task: Option<String>,
        /// Filter by cycle ID
        #[arg(long)]
        cycle: Option<String>,
        /// Output format: text | json (default: text)
        #[arg(long, default_value = "text")]
        format: String,
    },

    // ===== Replay =====
    /// Replay the run history of a task for post-mortem inspection. Prints
    /// each run's start/finish, agent, result, and notes in order.
    Replay {
        /// Task ID to replay runs for
        task: String,
        /// Limit number of runs to replay (default: 10)
        #[arg(long, default_value = "10")]
        limit: u32,
    },

    // ===== Backup / Restore / Migrate =====
    /// Export all Clawket data (DB + attached knowledge entries) to a
    /// portable tar.gz archive for cross-machine transfer or offsite backup.
    Backup {
        /// Output path for the archive (default: ./clawket-backup-<timestamp>.tar.gz)
        #[arg(long)]
        output: Option<String>,
        /// Project ID to back up (default: all projects)
        #[arg(long)]
        project: Option<String>,
    },

    /// Restore Clawket data from a backup archive. Default mode replaces
    /// the current database; `--merge` overlays the archive on top.
    Restore {
        /// Path to the backup archive
        input: String,
        /// Merge into existing data instead of replacing
        #[arg(long)]
        merge: bool,
        /// Preview without applying changes
        #[arg(long)]
        dry_run: bool,
    },

    /// Apply any pending Clawket DB schema migrations. Normally run
    /// automatically by the daemon at startup; this command lets you stage
    /// and inspect them out-of-band.
    Migrate {
        /// Preview pending migrations without applying
        #[arg(long)]
        dry_run: bool,
    },

    // ===== Config =====
    /// Read or write Clawket configuration values stored under
    /// `~/.config/clawket/`. Subcommands: get | set | unset | list.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    // ===== Self-update / version-check =====
    /// Download and install the latest Clawket release from GitHub Releases.
    /// Replaces the local CLI + daemon binaries (atomic swap; existing daemon
    /// process keeps running until next restart).
    Update {
        /// Print what would be downloaded without installing
        #[arg(long)]
        dry_run: bool,
        /// Pin to a specific version (e.g. "v3.1.0")
        #[arg(long)]
        version: Option<String>,
    },

    /// Check whether a newer Clawket version is available without installing
    /// it. Compares the local version against the latest GitHub Release.
    VersionCheck,

    // ===== Knowledge shortcuts =====
    /// Find tasks semantically similar to a query (vector search over task
    /// title + body). Top-level alias for `clawket task search --mode semantic`.
    FindSimilar {
        /// Query text
        query: String,
        /// Maximum results
        #[arg(long, default_value = "10")]
        limit: u32,
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
    },

    /// Get full context for a task — task body, runs, comments, attached
    /// knowledge entries — in one JSON payload, suitable for piping into
    /// an LLM prompt as session restore.
    GetTaskContext {
        /// Task ID
        id: String,
    },

    /// Get recent decision-type knowledge entries (`type=decision`) for a
    /// project, ordered by creation time.
    GetRecentDecisions {
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
        /// Maximum results
        #[arg(long, default_value = "10")]
        limit: u32,
    },

    // ===== Discover-loop =====
    /// Discover-loop automation — round-by-round QA dispatch, TSV evidence
    /// sync, and 3-way convergence query. Subcommands cover plan/cycle/unit
    /// auto-generation, batch dispatch manifests, TSV schema validation,
    /// bulk transcription, and last-2-rounds-zero convergence checks.
    #[command(alias = "dl")]
    DiscoverLoop {
        #[command(subcommand)]
        action: DiscoverAction,
    },
}

// ========== Daemon ==========
#[derive(Subcommand)]
pub enum DaemonAction {
    /// Start the clawketd daemon in the background (HTTP on localhost:19400 + Unix socket)
    Start,
    /// Stop the running clawketd daemon
    Stop,
    /// Show daemon status (PID, uptime, version)
    Status,
    /// Restart the daemon (stop + start)
    Restart,
    /// Show or tail daemon logs from `~/.local/state/clawket/`.
    Log {
        /// Number of recent lines to show (default: 50)
        #[arg(long, default_value = "50")]
        lines: u32,
        /// Follow/tail log output in real time
        #[arg(long, short = 'f')]
        follow: bool,
    },
}

// ========== Project ==========
#[derive(Subcommand)]
enum ProjectAction {
    /// Create a new project. Each project maps to one or more working directories.
    Create {
        /// Project name (used to generate ID: PROJ-<slugified-name>)
        name: String,
        /// Project description
        #[arg(long)]
        description: Option<String>,
        /// Working directory to associate (defaults to current dir)
        #[arg(long)]
        cwd: Option<String>,
        /// Short uppercase key for ticket numbers (e.g. APP → APP-1, APP-2)
        #[arg(long)]
        key: Option<String>,
    },
    /// View project details by ID
    View {
        /// Project ID
        id: String,
    },
    /// List all projects
    List,
    /// Update project properties
    Update {
        /// Project ID
        id: String,
        /// New project name
        #[arg(long)]
        name: Option<String>,
        /// New project description
        #[arg(long)]
        description: Option<String>,
        /// Wiki root paths as JSON array, e.g. '["docs","wiki","/absolute/path"]'
        #[arg(long)]
        wiki_paths: Option<String>,
    },
    /// Delete a project and all associated data
    Delete {
        /// Project ID
        id: String,
    },
    /// Disable Clawket hook enforcement for this project (sets enabled=0).
    /// While disabled, PreToolUse / UserPromptSubmit / ExitPlanMode hooks
    /// noop and Claude Code can edit without an active task. Same toggle
    /// as the dashboard's Project Settings → enabled switch.
    Disable {
        /// Project ID (or ticket key prefix, e.g. PROJ-...)
        id: String,
    },
    /// Re-enable Clawket hook enforcement for this project (sets enabled=1).
    Enable {
        /// Project ID
        id: String,
    },
    /// Resolve the project registered for a working directory. Returns the
    /// full project JSON (including `enabled`) so hook adapters can decide
    /// whether to enforce. Exits 0 with `null` when no project matches.
    Resolve {
        /// Working directory (defaults to current dir)
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Manage working directories for a project
    Cwd {
        #[command(subcommand)]
        action: ProjectCwdAction,
    },
}

#[derive(Subcommand)]
enum ProjectCwdAction {
    /// Add a working directory to the project
    Add {
        /// Project ID
        id: String,
        /// Directory path to add (defaults to current dir)
        #[arg(long)]
        path: Option<String>,
    },
    /// Remove a working directory from the project
    Remove {
        /// Project ID
        id: String,
        /// Directory path to remove
        #[arg(long)]
        path: String,
    },
    /// List working directories for a project
    List {
        /// Project ID
        id: String,
    },
}

// ========== Plan ==========
#[derive(Subcommand)]
enum PlanAction {
    /// Create a new plan. Plans start as 'draft' and must be approved before work can begin.
    /// Tasks can be created under draft plans (as todo) but cannot be started (in_progress).
    Create {
        /// Plan title
        title: String,
        /// Project ID this plan belongs to
        #[arg(long)]
        project: String,
        /// Plan description
        #[arg(long)]
        description: Option<String>,
        /// Source: manual (default) or import
        #[arg(long, default_value = "manual")]
        source: String,
        /// Source file path (for imported plans)
        #[arg(long)]
        source_path: Option<String>,
    },
    /// View plan details
    View {
        /// Plan ID
        id: String,
    },
    /// List plans with optional filters
    List {
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
        /// Filter by status: draft, active, completed
        #[arg(long)]
        status: Option<String>,
    },
    /// Update plan properties. Status: draft, active, completed
    Update {
        /// Plan ID
        id: String,
        /// New plan title
        #[arg(long)]
        title: Option<String>,
        /// New plan description
        #[arg(long)]
        description: Option<String>,
        /// Plan status: draft, active, completed. Use 'approve' command for draft → active.
        #[arg(long)]
        status: Option<String>,
    },
    /// Delete a plan
    Delete {
        /// Plan ID
        id: String,
    },
    /// Approve a draft plan (draft → active). Required before tasks can be started.
    Approve {
        /// Plan ID
        id: String,
    },
    /// Mark an active plan as completed (active → completed)
    Complete {
        /// Plan ID
        id: String,
    },
    /// Import a plan from a markdown file
    Import {
        /// Path to plan markdown file
        file: String,
        /// Project name to attach the plan to (created if missing)
        #[arg(long)]
        project: Option<String>,
        /// Working directory (used when project is omitted)
        #[arg(long)]
        cwd: Option<String>,
        /// Source label recorded on the plan (default: "import")
        #[arg(long, default_value = "import")]
        source: String,
        /// Preview the parsed plan without creating entities
        #[arg(long)]
        dry_run: bool,
        /// Use the strict 19-field envelope parser. Required for round-trip
        /// parity with `plan export --format md`. Loose-mode parsing (the
        /// default) ignores envelope bullets and dependency graphs; strict
        /// mode validates them line-by-line and persists `task_envelopes` +
        /// `task_depends_on` rows.
        #[arg(long)]
        strict: bool,
    },
    /// Export a plan as markdown / json / yaml. The DB is the single source
    /// of truth; markdown is a generated view, not a hand-edited document.
    /// Use this to regenerate `plans/*.md` snapshots after envelope edits,
    /// or to feed a plan into another tool.
    Export {
        /// Plan ID (PLAN-...)
        id: String,
        /// Output format: `md` (markdown, default) | `json` (canonical
        /// structured export) | `yaml` (same shape as json, yaml syntax)
        #[arg(long, default_value = "md")]
        format: String,
        /// Write to FILE instead of stdout. Parent dirs are created.
        #[arg(long)]
        output: Option<String>,
        /// Include knowledge entries attached to the plan in an appendix.
        /// Off by default — keeps the export size predictable.
        #[arg(long)]
        include_knowledge: bool,
    },
}

// ========== Unit ==========
#[derive(Subcommand)]
enum UnitAction {
    /// Create a new unit (grouping entity within a plan).
    /// Tasks in a parallel unit can be executed by multiple agents simultaneously.
    Create {
        /// Unit title
        title: String,
        /// Plan ID this unit belongs to
        #[arg(long)]
        plan: String,
        /// Unit goal description
        #[arg(long)]
        goal: Option<String>,
        /// Sort order within plan
        #[arg(long)]
        idx: Option<i64>,
        /// Execution mode: sequential (default) or parallel (multi-agent)
        #[arg(long, default_value = "sequential")]
        mode: String,
    },
    /// View unit details
    View {
        /// Unit ID
        id: String,
    },
    /// List units with optional filters
    List {
        /// Filter by plan ID
        #[arg(long)]
        plan: Option<String>,
    },
    /// Update unit properties
    Update {
        /// Unit ID
        id: String,
        /// New unit title
        #[arg(long)]
        title: Option<String>,
        /// Unit goal description
        #[arg(long)]
        goal: Option<String>,
        /// Execution mode: sequential or parallel (multi-agent)
        #[arg(long)]
        mode: Option<String>,
    },
    /// Delete a unit
    Delete {
        /// Unit ID
        id: String,
    },
}

// ========== Cycle ==========
#[derive(Subcommand)]
enum CycleAction {
    /// Create a new cycle (sprint). Starts in `planning` status. In v3.0
    /// every cycle belongs to exactly one unit (`--unit` is required), and
    /// each unit has at most one active cycle at a time.
    Create {
        /// Cycle title (e.g. "Sprint 1", "Hardening Cycle")
        title: String,
        /// Project ID this cycle belongs to
        #[arg(long)]
        project: String,
        /// Unit ID this cycle belongs to (required: every cycle must belong to exactly one unit).
        #[arg(long)]
        unit: String,
        /// Sprint goal
        #[arg(long)]
        goal: Option<String>,
        /// Sort order
        #[arg(long)]
        idx: Option<i64>,
    },
    /// View cycle details
    View {
        /// Cycle ID
        id: String,
    },
    /// List cycles with optional filters
    List {
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
        /// Filter by status: planning, active, completed
        #[arg(long)]
        status: Option<String>,
    },
    /// Update cycle properties. Status: planning, active, completed.
    /// Completed cycles cannot be restarted — create a new cycle instead.
    Update {
        /// Cycle ID
        id: String,
        /// New cycle title
        #[arg(long)]
        title: Option<String>,
        /// Sprint goal
        #[arg(long)]
        goal: Option<String>,
        /// Status: planning, active, completed
        #[arg(long)]
        status: Option<String>,
    },
    /// Delete a cycle (unassigns all tasks)
    Delete {
        /// Cycle ID
        id: String,
    },
    /// Activate a planning cycle (planning → active). Required before tasks can be started.
    Activate {
        /// Cycle ID
        id: String,
    },
    /// Mark an active cycle as completed (active → completed). Cannot be restarted afterwards.
    Complete {
        /// Cycle ID
        id: String,
    },
}

// ========== Task ==========
#[derive(Subcommand)]
enum TaskAction {
    /// Create a new task (atomic work unit). Unit and cycle are auto-inferred from the active plan/cycle if omitted.
    /// Status: todo → in_progress → done/cancelled. Blocked for external dependencies.
    Create {
        /// Task title describing the work
        title: String,
        /// Unit ID (auto-inferred from active plan if omitted)
        #[arg(long)]
        unit: Option<String>,
        /// Detailed description (markdown supported)
        #[arg(long, allow_hyphen_values = true)]
        body: Option<String>,
        /// Agent or person responsible (e.g. "main", "sub-agent-1")
        #[arg(long)]
        assignee: Option<String>,
        /// Sort order within unit
        #[arg(long)]
        idx: Option<i64>,
        /// Comma-separated task IDs this task depends on
        #[arg(long, value_delimiter = ',')]
        depends_on: Vec<String>,
        /// Parent task ID for sub-tasks
        #[arg(long)]
        parent_task: Option<String>,
        /// Priority: critical, high, medium, low
        #[arg(long, default_value = "medium")]
        priority: String,
        /// Complexity estimate (freeform, e.g. "high", "3 files")
        #[arg(long)]
        complexity: Option<String>,
        /// Estimated number of file edits
        #[arg(long)]
        estimated_edits: Option<i64>,
        /// Cycle ID. Auto-inferred from active cycle if omitted. Tasks without a cycle go to backlog.
        #[arg(long)]
        cycle: Option<String>,
        /// Task type: task, bug, feature, enhancement, refactor, docs, test, chore
        #[arg(long, default_value = "task")]
        r#type: String,
        /// Comma-separated labels for categorization (e.g. "ui,perf,follow-up").
        #[arg(long, value_delimiter = ',')]
        label: Vec<String>,
        /// Compute tier for this task: low | med | high. Overrides the global `--tier` for this command only.
        #[arg(long, value_parser = ["low", "med", "high"])]
        tier: Option<String>,
        /// Scenario ID this task maps to (e.g. "US-CLAWKET-CLI-001"). Used
        /// to link discover-loop scenarios to executed tasks.
        #[arg(long)]
        scenario_id: Option<String>,
        /// Evidence for the task result: file:line reference or free-text
        /// reasoning summary. Required when transitioning a task to `done`
        /// — the daemon enforces EVIDENCE_REQUIRED.
        #[arg(long, allow_hyphen_values = true)]
        evidence: Option<String>,
        /// Batch ID grouping tasks from the same sub-agent dispatch.
        /// Format: BATCH-<26-char ULID>.
        #[arg(long)]
        batch_id: Option<String>,
    },
    /// View task details by ID
    View {
        /// Task ID (TASK-ULID or ticket number like CK-285)
        id: String,
    },
    /// List tasks with optional filters
    List {
        /// Filter by unit ID
        #[arg(long)]
        unit: Option<String>,
        /// Filter by plan ID
        #[arg(long)]
        plan: Option<String>,
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
        /// Filter by status: todo, in_progress, blocked, done, cancelled
        #[arg(long)]
        status: Option<String>,
        /// Filter by Claude Code agent_id (from SubagentStart hook)
        #[arg(long)]
        agent_id: Option<String>,
        /// Filter by cycle ID (conflicts with --no-cycle)
        #[arg(long, conflicts_with = "no_cycle")]
        cycle: Option<String>,
        /// Only tasks with no cycle assigned (backlog)
        #[arg(long, conflicts_with = "cycle")]
        no_cycle: bool,
        /// Filter by label (e.g. "ui", "perf").
        #[arg(long)]
        label: Option<String>,
        /// Filter by compute tier. Accepts low|med|high (and G1|G2|G3
        /// aliases used by tier-aware policy docs).
        #[arg(long, value_parser = ["low", "med", "high", "G1", "G2", "G3", "g1", "g2", "g3"])]
        tier: Option<String>,
        /// Filter by scenario ID. Returns tasks linked to the given scenario.
        #[arg(long)]
        scenario_id: Option<String>,
        /// Filter by batch ID. Returns tasks from the same sub-agent dispatch batch.
        #[arg(long)]
        batch_id: Option<String>,
        /// Return only tasks whose `evidence` field is NULL or empty.
        /// Useful for spotting tasks that closed without an evidence trail.
        #[arg(long)]
        evidence_empty: bool,
        /// Maximum number of rows to return.
        #[arg(long)]
        limit: Option<i64>,
        /// Number of rows to skip before returning.
        #[arg(long)]
        offset: Option<i64>,
    },
    /// Update task fields. Status values: todo, in_progress, blocked, done, cancelled. Pass empty string ("") to --cycle to detach (move to backlog).
    Update {
        /// Task ID (TASK-ULID or ticket number like CK-285)
        id: String,
        /// New task title
        #[arg(long)]
        title: Option<String>,
        /// Replace task body (markdown supported). Pass "" to clear.
        #[arg(long, allow_hyphen_values = true)]
        body: Option<String>,
        /// Status: todo, in_progress, blocked, done, cancelled
        #[arg(long)]
        status: Option<String>,
        /// Agent or person responsible
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long, env = "CLAWKET_SESSION_ID", hide = true)]
        session_id: Option<String>,
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main", hide = true)]
        agent: String,
        /// Priority: critical, high, medium, low
        #[arg(long)]
        priority: Option<String>,
        /// Complexity estimate (freeform, e.g. "high", "3 files")
        #[arg(long)]
        complexity: Option<String>,
        /// Estimated number of file edits
        #[arg(long)]
        estimated_edits: Option<i64>,
        /// Parent task ID (for sub-tasks)
        #[arg(long)]
        parent_task: Option<String>,
        /// Cycle ID. Pass "" to detach and move task to backlog.
        #[arg(long)]
        cycle: Option<String>,
        /// Claude Code agent_id (from SubagentStart hook)
        #[arg(long, env = "CLAWKET_AGENT_ID", hide = true)]
        agent_id: Option<String>,
        /// Add a comment along with the update
        #[arg(long, allow_hyphen_values = true)]
        comment: Option<String>,
        /// Compute tier for this task: low | med | high. Overrides the global `--tier` for this command only.
        #[arg(long, value_parser = ["low", "med", "high"])]
        tier: Option<String>,
        /// Scenario ID this task maps to (e.g. "US-CLAWKET-CLI-001"). Used
        /// to link discover-loop scenarios to executed tasks.
        #[arg(long)]
        scenario_id: Option<String>,
        /// Evidence for the task result: file:line reference or free-text
        /// reasoning summary. Required when transitioning a task to `done`
        /// — the daemon enforces EVIDENCE_REQUIRED.
        #[arg(long, allow_hyphen_values = true)]
        evidence: Option<String>,
        /// Batch ID grouping tasks from the same sub-agent dispatch.
        /// Format: BATCH-<26-char ULID>.
        #[arg(long)]
        batch_id: Option<String>,
    },
    /// Delete a task (only allowed when its plan is still in draft)
    Delete {
        /// Task ID to delete
        id: String,
    },
    /// Append text to a task's body (does not replace existing content)
    AppendBody {
        /// Task ID
        id: String,
        /// Text to append to the body (positional)
        #[arg(allow_hyphen_values = true)]
        text: String,
    },
    /// Search tasks (FTS5 keyword / vector semantic / hybrid) across title and body
    Search {
        /// Search query
        query: String,
        /// Search mode: keyword | semantic | hybrid
        #[arg(long, default_value = "keyword")]
        mode: String,
        /// Maximum number of results
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Mark a task as done (shortcut for `task update --status done
    /// --evidence …`). The daemon enforces EVIDENCE_REQUIRED on the done
    /// transition, so `--evidence` is mandatory here.
    Complete {
        /// Task ID (TASK-ULID or ticket number)
        id: String,
        /// Evidence for the result: file:line reference or free-text
        /// reasoning summary. Required — daemon rejects done without it.
        #[arg(long, allow_hyphen_values = true)]
        evidence: String,
        /// Optional comment recorded alongside the status change
        #[arg(long, allow_hyphen_values = true)]
        comment: Option<String>,
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main", hide = true)]
        agent: String,
    },
    /// Cancel a task (alias for `task update --status cancelled`)
    Cancel {
        /// Task ID (TASK-ULID or ticket number)
        id: String,
        /// Reason captured as a comment
        #[arg(long, allow_hyphen_values = true)]
        reason: Option<String>,
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main", hide = true)]
        agent: String,
    },
    /// Block a task on an external dependency (alias for `task update --status blocked`)
    Block {
        /// Task ID (TASK-ULID or ticket number)
        id: String,
        /// Blocker description captured as a comment
        #[arg(long, allow_hyphen_values = true)]
        reason: Option<String>,
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main", hide = true)]
        agent: String,
    },
    /// Unblock a blocked task back to todo (alias for `task update --status todo`)
    Unblock {
        /// Task ID (TASK-ULID or ticket number)
        id: String,
        #[arg(long, allow_hyphen_values = true)]
        comment: Option<String>,
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main", hide = true)]
        agent: String,
    },
    // TaskAction::Envelope removed in v3.0 (FIX-CLI-009 / breaking change).
    // Envelope management is now handled directly via `PATCH /tasks/:id`
    // with the envelope JSON body. See daemon API docs for v3 envelope spec.
    /// Propose sub-tasks from the envelope's `success_criteria` and
    /// `decomposition_policy`. By default prints a numbered preview; pass
    /// `--accept ALL` or `--accept 1,3` to actually create child tasks.
    /// `--dry-run` prints the preview as JSON.
    Decompose {
        /// Parent task ID (TASK-ULID or ticket number like LM-79)
        id: String,
        /// Decomposition depth budget. Compared against envelope's `decomposition_policy.max_depth`.
        #[arg(long, default_value = "1")]
        max_depth: u32,
        /// Strategy hint annotated on each suggestion: auto | by-repo | scoped
        #[arg(long, default_value = "auto")]
        strategy: String,
        /// Print suggestions + violations as JSON without creating tasks
        #[arg(long)]
        dry_run: bool,
        /// Create sub-tasks. Pass `ALL` to accept everything, or comma-separated 1-based
        /// indices (e.g. `1,3,5`). Without this flag, the command only previews.
        #[arg(long)]
        accept: Option<String>,
    },
    /// Render the task subtree (root + descendants) as a unicode tree. Pulls
    /// from `/tasks/{id}/subtree` (DFS order). `--envelope-summary` annotates
    /// each line with the resolved envelope's `intent`.
    Tree {
        /// Root task ID (TASK-ULID or ticket number like LM-80)
        id: String,
        /// Maximum subtree depth to fetch. Caps at the daemon's TREE_NODE_CAP regardless.
        #[arg(long, default_value = "10")]
        depth: u32,
        /// Append `· <intent>` from the resolved envelope to each node line.
        #[arg(long)]
        envelope_summary: bool,
        /// Output format: tree (default unicode) | json (raw subtree array)
        #[arg(long, default_value = "tree")]
        format: String,
    },
    /// List ancestor tasks (parent chain) as a flat array. Designed for
    /// scripting (`--format json | jq`). `--depth N` limits chain length.
    Ancestors {
        /// Task ID (TASK-ULID or ticket number)
        id: String,
        /// Maximum chain depth (0 = no limit, capped at daemon's TREE_NODE_CAP).
        #[arg(long, default_value = "0")]
        depth: u32,
        /// Skip resolved_envelope per node (cheaper for big chains).
        #[arg(long)]
        no_envelope: bool,
        /// Output format: json (default, pipeable) | yaml | table
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Aggregate qa_status histogram for a sub-agent batch.
    ///
    /// Returns `{batch_id, total, pass, defect, scenario_error}` JSON.
    /// Useful for spotting attention dispersion in late tasks of a large batch.
    Stats {
        /// Sub-agent batch ID (26-char Crockford base32 ULID)
        #[arg(long)]
        batch_id: String,
    },
    /// List descendant tasks as a flat array. `--order bfs` switches from
    /// DFS to breadth-first traversal. Designed for scripting.
    Descendants {
        /// Task ID (TASK-ULID or ticket number)
        id: String,
        /// Maximum subtree depth.
        #[arg(long, default_value = "10")]
        depth: u32,
        /// Traversal order: dfs (default) | bfs
        #[arg(long, default_value = "dfs")]
        order: String,
        /// Skip resolved_envelope per node (cheaper for big subtrees).
        #[arg(long)]
        no_envelope: bool,
        /// Output format: json (default, pipeable) | yaml | table
        #[arg(long, default_value = "json")]
        format: String,
    },
}

// ========== Artifact ==========
#[derive(Subcommand)]
enum ArtifactAction {
    /// Create a knowledge entry (document, decision, reference). Attach to at least one of task/unit/plan.
    Create {
        /// Knowledge title
        title: String,
        /// Knowledge type: doc, decision, reference, note, spec
        #[arg(long)]
        r#type: String,
        /// Attach to task ID
        #[arg(long)]
        task: Option<String>,
        /// Attach to unit ID
        #[arg(long)]
        unit: Option<String>,
        /// Attach to plan ID
        #[arg(long)]
        plan: Option<String>,
        /// Knowledge content (markdown)
        #[arg(long, allow_hyphen_values = true)]
        content: Option<String>,
        /// Content format: md (default), txt, code
        #[arg(long, default_value = "md")]
        content_format: String,
        /// Parent knowledge ID (for hierarchical wiki structure)
        #[arg(long)]
        parent: Option<String>,
    },
    /// View a knowledge entry by ID
    View {
        /// Knowledge ID
        id: String,
    },
    /// Update knowledge fields (title, content, type)
    Update {
        /// Knowledge ID
        id: String,
        /// New title
        #[arg(long)]
        title: Option<String>,
        /// Replace content (markdown). Pass "" to clear
        #[arg(long, allow_hyphen_values = true)]
        content: Option<String>,
        /// Content format: md | txt | code
        #[arg(long)]
        content_format: Option<String>,
        /// Author for this change (audit trail)
        #[arg(long)]
        created_by: Option<String>,
    },
    /// List knowledge entries with optional filters
    List {
        /// Filter by task ID
        #[arg(long)]
        task: Option<String>,
        /// Filter by unit ID
        #[arg(long)]
        unit: Option<String>,
        /// Filter by plan ID
        #[arg(long)]
        plan: Option<String>,
        /// Filter by type
        #[arg(long)]
        r#type: Option<String>,
    },
    /// Delete a knowledge entry by ID
    Delete {
        /// Knowledge ID
        id: String,
    },
    /// Search wiki knowledge entries (FTS5 + vector hybrid)
    Search {
        /// Search query
        query: String,
        /// Search mode: keyword | semantic | hybrid
        #[arg(long, default_value = "hybrid")]
        mode: String,
        /// Filter by knowledge type: doc, decision, reference, note, spec
        #[arg(long)]
        r#type: Option<String>,
        /// Maximum number of results
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Import docs/ files as knowledge entries
    Import {
        /// Working directory to scan docs/ from
        #[arg(long)]
        cwd: String,
        /// Attach imported entries to this plan
        #[arg(long)]
        plan: Option<String>,
        /// Attach imported entries to this unit
        #[arg(long)]
        unit: Option<String>,
        /// Preview without creating
        #[arg(long)]
        dry_run: bool,
    },
    /// Export knowledge entries to docs/ directory
    Export {
        /// Target working directory (writes to <cwd>/docs/)
        #[arg(long)]
        cwd: String,
        /// Export only entries attached to this plan
        #[arg(long)]
        plan: Option<String>,
        /// Export only entries attached to this unit
        #[arg(long)]
        unit: Option<String>,
    },
}

// ========== Run ==========
#[derive(Subcommand)]
enum RunAction {
    /// Start a run record for a task (usually auto-created by hooks on task start)
    Start {
        /// Task ID to start a run for
        task: String,
        /// Claude Code session ID (internal, from hook)
        #[arg(long, env = "CLAWKET_SESSION_ID", hide = true)]
        session_id: Option<String>,
        /// Agent executing the run
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main")]
        agent: String,
    },
    /// Finish an active run with a result
    Finish {
        /// Run ID
        id: String,
        /// Result: success | failure | cancelled
        result: String,
        /// Free-form notes about the run outcome
        #[arg(long, allow_hyphen_values = true)]
        notes: Option<String>,
    },
    /// View run details by ID
    View {
        /// Run ID
        id: String,
    },
    /// List runs with optional filters
    List {
        /// Filter by task ID
        #[arg(long)]
        task: Option<String>,
        /// Filter by session ID
        #[arg(long, env = "CLAWKET_SESSION_ID", hide = true)]
        session_id: Option<String>,
    },
}

// ========== Question ==========
#[derive(Subcommand)]
enum QuestionAction {
    /// Create a question requesting human clarification or decision
    Create {
        /// Question body
        body: String,
        /// Attach to plan ID
        #[arg(long)]
        plan: Option<String>,
        /// Attach to unit ID
        #[arg(long)]
        unit: Option<String>,
        /// Attach to task ID
        #[arg(long)]
        task: Option<String>,
        /// Question kind: clarification | decision | blocker
        #[arg(long, default_value = "clarification")]
        kind: String,
        /// Origin of the question: prompt | plan | review
        #[arg(long, default_value = "prompt")]
        origin: String,
        /// Who asked the question
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main")]
        asked_by: String,
    },
    /// Answer an open question
    Answer {
        /// Question ID
        id: String,
        /// Answer text (positional)
        #[arg(allow_hyphen_values = true)]
        text: String,
        /// Who answered: human | main | <agent-name>
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "human")]
        by: String,
    },
    /// View question details by ID
    View {
        /// Question ID
        id: String,
    },
    /// List questions with optional filters
    List {
        /// Filter by plan ID
        #[arg(long)]
        plan: Option<String>,
        /// Filter by unit ID
        #[arg(long)]
        unit: Option<String>,
        /// Filter by task ID
        #[arg(long)]
        task: Option<String>,
        /// If true, show only unanswered questions
        #[arg(long)]
        pending: Option<bool>,
    },
}

// ========== Comment ==========
#[derive(Subcommand)]
enum CommentAction {
    /// Add a comment to a task, unit, or plan.
    Create {
        /// Comment body (markdown supported)
        body: String,
        /// Attach to task ID
        #[arg(long)]
        task: Option<String>,
        /// Attach to unit ID
        #[arg(long)]
        unit: Option<String>,
        /// Attach to plan ID
        #[arg(long)]
        plan: Option<String>,
        /// Comment author (defaults to "main")
        #[arg(long, env = "CLAWKET_AGENT_NAME", default_value = "main")]
        author: String,
        /// Optional label for categorizing the comment
        #[arg(long)]
        label: Option<String>,
    },
    /// List comments for a task, unit, or plan
    List {
        /// Filter by task ID
        #[arg(long)]
        task: Option<String>,
        /// Filter by unit ID
        #[arg(long)]
        unit: Option<String>,
        /// Filter by plan ID
        #[arg(long)]
        plan: Option<String>,
    },
    /// Delete a comment by ID
    Delete {
        /// Comment ID
        id: String,
    },
    /// Update an existing comment body
    Update {
        /// Comment ID
        id: String,
        /// New body (markdown supported)
        #[arg(allow_hyphen_values = true)]
        body: String,
    },
}

// ========== Config ==========
#[derive(Subcommand)]
enum ConfigAction {
    /// Read a configuration value by key.
    Get {
        /// Configuration key (e.g. "default_project", "daemon.port")
        key: String,
    },
    /// Write a configuration value.
    Set {
        /// Configuration key
        key: String,
        /// Value to set
        value: String,
    },
    /// Remove a configuration key.
    Unset {
        /// Configuration key
        key: String,
    },
    /// List all configuration keys and their current values.
    List,
}

// ========== Discover-loop ==========
#[derive(Subcommand)]
enum DiscoverAction {
    // ---- A. Plan/cycle/unit auto-generation ----
    /// Auto-generate a round plan (draft → active) + active cycle + QA
    /// units in one call. Plan title: "<domain> Round <round>". Units:
    /// "QA-<domain> <area>" with mode=parallel. The cycle is anchored to
    /// the first unit; cross-unit tasks are allowed.
    Start {
        /// Project ID
        #[arg(long)]
        project: String,
        /// Domain name (e.g. "Dogfood", "Chess 학습")
        #[arg(long)]
        domain: String,
        /// Round number (1-based)
        #[arg(long)]
        round: u32,
        /// QA unit area names (comma-separated, e.g. "대시보드,CLI,Daemon")
        #[arg(long, value_delimiter = ',')]
        areas: Vec<String>,
        /// Optional plan description
        #[arg(long, allow_hyphen_values = true)]
        description: Option<String>,
    },

    /// Auto-create the next round from a previous plan. Domain and areas
    /// are inferred from the previous plan; round = prev+1.
    NextRound {
        /// Previous plan ID to infer domain, areas, project, and round number from
        #[arg(long)]
        previous_plan: String,
        /// Override domain (inferred from previous plan title if omitted)
        #[arg(long)]
        domain: Option<String>,
        /// Override unit areas (comma-separated; inferred from previous units if omitted)
        #[arg(long, value_delimiter = ',')]
        areas: Option<Vec<String>>,
        /// Override round number (inferred as prev+1 if omitted)
        #[arg(long)]
        round: Option<u32>,
    },

    // ---- B. Dispatch metadata + TSV schema validation ----
    /// Output a batch dispatch manifest for a plan's units. Reads scenario
    /// knowledge counts and generates BATCH-<ULID> identifiers. Warns if any
    /// unit exceeds the batch_size cap.
    DispatchPlan {
        /// Plan ID to build manifest for
        #[arg(long)]
        plan: String,
        /// Max scenarios per sub-agent batch (default: 30).
        #[arg(long, default_value = "30")]
        batch_size: u32,
    },

    /// Validate a TSV evidence file against the 6-field schema.
    /// Schema: scenario_id<TAB>status<TAB>reasoning<TAB>evidence<TAB>tier_used<TAB>batch_id.
    /// Pure validation — does NOT write to the database.
    VerifyTsv {
        /// Path to the TSV file to validate
        path: String,
    },

    /// Generate a fresh BATCH-<ULID> identifier for tagging TSV evidence rows.
    BatchId,

    // ---- C. Bulk sync transcription ----
    /// Transcribe TSV evidence rows into Clawket tasks. Pure transcription —
    /// no reasoning inside sync. Status mapping: pass→done, defect→blocked,
    /// scenario_error→cancelled. Idempotent: existing tasks (same
    /// scenario_id + cycle_id) are updated rather than duplicated.
    Sync {
        /// Path to the TSV evidence file
        path: String,
        /// Target unit ID for new tasks
        #[arg(long)]
        unit: String,
        /// Target cycle ID (must be active)
        #[arg(long)]
        cycle: String,
        /// Assignee label for created tasks (default: discover-loop)
        #[arg(long)]
        assignee: Option<String>,
    },

    // ---- D. 3-way convergence query ----
    /// Show defect / scenario_error / pass counts for the active round,
    /// with regression detection against the previous round.
    Status {
        /// Plan ID (or supply --project to use the active plan)
        #[arg(long)]
        plan: Option<String>,
        /// Project ID (uses active plan for the project)
        #[arg(long)]
        project: Option<String>,
    },

    /// Check the last-two-rounds-zero convergence condition.
    /// Exits 0 when defect=0 + scenario_error=0 holds for 2 consecutive
    /// rounds. Exits 1 when not yet converged.
    Converged {
        /// Plan ID (or supply --project to use the active plan)
        #[arg(long)]
        plan: Option<String>,
        /// Project ID (uses active plan for the project)
        #[arg(long)]
        project: Option<String>,
    },

    /// List all Round-N plans for a project with per-round task counts.
    /// Useful for visualising the monotone-decrease convergence graph.
    Rounds {
        /// Project ID
        project: String,
    },
}

fn strip_nulls(val: &serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::Object(map) => {
            let filtered: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(strip_nulls).collect())
        }
        other => other.clone(),
    }
}

fn output_fmt(val: &serde_json::Value, format: &str) {
    match format {
        "table" => print_table(val),
        "yaml" => print_yaml(val, 0),
        "json" => println!("{}", serde_json::to_string(&strip_nulls(val)).unwrap()),
        other => {
            eprintln!("ERROR: unknown --format '{other}'. Choose: json | table | yaml");
            std::process::exit(2);
        }
    }
}

fn print_table(val: &serde_json::Value) {
    match val {
        serde_json::Value::Array(arr) if !arr.is_empty() => {
            if let Some(first) = arr[0].as_object() {
                let keys: Vec<&String> = first.keys().collect();
                // Filter out long fields
                let visible: Vec<&&String> = keys
                    .iter()
                    .filter(|k| !["body", "content", "depends_on"].contains(&k.as_str()))
                    .collect();
                let headers: Vec<&str> = visible.iter().map(|k| k.as_str()).collect();
                let rows: Vec<Vec<String>> = arr
                    .iter()
                    .map(|item| {
                        visible
                            .iter()
                            .map(|k| {
                                let v = item.get(k.as_str()).unwrap_or(&serde_json::Value::Null);
                                let s = match v {
                                    serde_json::Value::Null => String::new(),
                                    serde_json::Value::String(s) => s.clone(),
                                    serde_json::Value::Number(n) => n.to_string(),
                                    serde_json::Value::Bool(b) => b.to_string(),
                                    _ => serde_json::to_string(v).unwrap_or_default(),
                                };
                                if s.chars().count() > 50 {
                                    let truncated: String = s.chars().take(47).collect();
                                    format!("{}...", truncated)
                                } else {
                                    s
                                }
                            })
                            .collect()
                    })
                    .collect();
                // Compute widths (use Unicode display width for CJK chars)
                fn display_width(s: &str) -> usize {
                    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
                }
                fn pad_to_width(s: &str, target: usize) -> String {
                    let w = display_width(s);
                    if w >= target {
                        s.to_string()
                    } else {
                        format!("{}{}", s, " ".repeat(target - w))
                    }
                }
                let widths: Vec<usize> = headers
                    .iter()
                    .enumerate()
                    .map(|(i, h)| {
                        let max_row = rows
                            .iter()
                            .map(|r| r.get(i).map_or(0, |c| display_width(c)))
                            .max()
                            .unwrap_or(0);
                        display_width(h).max(max_row)
                    })
                    .collect();
                let sep: String = format!(
                    "+{}+",
                    widths
                        .iter()
                        .map(|w| "-".repeat(w + 2))
                        .collect::<Vec<_>>()
                        .join("+")
                );
                let fmt_row = |cells: &[String]| -> String {
                    format!(
                        "| {} |",
                        cells
                            .iter()
                            .enumerate()
                            .map(|(i, c)| pad_to_width(c, widths[i]))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )
                };
                println!("{}", sep);
                println!(
                    "{}",
                    fmt_row(&headers.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                );
                println!("{}", sep);
                for row in &rows {
                    println!("{}", fmt_row(row));
                }
                println!("{}", sep);
            }
        }
        serde_json::Value::Object(obj) => {
            for (k, v) in obj {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    _ => serde_json::to_string(v).unwrap_or_default(),
                };
                println!(
                    "{}: {}",
                    k,
                    if s.chars().count() > 80 {
                        let truncated: String = s.chars().take(77).collect();
                        format!("{}...", truncated)
                    } else {
                        s
                    }
                );
            }
        }
        _ => println!("{}", serde_json::to_string(val).unwrap()),
    }
}

fn print_yaml(val: &serde_json::Value, indent: usize) {
    let pad = "  ".repeat(indent);
    match val {
        serde_json::Value::Null => println!("{}null", pad),
        serde_json::Value::Bool(b) => println!("{}{}", pad, b),
        serde_json::Value::Number(n) => println!("{}{}", pad, n),
        serde_json::Value::String(s) => println!("{}{}", pad, s),
        serde_json::Value::Array(arr) => {
            for item in arr {
                print!("{}- ", pad);
                if item.is_object() {
                    println!();
                    print_yaml(item, indent + 1);
                } else {
                    let s = serde_json::to_string(item).unwrap_or_default();
                    println!("{}", s);
                }
            }
        }
        serde_json::Value::Object(obj) => {
            for (k, v) in obj {
                match v {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        println!("{}{}:", pad, k);
                        print_yaml(v, indent + 1);
                    }
                    _ => {
                        let s = match v {
                            serde_json::Value::Null => "null".to_string(),
                            serde_json::Value::String(s) => s.clone(),
                            _ => serde_json::to_string(v).unwrap_or_default(),
                        };
                        println!("{}{}: {}", pad, k, s);
                    }
                }
            }
        }
    }
}

async fn task_transition<F>(
    c: &client::HttpClient,
    id: &str,
    status: &str,
    comment: Option<&str>,
    evidence: Option<&str>,
    agent: &str,
    output: F,
) -> Result<()>
where
    F: Fn(&serde_json::Value),
{
    let mut payload = json!({ "status": status, "_agent": agent });
    if let Some(text) = comment {
        payload["_comment"] = json!(text);
        payload["_author"] = json!(agent);
    }
    if let Some(text) = evidence {
        payload["evidence"] = json!(text);
    }
    let val = client::request(c, "PATCH", &format!("/tasks/{id}"), Some(payload)).await?;
    output(&val);
    Ok(())
}

mod commands {
    pub mod execute {
        //! Helpers for the `task view` drift banner.

        pub mod drift_warning {
            //! Format the daemon's `/tasks/{id}/drift` response as a human
            //! warning banner (LM-82 / RL-U5-06). Pure: takes the JSON value,
            //! returns an Option<String> that the dispatcher prints to
            //! stderr. None for `drift_level == "none"` keeps the happy path
            //! quiet; minor is one gray line; major is a yellow multi-line
            //! banner with the file count and a remediation hint.
            use serde_json::Value;

            const C_GRAY: &str = "\x1b[90m";
            const C_YELLOW: &str = "\x1b[33m";
            const C_BOLD: &str = "\x1b[1m";
            const C_RESET: &str = "\x1b[0m";

            /// Strip ANSI color when stderr is not a TTY (or NO_COLOR is set).
            /// Tests opt out of color regardless via the `_plain` builder.
            pub fn format(drift: &Value, color: bool) -> Option<String> {
                let level = drift
                    .get("drift_level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none");
                if level == "none" {
                    return None;
                }
                let count = drift
                    .get("changed_files_in_scope")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .or_else(|| {
                        drift
                            .get("total_changed")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as usize)
                    })
                    .unwrap_or(0);
                let total = drift
                    .get("total_changed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(count as u64);
                let planned = drift
                    .get("planned_sha")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let current = drift
                    .get("current_sha")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let short = |s: &str| -> String {
                    if s.len() >= 7 {
                        s.chars().take(7).collect()
                    } else {
                        s.to_string()
                    }
                };
                match level {
                    "minor" => {
                        let body = format!(
                            "[drift:minor] {count} of {total} changed files in scope \
                             (planned {} → current {})",
                            short(planned),
                            short(current),
                        );
                        Some(if color {
                            format!("{C_GRAY}{body}{C_RESET}")
                        } else {
                            body
                        })
                    }
                    "major" => {
                        let header = format!(
                            "{}drift: MAJOR{}  {count} files in scope changed (out of {total})",
                            if color { C_BOLD } else { "" },
                            if color { C_RESET } else { "" },
                        );
                        let detail = format!(
                            "  planned_sha: {} → current: {}",
                            short(planned),
                            short(current),
                        );
                        let hint = "  hint: `clawket task envelope <id> set --field \
                                    planned_sha=$(git rev-parse HEAD)` to re-anchor";
                        let body = format!("{header}\n{detail}\n{hint}");
                        Some(if color {
                            format!("{C_YELLOW}⚠ {body}{C_RESET}")
                        } else {
                            format!("⚠ {body}")
                        })
                    }
                    other => {
                        // Unknown level — surface the raw label so a future
                        // daemon adding e.g. "critical" doesn't go silent.
                        Some(format!("[drift:{other}] {count} files in scope"))
                    }
                }
            }

            #[cfg(test)]
            mod tests {
                use super::*;
                use serde_json::json;

                fn drift(level: &str, in_scope: usize, total: u64) -> Value {
                    let files: Vec<String> =
                        (0..in_scope).map(|i| format!("src/file{i}.rs")).collect();
                    json!({
                        "drift_level": level,
                        "changed_files_in_scope": files,
                        "total_changed": total,
                        "planned_sha": "8e25ace99553ab5984bf537046499bf3f9331426",
                        "current_sha": "abcdef1234567890abcdef1234567890abcdef12",
                    })
                }

                #[test]
                fn none_returns_no_banner() {
                    assert!(format(&drift("none", 0, 0), false).is_none());
                }

                #[test]
                fn minor_prints_single_gray_line() {
                    let out = format(&drift("minor", 2, 3), false).unwrap();
                    assert!(out.starts_with("[drift:minor]"), "got: {out}");
                    assert!(out.contains("2 of 3"));
                    // Short SHA: 7 chars
                    assert!(out.contains("8e25ace"));
                    assert!(out.contains("abcdef1"));
                    // Plain mode: no ANSI escape
                    assert!(!out.contains('\x1b'));
                }

                #[test]
                fn minor_with_color_wraps_in_gray_ansi() {
                    let out = format(&drift("minor", 1, 1), true).unwrap();
                    assert!(out.starts_with("\x1b[90m"));
                    assert!(out.ends_with("\x1b[0m"));
                }

                #[test]
                fn major_prints_warning_with_count_and_hint() {
                    let out = format(&drift("major", 5, 7), false).unwrap();
                    assert!(out.contains("drift: MAJOR"), "got: {out}");
                    assert!(out.contains("5 files"));
                    assert!(out.contains("out of 7"));
                    assert!(
                        out.contains("clawket task envelope"),
                        "remediation hint missing: {out}"
                    );
                    assert!(
                        out.contains("git rev-parse HEAD"),
                        "remediation hint missing: {out}"
                    );
                    // Three lines minimum (header, detail, hint)
                    assert!(out.lines().count() >= 3, "got: {out}");
                }

                #[test]
                fn major_with_color_wraps_in_yellow_and_bold() {
                    let out = format(&drift("major", 5, 7), true).unwrap();
                    assert!(out.starts_with("\x1b[33m⚠"));
                    assert!(out.contains("\x1b[1mdrift: MAJOR\x1b[0m"));
                }

                #[test]
                fn unknown_level_surfaces_raw_label() {
                    let v = json!({
                        "drift_level": "critical",
                        "changed_files_in_scope": ["x.rs"],
                        "total_changed": 1,
                        "planned_sha": "abc",
                        "current_sha": "def",
                    });
                    let out = format(&v, false).unwrap();
                    assert!(out.contains("drift:critical"));
                }

                #[test]
                fn missing_count_falls_back_to_total_changed() {
                    let v = json!({
                        "drift_level": "minor",
                        "total_changed": 4,
                        "planned_sha": "a",
                        "current_sha": "b",
                    });
                    let out = format(&v, false).unwrap();
                    assert!(out.contains("4 of 4"));
                }
            }
        }
    }

    pub mod task {
        pub mod decompose {
            //! `clawket task decompose <task>` — propose sub-tasks from the
            //! envelope's success_criteria + decomposition_policy (LM-79 /
            //! RL-U5-03). Pure helpers here mirror the logic in
            //! `mcp::clawket_decompose_task` (LM-74) but stay decoupled to
            //! avoid coupling the MCP tool's input shape to the CLI's.
            use serde_json::{Value, json};

            /// Parse success_criteria from any envelope shape. ADR-0001 says
            /// `array of strings`, but legacy/string-bulleted envelopes need
            /// to keep working; accept both.
            pub fn extract_success_criteria(envelope: &Value) -> Vec<String> {
                let Some(sc) = envelope.get("success_criteria") else {
                    return Vec::new();
                };
                if let Some(arr) = sc.as_array() {
                    return arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                if let Some(s) = sc.as_str() {
                    return s
                        .lines()
                        .map(|l| {
                            l.trim_start_matches(['-', '*', ' ', '\t'])
                                .trim()
                                .to_string()
                        })
                        .filter(|l| !l.is_empty())
                        .collect();
                }
                Vec::new()
            }

            /// Truncate at char boundary, append U+2026 if cut.
            pub fn truncate(s: &str, max: usize) -> String {
                let count = s.chars().count();
                if count > max {
                    let head: String = s.chars().take(max).collect();
                    format!("{head}…")
                } else {
                    s.to_string()
                }
            }

            /// Build the numbered suggestion list from parent title +
            /// extracted criteria. `strategy` annotates each suggestion's
            /// scope_hint; the actual scope decision is the user's.
            pub fn build_suggestions(
                parent_title: &str,
                criteria: &[String],
                strategy: &str,
            ) -> Vec<Value> {
                criteria
                    .iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let scope_hint = match strategy {
                            "by-repo" => "scope by-repo (cli/daemon/web 분리)",
                            "scoped" => "scope by surface (api|ui|infra)",
                            _ => "auto",
                        };
                        json!({
                            "idx": i,
                            "title": format!("{} — {}", parent_title, truncate(line, 80)),
                            "rationale": format!("success_criteria #{}: {}", i + 1, truncate(line, 120)),
                            "scope_hint": scope_hint,
                            "inherited_envelope_keys": [
                                "intent", "prompt_template", "decomposition_policy"
                            ],
                        })
                    })
                    .collect()
            }

            /// Walk decomposition_policy constraints and produce violations.
            /// Mutates `suggestions` to truncate when `max_subtasks` is set.
            pub fn check_policy_violations(
                envelope: &Value,
                suggestions: &mut Vec<Value>,
                max_depth: u32,
            ) -> Vec<Value> {
                let mut violations = Vec::new();
                if let Some(p) = envelope.get("decomposition_policy") {
                    if let Some(min_n) = p.get("min_subtasks").and_then(|v| v.as_u64())
                        && (suggestions.len() as u64) < min_n
                    {
                        violations.push(json!({
                            "field": "min_subtasks", "severity": "warning",
                            "message": format!(
                                "only {} suggestions, min_subtasks={}",
                                suggestions.len(), min_n
                            ),
                        }));
                    }
                    if let Some(max_n) = p.get("max_subtasks").and_then(|v| v.as_u64())
                        && (suggestions.len() as u64) > max_n
                    {
                        violations.push(json!({
                            "field": "max_subtasks", "severity": "warning",
                            "message": format!(
                                "{} suggestions exceed max_subtasks={}",
                                suggestions.len(), max_n
                            ),
                        }));
                        suggestions.truncate(max_n as usize);
                    }
                    if let Some(pmd) = p.get("max_depth").and_then(|v| v.as_u64())
                        && (max_depth as u64) > pmd
                    {
                        violations.push(json!({
                            "field": "max_depth", "severity": "error",
                            "message": format!(
                                "requested max_depth={} exceeds policy max_depth={}",
                                max_depth, pmd
                            ),
                        }));
                    }
                } else {
                    violations.push(json!({
                        "field": "decomposition_policy", "severity": "warning",
                        "message": "no decomposition_policy on resolved envelope — using default",
                    }));
                }
                if suggestions.is_empty() {
                    violations.push(json!({
                        "field": "success_criteria", "severity": "error",
                        "message": "no parseable success_criteria — cannot derive subtasks. \
                                    Add a bullet list under success_criteria.",
                    }));
                }
                violations
            }

            /// Parse `--accept` argument. `"ALL"` (case-insensitive) returns
            /// `Accept::All`; comma-separated 1-based integers return
            /// `Accept::Indices`. Empty input is rejected.
            #[derive(Debug, PartialEq, Eq)]
            pub enum Accept {
                All,
                Indices(Vec<usize>),
            }

            pub fn parse_accept(raw: &str) -> anyhow::Result<Accept> {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    anyhow::bail!("--accept value is empty");
                }
                if trimmed.eq_ignore_ascii_case("all") {
                    return Ok(Accept::All);
                }
                let mut idxs = Vec::new();
                for part in trimmed.split(',') {
                    let p = part.trim();
                    if p.is_empty() {
                        continue;
                    }
                    let n: usize = p.parse().map_err(|_| {
                        anyhow::anyhow!(
                            "--accept expects ALL or comma-separated 1-based integers, \
                             got `{p}`"
                        )
                    })?;
                    if n == 0 {
                        anyhow::bail!("--accept indices are 1-based; got 0");
                    }
                    idxs.push(n);
                }
                if idxs.is_empty() {
                    anyhow::bail!("--accept produced no indices from `{raw}`");
                }
                Ok(Accept::Indices(idxs))
            }

            /// Apply Accept against the suggestion list. Out-of-range indices
            /// are silently dropped — the CLI surfaces them in the post-create
            /// summary so the user can re-run.
            pub fn apply_accept(suggestions: Vec<Value>, accept: &Accept) -> Vec<Value> {
                match accept {
                    Accept::All => suggestions,
                    Accept::Indices(idxs) => suggestions
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| idxs.contains(&(i + 1)))
                        .map(|(_, v)| v)
                        .collect(),
                }
            }

            /// LM-265 / L1.3.c — atomic gate. Result of inspecting a task's
            /// `atomic_size_hint` before any envelope work happens. If the
            /// strict-format author declared the task atomic
            /// (`tiny` / `small`) the CLI refuses to propose sub-tasks: a
            /// 0/1-suggestion preview wastes the user's attention and an
            /// `--accept` would create children that violate the original
            /// scope contract. The hint is sourced from the task row
            /// (mirrored from the envelope at import time per LM-263), so
            /// the gate works even if the envelope has since drifted.
            #[derive(Debug, PartialEq, Eq)]
            pub enum AtomicGate {
                Allow,
                Refuse {
                    size_hint: String,
                    suggestion: String,
                },
            }

            /// Map an `atomic_size_hint` to a gate decision.
            ///
            /// `tiny`/`small` → refuse. Everything else (`medium`, `large`,
            /// unknown values from legacy plans without the field) → allow,
            /// because the cap is enforced separately by `size_max_subtasks`.
            pub fn check_atomic_size_hint(hint: &str) -> AtomicGate {
                match hint {
                    "tiny" | "small" => AtomicGate::Refuse {
                        size_hint: hint.to_string(),
                        suggestion: "Already atomic — process in a single session. \
                             To override, raise atomic_size_hint via plan re-import."
                            .to_string(),
                    },
                    _ => AtomicGate::Allow,
                }
            }

            /// LM-265 / L1.3.c — size-proportional fan-out cap.
            ///
            /// `medium` → 3, `large` → 5. Unknown hints return `None`,
            /// which falls back to whatever cap the envelope's
            /// `decomposition_policy.max_subtasks` already declares (or no
            /// cap at all). The numbers are deliberately small: strict
            /// format treats decomposition as a hint, not a generator —
            /// a user who needs more children should re-author the plan.
            pub fn size_max_subtasks(hint: &str) -> Option<u64> {
                match hint {
                    "medium" => Some(3),
                    "large" => Some(5),
                    _ => None,
                }
            }

            /// Apply the size cap (if any) to `suggestions`, returning a
            /// violation entry when truncation occurred. Mirrors the
            /// shape `check_policy_violations` produces so the CLI's JSON
            /// output stays uniform (one schema, two sources of caps).
            pub fn apply_size_cap(suggestions: &mut Vec<Value>, hint: &str) -> Option<Value> {
                let cap = size_max_subtasks(hint)?;
                if (suggestions.len() as u64) <= cap {
                    return None;
                }
                let original = suggestions.len();
                suggestions.truncate(cap as usize);
                Some(json!({
                    "field": "atomic_size_hint",
                    "severity": "warning",
                    "message": format!(
                        "atomic_size_hint=\"{hint}\" caps suggestions at {cap}; \
                         truncated {original} → {cap}. Re-author the plan if \
                         more children are needed."
                    ),
                }))
            }

            /// LM-265 / L1.3.c — `decomposition_policy = "manual"` means
            /// the user is hand-authoring children. The CLI must downgrade
            /// any `--accept` to dry-run so it never silently creates
            /// rows under a manual policy. (`auto` and `atomic` don't
            /// trigger this; `atomic` is enforced upstream by the
            /// `atomic_size_hint` gate, and `auto` is the default.)
            pub fn is_manual_policy(policy: &str) -> bool {
                policy == "manual"
            }

            #[cfg(test)]
            mod tests {
                use super::*;

                fn fixture_envelope_with_array_criteria() -> Value {
                    json!({
                        "success_criteria": [
                            "tests pass",
                            "docs land",
                            "lint clean"
                        ],
                        "decomposition_policy": {
                            "max_depth": 2,
                            "min_subtasks": 2,
                            "max_subtasks": 5
                        }
                    })
                }

                #[test]
                fn extract_success_criteria_handles_array_form() {
                    let env = fixture_envelope_with_array_criteria();
                    let c = extract_success_criteria(&env);
                    assert_eq!(c, vec!["tests pass", "docs land", "lint clean"]);
                }

                #[test]
                fn extract_success_criteria_handles_string_bullet_form() {
                    let env = json!({
                        "success_criteria": "- alpha\n- beta\n* gamma\n  \n  delta"
                    });
                    let c = extract_success_criteria(&env);
                    assert_eq!(c, vec!["alpha", "beta", "gamma", "delta"]);
                }

                #[test]
                fn extract_success_criteria_skips_empty_lines_and_strings() {
                    let env = json!({"success_criteria": ["", "  ", "real"]});
                    assert_eq!(extract_success_criteria(&env), vec!["real"]);
                }

                #[test]
                fn extract_success_criteria_returns_empty_when_field_missing() {
                    assert!(extract_success_criteria(&json!({})).is_empty());
                }

                #[test]
                fn truncate_short_string_unchanged() {
                    assert_eq!(truncate("hi", 10), "hi");
                }

                #[test]
                fn truncate_long_string_appends_ellipsis() {
                    let out = truncate("abcdefghij", 5);
                    assert_eq!(out, "abcde…");
                }

                #[test]
                fn truncate_handles_multibyte_chars() {
                    let out = truncate("훅 동작 실증 테스트", 4);
                    assert_eq!(out, "훅 동작…");
                }

                #[test]
                fn build_suggestions_titles_include_parent_and_criteria() {
                    let criteria = vec!["alpha".into(), "beta".into()];
                    let s = build_suggestions("Parent Task", &criteria, "auto");
                    assert_eq!(s.len(), 2);
                    assert_eq!(s[0]["title"], "Parent Task — alpha");
                    assert_eq!(s[0]["idx"], 0);
                    assert_eq!(s[1]["idx"], 1);
                    assert_eq!(s[0]["scope_hint"], "auto");
                }

                #[test]
                fn build_suggestions_strategy_changes_scope_hint() {
                    let c = vec!["x".into()];
                    let by_repo = build_suggestions("p", &c, "by-repo");
                    assert!(
                        by_repo[0]["scope_hint"]
                            .as_str()
                            .unwrap()
                            .contains("by-repo")
                    );
                    let scoped = build_suggestions("p", &c, "scoped");
                    assert!(
                        scoped[0]["scope_hint"]
                            .as_str()
                            .unwrap()
                            .contains("by surface")
                    );
                }

                #[test]
                fn check_violations_warns_when_under_min() {
                    let env = fixture_envelope_with_array_criteria();
                    let mut s = vec![json!({})];
                    let v = check_policy_violations(&env, &mut s, 2);
                    assert!(v.iter().any(|x| x["field"] == "min_subtasks"));
                }

                #[test]
                fn check_violations_truncates_when_over_max() {
                    let env = fixture_envelope_with_array_criteria();
                    let mut s: Vec<Value> = (0..7).map(|_| json!({})).collect();
                    let v = check_policy_violations(&env, &mut s, 2);
                    assert!(v.iter().any(|x| x["field"] == "max_subtasks"));
                    assert_eq!(s.len(), 5, "max_subtasks=5 must truncate the list");
                }

                #[test]
                fn check_violations_errors_on_max_depth_overrun() {
                    let env = fixture_envelope_with_array_criteria();
                    let mut s = vec![json!({}), json!({}), json!({})];
                    let v = check_policy_violations(&env, &mut s, 5);
                    assert!(
                        v.iter()
                            .any(|x| x["field"] == "max_depth" && x["severity"] == "error")
                    );
                }

                #[test]
                fn check_violations_warns_on_missing_policy() {
                    let env = json!({"success_criteria": ["x"]});
                    let mut s = vec![json!({})];
                    let v = check_policy_violations(&env, &mut s, 2);
                    assert!(v.iter().any(
                        |x| x["field"] == "decomposition_policy" && x["severity"] == "warning"
                    ));
                }

                #[test]
                fn check_violations_errors_when_suggestions_empty() {
                    let env = fixture_envelope_with_array_criteria();
                    let mut s: Vec<Value> = Vec::new();
                    let v = check_policy_violations(&env, &mut s, 2);
                    assert!(
                        v.iter()
                            .any(|x| x["field"] == "success_criteria" && x["severity"] == "error")
                    );
                }

                #[test]
                fn parse_accept_all_case_insensitive() {
                    assert_eq!(parse_accept("ALL").unwrap(), Accept::All);
                    assert_eq!(parse_accept("all").unwrap(), Accept::All);
                    assert_eq!(parse_accept("All").unwrap(), Accept::All);
                }

                #[test]
                fn parse_accept_csv_indices() {
                    assert_eq!(
                        parse_accept("1,3,5").unwrap(),
                        Accept::Indices(vec![1, 3, 5])
                    );
                    assert_eq!(
                        parse_accept(" 2 , 4 ").unwrap(),
                        Accept::Indices(vec![2, 4])
                    );
                }

                #[test]
                fn parse_accept_rejects_zero_or_garbage() {
                    assert!(parse_accept("0").is_err());
                    assert!(parse_accept("1,foo").is_err());
                    assert!(parse_accept("").is_err());
                    assert!(parse_accept(", ,").is_err());
                }

                #[test]
                fn apply_accept_all_keeps_everything() {
                    let s = vec![json!({"idx": 0}), json!({"idx": 1}), json!({"idx": 2})];
                    let kept = apply_accept(s.clone(), &Accept::All);
                    assert_eq!(kept.len(), 3);
                }

                #[test]
                fn apply_accept_indices_filters_to_listed() {
                    let s = vec![json!({"idx": 0}), json!({"idx": 1}), json!({"idx": 2})];
                    let kept = apply_accept(s, &Accept::Indices(vec![1, 3]));
                    assert_eq!(kept.len(), 2);
                    assert_eq!(kept[0]["idx"], 0);
                    assert_eq!(kept[1]["idx"], 2);
                }

                #[test]
                fn apply_accept_indices_silently_drops_out_of_range() {
                    let s = vec![json!({"idx": 0})];
                    let kept = apply_accept(s, &Accept::Indices(vec![1, 99]));
                    assert_eq!(kept.len(), 1);
                }

                // LM-265 / L1.3.c — atomic gate + size caps + manual policy.
                // Test names use the `decompose_atomic_*` prefix so they
                // match the verification_cmd `cargo test decompose_atomic`
                // from the LM-265 envelope.

                #[test]
                fn decompose_atomic_gate_refuses_tiny_and_small() {
                    for hint in ["tiny", "small"] {
                        match check_atomic_size_hint(hint) {
                            AtomicGate::Refuse {
                                size_hint,
                                suggestion,
                            } => {
                                assert_eq!(size_hint, hint);
                                assert!(
                                    suggestion.contains("single session"),
                                    "refusal message must point at single-session execution: {suggestion}"
                                );
                            }
                            other => panic!("expected Refuse for {hint:?}, got {other:?}"),
                        }
                    }
                }

                #[test]
                fn decompose_atomic_gate_allows_medium_and_above() {
                    for hint in ["medium", "large", "unknown", ""] {
                        assert_eq!(
                            check_atomic_size_hint(hint),
                            AtomicGate::Allow,
                            "{hint:?} should not trip the atomic gate"
                        );
                    }
                }

                #[test]
                fn decompose_atomic_size_cap_medium_caps_at_3() {
                    let mut s: Vec<Value> = (0..7).map(|i| json!({"idx": i})).collect();
                    let v = apply_size_cap(&mut s, "medium");
                    assert_eq!(s.len(), 3, "medium must truncate to 3");
                    let v = v.expect("medium with 7 suggestions must produce a violation");
                    assert_eq!(v["field"], "atomic_size_hint");
                    assert_eq!(v["severity"], "warning");
                }

                #[test]
                fn decompose_atomic_size_cap_large_caps_at_5() {
                    let mut s: Vec<Value> = (0..9).map(|i| json!({"idx": i})).collect();
                    let v = apply_size_cap(&mut s, "large");
                    assert_eq!(s.len(), 5, "large must truncate to 5");
                    assert!(
                        v.is_some(),
                        "large with 9 suggestions must produce a violation"
                    );
                }

                #[test]
                fn decompose_atomic_size_cap_under_threshold_is_noop() {
                    let mut s: Vec<Value> = (0..2).map(|i| json!({"idx": i})).collect();
                    let v = apply_size_cap(&mut s, "medium");
                    assert_eq!(s.len(), 2, "below cap must not truncate");
                    assert!(v.is_none(), "no violation when under cap");
                }

                #[test]
                fn decompose_atomic_size_cap_unknown_hint_is_noop() {
                    let mut s: Vec<Value> = (0..7).map(|i| json!({"idx": i})).collect();
                    let v = apply_size_cap(&mut s, "small"); // refused upstream, but
                    // also covers any hint that returns None from size_max_subtasks
                    assert_eq!(
                        s.len(),
                        7,
                        "no cap defined for `small` (gate refuses earlier)"
                    );
                    assert!(v.is_none());
                }

                #[test]
                fn decompose_atomic_manual_policy_is_detected() {
                    assert!(is_manual_policy("manual"));
                    for other in ["auto", "atomic", "Manual", "MANUAL", ""] {
                        assert!(
                            !is_manual_policy(other),
                            "{other:?} must not be classified as manual"
                        );
                    }
                }
            }
        }

        pub mod tree {
            //! `clawket task tree <task>` — render the task subtree fetched
            //! from `/tasks/{id}/subtree` (LM-80 / RL-U5-04). Pure helpers
            //! (depth bookkeeping + unicode prefix building) live here so
            //! they can be unit-tested without a daemon.
            use serde_json::Value;

            /// One row from `/tasks/{id}/subtree`. We pick exactly the
            /// fields the renderer needs and ignore the rest — keeps the
            /// pure layer decoupled from daemon schema drift.
            #[derive(Debug, Clone)]
            pub struct Node {
                pub depth: usize,
                pub ticket: String,
                pub title: String,
                pub status: String,
                pub intent: Option<String>,
            }

            /// Convert the raw daemon JSON array to renderer Nodes.
            pub fn nodes_from_subtree(raw: &Value) -> Vec<Node> {
                let arr = match raw.as_array() {
                    Some(a) => a,
                    None => return Vec::new(),
                };
                arr.iter()
                    .map(|row| {
                        let task = row.get("task").unwrap_or(row);
                        let depth = row
                            .get("depth")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0)
                            .max(0) as usize;
                        let ticket = task
                            .get("ticket_number")
                            .and_then(|v| v.as_str())
                            .or_else(|| task.get("id").and_then(|v| v.as_str()))
                            .unwrap_or("?")
                            .to_string();
                        let title = task
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(untitled)")
                            .to_string();
                        let status = task
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string();
                        let intent = row
                            .get("resolved_envelope")
                            .and_then(|env| env.get("intent"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        Node {
                            depth,
                            ticket,
                            title,
                            status,
                            intent,
                        }
                    })
                    .collect()
            }

            /// For each node in DFS order, decide whether it is the last
            /// child of its parent. A "younger sibling" exists when there
            /// is a later node at the same depth before any node at
            /// shallower depth (which would mean we backed out of the
            /// subtree).
            pub fn compute_is_last(depths: &[usize]) -> Vec<bool> {
                let n = depths.len();
                let mut out = vec![true; n];
                for i in 0..n {
                    let d = depths[i];
                    for j in (i + 1)..n {
                        if depths[j] < d {
                            break;
                        }
                        if depths[j] == d {
                            out[i] = false;
                            break;
                        }
                    }
                }
                out
            }

            /// Truncate at char boundary, append U+2026 if cut.
            fn truncate(s: &str, max: usize) -> String {
                let count = s.chars().count();
                if count > max {
                    let head: String = s.chars().take(max).collect();
                    format!("{head}…")
                } else {
                    s.to_string()
                }
            }

            /// Render lines for a flat DFS-ordered subtree. The root
            /// (depth=0) is printed without a prefix; descendants get
            /// unicode box-drawing connectors plus per-ancestor vertical
            /// bars where applicable. `envelope_summary` switches the
            /// intent-suffix on per node.
            pub fn render_tree_lines(nodes: &[Node], envelope_summary: bool) -> Vec<String> {
                let depths: Vec<usize> = nodes.iter().map(|n| n.depth).collect();
                let is_last = compute_is_last(&depths);
                let mut last_at_depth: Vec<bool> = Vec::new();
                let mut out = Vec::with_capacity(nodes.len());
                for (i, node) in nodes.iter().enumerate() {
                    let mut line = String::new();
                    let d = node.depth;
                    if d > 0 {
                        for k in 1..d {
                            let was_last = last_at_depth.get(k).copied().unwrap_or(true);
                            line.push_str(if was_last { "    " } else { "│   " });
                        }
                        line.push_str(if is_last[i] {
                            "└── "
                        } else {
                            "├── "
                        });
                    }
                    line.push_str(&format!(
                        "{} [{}] {}",
                        node.ticket,
                        node.status,
                        truncate(&node.title, 80)
                    ));
                    if envelope_summary {
                        if let Some(ref intent) = node.intent {
                            line.push_str(&format!("  · {}", truncate(intent, 80)));
                        }
                    }
                    out.push(line);
                    while last_at_depth.len() <= d {
                        last_at_depth.push(true);
                    }
                    last_at_depth[d] = is_last[i];
                }
                out
            }

            #[cfg(test)]
            mod tests {
                use super::*;
                use serde_json::json;

                fn n(depth: usize, ticket: &str, status: &str, title: &str) -> Node {
                    Node {
                        depth,
                        ticket: ticket.into(),
                        title: title.into(),
                        status: status.into(),
                        intent: None,
                    }
                }

                #[test]
                fn nodes_from_subtree_extracts_required_fields() {
                    let raw = json!([
                        {
                            "task": {
                                "id": "TASK-1", "ticket_number": "LM-1",
                                "title": "Root", "status": "in_progress"
                            },
                            "depth": 0,
                            "resolved_envelope": {"intent": "Be the parent"}
                        },
                        {
                            "task": {
                                "id": "TASK-2", "ticket_number": "LM-2",
                                "title": "Child", "status": "todo"
                            },
                            "depth": 1,
                            "resolved_envelope": null
                        }
                    ]);
                    let ns = nodes_from_subtree(&raw);
                    assert_eq!(ns.len(), 2);
                    assert_eq!(ns[0].ticket, "LM-1");
                    assert_eq!(ns[0].depth, 0);
                    assert_eq!(ns[0].status, "in_progress");
                    assert_eq!(ns[0].intent.as_deref(), Some("Be the parent"));
                    assert_eq!(ns[1].depth, 1);
                    assert_eq!(ns[1].intent, None);
                }

                #[test]
                fn nodes_from_subtree_handles_missing_ticket_falls_back_to_id() {
                    let raw = json!([{
                        "task": {"id": "TASK-X", "title": "T", "status": "todo"},
                        "depth": 0
                    }]);
                    let ns = nodes_from_subtree(&raw);
                    assert_eq!(ns[0].ticket, "TASK-X");
                }

                #[test]
                fn nodes_from_subtree_returns_empty_on_non_array() {
                    let ns = nodes_from_subtree(&json!({"oops": "object"}));
                    assert!(ns.is_empty());
                }

                #[test]
                fn compute_is_last_marks_only_child_as_last() {
                    assert_eq!(compute_is_last(&[0]), vec![true]);
                }

                #[test]
                fn compute_is_last_marks_middle_siblings_as_not_last() {
                    // root, A, B, C at depth 1
                    assert_eq!(
                        compute_is_last(&[0, 1, 1, 1]),
                        vec![true, false, false, true]
                    );
                }

                #[test]
                fn compute_is_last_handles_nested_subtree_after_a_sibling() {
                    // 0:root, 1:A, 2:A.x, 1:B (B is younger sibling of A)
                    // A is NOT last because B follows at the same depth.
                    let v = compute_is_last(&[0, 1, 2, 1]);
                    assert_eq!(v, vec![true, false, true, true]);
                }

                #[test]
                fn render_root_only_has_no_prefix() {
                    let lines = render_tree_lines(&[n(0, "LM-1", "todo", "Solo")], false);
                    assert_eq!(lines.len(), 1);
                    assert!(!lines[0].contains("├"));
                    assert!(!lines[0].contains("└"));
                    assert!(lines[0].contains("LM-1"));
                    assert!(lines[0].contains("[todo]"));
                    assert!(lines[0].contains("Solo"));
                }

                #[test]
                fn render_two_children_uses_branch_then_corner() {
                    let nodes = vec![
                        n(0, "LM-1", "in_progress", "Root"),
                        n(1, "LM-2", "todo", "First"),
                        n(1, "LM-3", "done", "Second"),
                    ];
                    let lines = render_tree_lines(&nodes, false);
                    assert_eq!(lines.len(), 3);
                    assert!(lines[1].starts_with("├── "));
                    assert!(lines[2].starts_with("└── "));
                }

                #[test]
                fn render_three_depth_keeps_vertical_bar_under_unfinished_branch() {
                    // root
                    // ├── A
                    // │   └── A.1
                    // └── B
                    let nodes = vec![
                        n(0, "LM-1", "todo", "root"),
                        n(1, "LM-2", "todo", "A"),
                        n(2, "LM-3", "todo", "A.1"),
                        n(1, "LM-4", "todo", "B"),
                    ];
                    let lines = render_tree_lines(&nodes, false);
                    assert_eq!(lines.len(), 4, "3-depth tree must render all 4 nodes");
                    assert!(lines[1].starts_with("├── "));
                    // A.1 must keep the parent's vertical bar because A has
                    // a younger sibling B coming up.
                    assert!(lines[2].starts_with("│   └── "), "got: {:?}", lines[2]);
                    assert!(lines[3].starts_with("└── "));
                }

                #[test]
                fn render_envelope_summary_appends_intent() {
                    let mut nodes = vec![n(0, "LM-1", "todo", "root")];
                    nodes[0].intent = Some("ship the thing".into());
                    let plain = render_tree_lines(&nodes, false);
                    let with_env = render_tree_lines(&nodes, true);
                    assert!(!plain[0].contains("ship the thing"));
                    assert!(with_env[0].contains("ship the thing"));
                }

                #[test]
                fn render_envelope_summary_skips_when_intent_missing() {
                    let nodes = vec![n(0, "LM-1", "todo", "root")];
                    let with_env = render_tree_lines(&nodes, true);
                    assert!(!with_env[0].contains("·"));
                }

                #[test]
                fn render_truncates_long_titles_with_ellipsis() {
                    let long = "a".repeat(200);
                    let nodes = vec![n(0, "LM-1", "todo", &long)];
                    let lines = render_tree_lines(&nodes, false);
                    assert!(
                        lines[0].ends_with('…'),
                        "expected ellipsis, got {:?}",
                        lines[0]
                    );
                }

                #[test]
                fn render_handles_empty_input() {
                    assert!(render_tree_lines(&[], false).is_empty());
                }
            }
        }
    }

    pub mod plan {
        pub mod export {
            //! `clawket plan export <id> [--format md|json|yaml]` —
            //! reverse-render a plan from the DB to markdown / canonical
            //! JSON / YAML (LM-86 / RL-U5-10).
            //!
            //! The DB is the single source of truth. Hand-edited plan
            //! markdown drifts from the DB the moment any task or envelope
            //! is touched; this command makes the markdown a *generated
            //! view*, not a maintained artifact.
            //!
            //! Pure renderers (`render_markdown`, `render_json`,
            //! `render_yaml`, `mermaid_dag`) take a fully-fetched
            //! `PlanBundle` and return strings. The async dispatcher does
            //! the I/O (fetch plan / units / tasks / envelopes, write to
            //! stdout or `--output`). This split lets the renderers be
            //! unit-tested without a daemon.
            //!
            //! Round-trip note: the current `plan import` parser (daemon
            //! `import_plan.rs`) reads only `# title`, `## Unit N: title`
            //! and `### task title` headings — envelope fields and
            //! depends_on edges are *exported* as bullets but ignored on
            //! reimport. Full round-trip parity is a follow-up task; see
            //! the comment on `render_envelope_bullets` for the block
            //! shape import would need to learn.
            use serde_json::{Value, json};

            /// Plan + units + tasks (with resolved envelopes) — the input
            /// to every renderer. Built by the dispatcher from the daemon.
            #[derive(Debug, Clone, Default)]
            pub struct PlanBundle {
                pub plan: Value,
                pub units: Vec<UnitBundle>,
                /// Optional knowledge entries attached to the plan,
                /// included when `--include-knowledge` is set.
                pub knowledge: Vec<Value>,
            }

            #[derive(Debug, Clone, Default)]
            pub struct UnitBundle {
                pub unit: Value,
                pub tasks: Vec<TaskBundle>,
            }

            #[derive(Debug, Clone, Default)]
            pub struct TaskBundle {
                pub task: Value,
                /// Resolved envelope (parents merged) or `Value::Null` if
                /// the task has no envelope.
                pub envelope: Value,
            }

            /// 19 envelope fields per ADR-0001, in canonical render order.
            /// Anything outside this list still appears in the JSON export
            /// but is omitted from the markdown bullet list — keeps the
            /// rendered shape stable across schema growth.
            pub const ENVELOPE_FIELDS: &[&str] = &[
                "version",
                "intent",
                "target_repo",
                "target_model",
                "max_turns",
                "prompt_template",
                "context_refs",
                "scope_boundary",
                "atomic_size_hint",
                "success_criteria",
                "verification_cmd",
                "depends_on",
                "blocked_by",
                "planned_sha",
                "decomposition_policy",
                "checkpoint_interval",
                "rollback_strategy",
                "origin",
                "assigned_model",
            ];

            /// Render a plan as markdown. The block layout mirrors the
            /// hand-authored `plans/v11-*.md` shape so reviewers can read
            /// the export the same way they read the originals.
            pub fn render_markdown(b: &PlanBundle) -> String {
                let mut out = String::new();
                let title = b
                    .plan
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(untitled)");
                out.push_str(&format!("# {title}\n\n"));

                if let Some(desc) = b.plan.get("description").and_then(|v| v.as_str()) {
                    if !desc.is_empty() {
                        out.push_str(desc.trim_end());
                        out.push_str("\n\n");
                    }
                }

                push_meta_section(&mut out, &b.plan);
                push_overview_table(&mut out, b);
                push_envelope_legend(&mut out);

                for ub in &b.units {
                    push_unit_section(&mut out, ub);
                }

                push_dependency_graph(&mut out, b);
                push_milestones(&mut out, &b.plan);

                if !b.knowledge.is_empty() {
                    push_knowledge_appendix(&mut out, &b.knowledge);
                }

                out
            }

            fn push_meta_section(out: &mut String, plan: &Value) {
                out.push_str("## Meta\n\n");
                let id = plan.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let status = plan.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let project = plan
                    .get("project_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                out.push_str(&format!("- id: `{id}`\n"));
                out.push_str(&format!("- project: `{project}`\n"));
                out.push_str(&format!("- status: `{status}`\n"));
                if let Some(src) = plan.get("source").and_then(|v| v.as_str()) {
                    out.push_str(&format!("- source: `{src}`\n"));
                }
                out.push('\n');
            }

            fn push_overview_table(out: &mut String, b: &PlanBundle) {
                out.push_str("## Overview\n\n");
                out.push_str("| Unit | Title | Tasks | todo | doing | done |\n");
                out.push_str("|------|-------|------:|-----:|------:|-----:|\n");
                for (i, ub) in b.units.iter().enumerate() {
                    let utitle = ub.unit.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                    let total = ub.tasks.len();
                    let mut todo = 0usize;
                    let mut doing = 0usize;
                    let mut done = 0usize;
                    for tb in &ub.tasks {
                        match tb.task.get("status").and_then(|v| v.as_str()).unwrap_or("") {
                            "todo" | "blocked" => todo += 1,
                            "in_progress" => doing += 1,
                            "done" => done += 1,
                            _ => {}
                        }
                    }
                    out.push_str(&format!(
                        "| {n} | {utitle} | {total} | {todo} | {doing} | {done} |\n",
                        n = i + 1
                    ));
                }
                out.push('\n');
            }

            fn push_envelope_legend(out: &mut String) {
                out.push_str("## 표기 규약 (Envelope 19 fields per ADR-0001)\n\n");
                out.push_str(
                    "Each task below renders the resolved envelope (parent chain merged) \
                     as a bullet list. Fields render in this canonical order:\n\n",
                );
                for f in ENVELOPE_FIELDS {
                    out.push_str(&format!("- `{f}`\n"));
                }
                out.push_str(
                    "\nFields not in this list are exported in `--format json` but \
                     omitted from markdown bullets to keep the export stable.\n\n",
                );
            }

            fn push_unit_section(out: &mut String, ub: &UnitBundle) {
                let idx = ub.unit.get("idx").and_then(|v| v.as_i64()).unwrap_or(0);
                let title = ub.unit.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                out.push_str(&format!("## Unit {idx}: {title}\n\n"));
                if let Some(goal) = ub.unit.get("goal").and_then(|v| v.as_str()) {
                    if !goal.is_empty() {
                        out.push_str(&format!("**Goal**: {goal}\n\n"));
                    }
                }
                for tb in &ub.tasks {
                    push_task_section(out, tb);
                }
            }

            fn push_task_section(out: &mut String, tb: &TaskBundle) {
                let title = tb.task.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let ticket = tb
                    .task
                    .get("ticket_number")
                    .and_then(|v| v.as_str())
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default();
                let status = tb
                    .task
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                out.push_str(&format!("### {title}{ticket} — _{status}_\n\n"));
                if let Some(body) = tb.task.get("body").and_then(|v| v.as_str()) {
                    if !body.is_empty() {
                        out.push_str(body.trim_end());
                        out.push_str("\n\n");
                    }
                }
                if !tb.envelope.is_null() {
                    out.push_str("**Envelope:**\n\n");
                    out.push_str(&render_envelope_bullets(&tb.envelope));
                    out.push('\n');
                }
            }

            /// Bullet-list shape of an envelope. Stable across schema
            /// growth: only `ENVELOPE_FIELDS` render here. Importer round-
            /// trip would parse this exact shape — `- key: value` with
            /// JSON-encoded scalars/arrays.
            pub fn render_envelope_bullets(env: &Value) -> String {
                let mut out = String::new();
                for f in ENVELOPE_FIELDS {
                    if let Some(v) = env.get(*f) {
                        if v.is_null() {
                            continue;
                        }
                        let rendered = match v {
                            Value::String(s) => format!("\"{s}\""),
                            Value::Array(_) | Value::Object(_) => {
                                serde_json::to_string(v).unwrap_or_else(|_| "null".into())
                            }
                            _ => v.to_string(),
                        };
                        out.push_str(&format!("- `{f}`: {rendered}\n"));
                    }
                }
                out
            }

            fn push_dependency_graph(out: &mut String, b: &PlanBundle) {
                let edges = collect_depends_on(b);
                if edges.is_empty() {
                    return;
                }
                out.push_str("## Dependency Graph\n\n");
                out.push_str("```mermaid\n");
                out.push_str("graph LR\n");
                for ub in &b.units {
                    for tb in &ub.tasks {
                        let label = tb
                            .task
                            .get("ticket_number")
                            .and_then(|v| v.as_str())
                            .or_else(|| tb.task.get("id").and_then(|v| v.as_str()))
                            .unwrap_or("?");
                        let id = tb.task.get("id").and_then(|v| v.as_str()).unwrap_or(label);
                        out.push_str(&format!("  {id}[\"{label}\"]\n"));
                    }
                }
                for (from, to) in &edges {
                    out.push_str(&format!("  {from} --> {to}\n"));
                }
                out.push_str("```\n\n");
            }

            /// `(from, to)` edges; `from` depends on `to`. Used for the
            /// mermaid block and the JSON export.
            pub fn collect_depends_on(b: &PlanBundle) -> Vec<(String, String)> {
                let mut edges = Vec::new();
                for ub in &b.units {
                    for tb in &ub.tasks {
                        let from = match tb.task.get("id").and_then(|v| v.as_str()) {
                            Some(s) => s.to_string(),
                            None => continue,
                        };
                        let Some(arr) = tb.task.get("depends_on").and_then(|v| v.as_array()) else {
                            continue;
                        };
                        for d in arr {
                            if let Some(s) = d.as_str() {
                                edges.push((from.clone(), s.to_string()));
                            }
                        }
                    }
                }
                edges
            }

            fn push_milestones(out: &mut String, plan: &Value) {
                let Some(ms) = plan.get("milestones_json").and_then(|v| v.as_array()) else {
                    return;
                };
                if ms.is_empty() {
                    return;
                }
                out.push_str("## Milestones\n\n");
                for m in ms {
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let due = m.get("due").and_then(|v| v.as_str()).unwrap_or("");
                    out.push_str(&format!("- **{name}**"));
                    if !due.is_empty() {
                        out.push_str(&format!(" — due {due}"));
                    }
                    if let Some(d) = m.get("description").and_then(|v| v.as_str()) {
                        out.push_str(&format!(": {d}"));
                    }
                    out.push('\n');
                }
                out.push('\n');
            }

            fn push_knowledge_appendix(out: &mut String, knowledge: &[Value]) {
                out.push_str("## Knowledge\n\n");
                for a in knowledge {
                    let title = a.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                    let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let kind = a.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                    out.push_str(&format!("### {title} (`{id}` · {kind})\n\n"));
                    if let Some(body) = a.get("body").and_then(|v| v.as_str()) {
                        out.push_str(body.trim_end());
                        out.push_str("\n\n");
                    }
                }
            }

            /// Canonical JSON export. Shape:
            /// `{plan, units: [{unit, tasks: [{task, envelope}]}], depends_on, knowledge}`.
            pub fn render_json(b: &PlanBundle) -> Value {
                let units: Vec<Value> = b
                    .units
                    .iter()
                    .map(|ub| {
                        let tasks: Vec<Value> = ub
                            .tasks
                            .iter()
                            .map(|tb| json!({"task": tb.task, "envelope": tb.envelope}))
                            .collect();
                        json!({"unit": ub.unit, "tasks": tasks})
                    })
                    .collect();
                let edges: Vec<Value> = collect_depends_on(b)
                    .into_iter()
                    .map(|(f, t)| json!({"from": f, "to": t}))
                    .collect();
                json!({
                    "plan": b.plan,
                    "units": units,
                    "depends_on": edges,
                    "knowledge": b.knowledge,
                })
            }

            #[cfg(test)]
            mod tests {
                use super::*;

                fn task(id: &str, title: &str, status: &str, depends_on: Vec<&str>) -> Value {
                    json!({
                        "id": id,
                        "title": title,
                        "status": status,
                        "ticket_number": id,
                        "depends_on": depends_on,
                    })
                }

                fn bundle_simple() -> PlanBundle {
                    PlanBundle {
                        plan: json!({
                            "id": "PLAN-XYZ",
                            "title": "Sample Plan",
                            "description": "A short description.",
                            "project_id": "PROJ-X",
                            "status": "active",
                            "source": "manual",
                        }),
                        units: vec![UnitBundle {
                            unit: json!({"id": "UNIT-1", "title": "Setup", "idx": 0, "goal": "G"}),
                            tasks: vec![TaskBundle {
                                task: task("T-1", "First task", "todo", vec![]),
                                envelope: json!({
                                    "version": 1,
                                    "intent": "do thing",
                                    "success_criteria": ["a", "b"],
                                }),
                            }],
                        }],
                        knowledge: vec![],
                    }
                }

                #[test]
                fn markdown_renders_required_sections() {
                    let md = render_markdown(&bundle_simple());
                    assert!(md.starts_with("# Sample Plan"));
                    assert!(md.contains("## Meta"));
                    assert!(md.contains("## Overview"));
                    assert!(md.contains("## 표기 규약"));
                    assert!(md.contains("## Unit 0: Setup"));
                    assert!(md.contains("### First task (T-1) — _todo_"));
                }

                #[test]
                fn markdown_renders_envelope_bullets_in_canonical_order() {
                    let md = render_markdown(&bundle_simple());
                    let intent_pos = md.find("`intent`").unwrap();
                    let success_pos = md.find("`success_criteria`").unwrap();
                    assert!(
                        intent_pos < success_pos,
                        "intent must precede success_criteria"
                    );
                }

                #[test]
                fn envelope_bullets_skip_unknown_fields() {
                    let env = json!({"intent": "x", "weird_field": "should not render"});
                    let s = render_envelope_bullets(&env);
                    assert!(s.contains("`intent`"));
                    assert!(!s.contains("weird_field"));
                }

                #[test]
                fn envelope_bullets_handle_arrays_and_strings() {
                    let env = json!({
                        "intent": "rewrite",
                        "success_criteria": ["a", "b"],
                        "version": 2,
                    });
                    let s = render_envelope_bullets(&env);
                    assert!(s.contains("`intent`: \"rewrite\""));
                    assert!(s.contains("`success_criteria`: [\"a\",\"b\"]"));
                    assert!(s.contains("`version`: 2"));
                }

                #[test]
                fn overview_table_counts_by_status() {
                    let mut b = bundle_simple();
                    b.units[0].tasks.push(TaskBundle {
                        task: task("T-2", "second", "in_progress", vec![]),
                        envelope: Value::Null,
                    });
                    b.units[0].tasks.push(TaskBundle {
                        task: task("T-3", "third", "done", vec![]),
                        envelope: Value::Null,
                    });
                    let md = render_markdown(&b);
                    let row = md.lines().find(|l| l.contains("Setup")).unwrap();
                    assert!(row.contains("| 3 |"), "total = 3, got: {row}");
                    assert!(row.contains("| 1 |"), "todo = 1, got: {row}");
                }

                #[test]
                fn mermaid_emits_one_edge_per_dependency() {
                    let mut b = bundle_simple();
                    b.units[0].tasks.push(TaskBundle {
                        task: task("T-2", "second", "todo", vec!["T-1"]),
                        envelope: Value::Null,
                    });
                    b.units[0].tasks.push(TaskBundle {
                        task: task("T-3", "third", "todo", vec!["T-1", "T-2"]),
                        envelope: Value::Null,
                    });
                    let md = render_markdown(&b);
                    assert!(md.contains("```mermaid"));
                    assert!(md.contains("T-2 --> T-1"));
                    assert!(md.contains("T-3 --> T-1"));
                    assert!(md.contains("T-3 --> T-2"));
                    let edges = collect_depends_on(&b);
                    let mermaid_edge_count = md.lines().filter(|l| l.contains(" --> ")).count();
                    assert!(
                        mermaid_edge_count >= edges.len(),
                        "mermaid DAG nodes/edges must cover all depends_on (LM-86 success_criterion c)"
                    );
                }

                #[test]
                fn mermaid_section_omitted_when_no_edges() {
                    let md = render_markdown(&bundle_simple());
                    assert!(!md.contains("## Dependency Graph"));
                }

                #[test]
                fn json_export_round_trips_structurally() {
                    let b = bundle_simple();
                    let v = render_json(&b);
                    assert_eq!(v["plan"]["id"], "PLAN-XYZ");
                    assert_eq!(v["units"][0]["unit"]["id"], "UNIT-1");
                    assert_eq!(v["units"][0]["tasks"][0]["task"]["id"], "T-1");
                    assert_eq!(v["units"][0]["tasks"][0]["envelope"]["intent"], "do thing");
                    assert!(v["depends_on"].is_array());
                }

                #[test]
                fn json_export_collects_depends_on_edges() {
                    let mut b = bundle_simple();
                    b.units[0].tasks.push(TaskBundle {
                        task: task("T-2", "x", "todo", vec!["T-1"]),
                        envelope: Value::Null,
                    });
                    let v = render_json(&b);
                    let edges = v["depends_on"].as_array().unwrap();
                    assert_eq!(edges.len(), 1);
                    assert_eq!(edges[0]["from"], "T-2");
                    assert_eq!(edges[0]["to"], "T-1");
                }

                #[test]
                fn milestones_section_renders_when_present() {
                    let mut b = bundle_simple();
                    b.plan["milestones_json"] = json!([
                        {"name": "M1", "due": "2026-05-01", "description": "alpha"},
                        {"name": "M2"},
                    ]);
                    let md = render_markdown(&b);
                    assert!(md.contains("## Milestones"));
                    assert!(md.contains("**M1** — due 2026-05-01: alpha"));
                    assert!(md.contains("**M2**"));
                }

                #[test]
                fn knowledge_appendix_renders_when_requested() {
                    let mut b = bundle_simple();
                    b.knowledge = vec![json!({
                        "id": "KN-1",
                        "title": "Decision",
                        "type": "decision",
                        "body": "we picked B over A.",
                    })];
                    let md = render_markdown(&b);
                    assert!(md.contains("## Knowledge"));
                    assert!(md.contains("Decision"));
                    assert!(md.contains("we picked B over A."));
                }

                #[test]
                fn envelope_field_count_matches_adr_0001() {
                    assert_eq!(
                        ENVELOPE_FIELDS.len(),
                        19,
                        "ADR-0001 fixes envelope at 19 fields; renderer must stay in lockstep"
                    );
                }
            }

            /// LM-254 / L1.2.a — strict-format spec lock-in.
            ///
            /// The previous `tests` module checks individual rendering
            /// concerns (mermaid edges, milestones, etc.). This module
            /// asserts the **whole-document shape** declared by
            /// `cli/docs/plans/strict-format.md` against a fixture bundle,
            /// so any drift between the spec and `render_markdown` fails
            /// loudly. Running test: `cargo test --bin clawket -- \
            /// commands::plan::export::strict_spec`.
            #[cfg(test)]
            mod strict_spec {
                use super::*;

                /// Canonical strict-format envelope for one task — all 19
                /// fields populated so the bullet-order assertion has full
                /// coverage. Required tier per ADR-0001 (`version`,
                /// `intent`, `target_repo`, `success_criteria`,
                /// `verification_cmd`, `decomposition_policy`,
                /// `context_refs`) is always present.
                fn full_envelope(intent: &str) -> Value {
                    json!({
                        "version": 1,
                        "intent": intent,
                        "target_repo": "daemon",
                        "target_model": "opus",
                        "max_turns": 12,
                        "prompt_template": "do thing",
                        "context_refs": [{"kind":"decision","id":"DEC-1"}],
                        "scope_boundary": "daemon/src/",
                        "atomic_size_hint": "small",
                        "success_criteria": ["a", "b"],
                        "verification_cmd": "cargo test",
                        "depends_on": [],
                        "blocked_by": [],
                        "planned_sha": "abc123",
                        "decomposition_policy": "atomic",
                        "checkpoint_interval": 5,
                        "rollback_strategy": "git revert",
                        "origin": "RL-X-01",
                        "assigned_model": "opus",
                    })
                }

                fn task(id: &str, title: &str, status: &str, depends_on: Vec<&str>) -> Value {
                    json!({
                        "id": id,
                        "title": title,
                        "status": status,
                        "ticket_number": id,
                        "depends_on": depends_on,
                    })
                }

                fn fixture_bundle() -> PlanBundle {
                    PlanBundle {
                        plan: json!({
                            "id": "PLAN-FIXTURE",
                            "title": "Spec Lock-in Plan",
                            "project_id": "PROJ-X",
                            "status": "active",
                        }),
                        units: vec![
                            UnitBundle {
                                unit: json!({"id":"UNIT-1","idx":0,"title":"Foundations","goal":"Lay groundwork."}),
                                tasks: vec![
                                    TaskBundle {
                                        task: task("T-A", "First", "todo", vec![]),
                                        envelope: full_envelope("first intent"),
                                    },
                                    TaskBundle {
                                        task: task("T-B", "Second", "in_progress", vec!["T-A"]),
                                        envelope: full_envelope("second intent"),
                                    },
                                ],
                            },
                            UnitBundle {
                                unit: json!({"id":"UNIT-2","idx":1,"title":"Delivery","goal":"Ship it."}),
                                tasks: vec![TaskBundle {
                                    task: task("T-C", "Third", "done", vec!["T-B"]),
                                    envelope: full_envelope("third intent"),
                                }],
                            },
                        ],
                        knowledge: vec![],
                    }
                }

                /// strict-format.md §"Section order" — top-to-bottom order
                /// of the H1, plan body, and required H2 sections.
                #[test]
                fn section_order_matches_spec() {
                    let md = render_markdown(&fixture_bundle());
                    let positions = [
                        ("# Spec Lock-in Plan", "H1"),
                        ("## Meta", "Meta"),
                        ("## Overview", "Overview"),
                        (
                            "## 표기 규약 (Envelope 19 fields per ADR-0001)",
                            "Envelope legend",
                        ),
                        ("## Unit 0: Foundations", "Unit 0"),
                        ("## Unit 1: Delivery", "Unit 1"),
                        ("## Dependency Graph", "Dependency Graph"),
                    ];
                    let mut prev_pos: Option<usize> = None;
                    let mut prev_label = "<start>";
                    for (needle, label) in positions {
                        let pos = md.find(needle).unwrap_or_else(|| {
                            panic!(
                                "section {label} ({needle:?}) missing — strict-format.md §Section order requires it.\nfull md:\n{md}"
                            )
                        });
                        if let Some(prev) = prev_pos {
                            assert!(
                                pos > prev,
                                "section {label} appears before {prev_label} — violates strict-format.md §Section order"
                            );
                        }
                        prev_pos = Some(pos);
                        prev_label = label;
                    }
                }

                /// strict-format.md §2 — Meta block shape.
                #[test]
                fn meta_block_shape_matches_spec() {
                    let md = render_markdown(&fixture_bundle());
                    let meta = extract_section(&md, "## Meta");
                    assert!(
                        meta.contains("- id: `PLAN-FIXTURE`"),
                        "Meta missing backticked id: {meta}"
                    );
                    assert!(
                        meta.contains("- project: `PROJ-X`"),
                        "Meta missing project: {meta}"
                    );
                    assert!(
                        meta.contains("- status: `active`"),
                        "Meta missing status: {meta}"
                    );
                }

                /// strict-format.md §3 — Overview header MUST be exactly the
                /// six columns Unit/Title/Tasks/todo/doing/done.
                #[test]
                fn overview_header_matches_spec() {
                    let md = render_markdown(&fixture_bundle());
                    assert!(
                        md.contains("| Unit | Title | Tasks | todo | doing | done |"),
                        "Overview header drifted from strict-format.md §3 (case-sensitive six-column header)"
                    );
                }

                /// strict-format.md §4 — envelope legend MUST list all 19
                /// fields in the canonical order, each backtick-quoted.
                #[test]
                fn envelope_legend_lists_all_19_fields_in_order() {
                    let md = render_markdown(&fixture_bundle());
                    let legend = extract_section(&md, "## 표기 규약");
                    let mut prev_pos = 0usize;
                    for f in ENVELOPE_FIELDS {
                        let needle = format!("- `{f}`");
                        let pos = legend.find(&needle).unwrap_or_else(|| {
                            panic!("envelope legend missing field `{f}` (strict-format.md §4)")
                        });
                        assert!(
                            pos >= prev_pos,
                            "envelope legend out of order at `{f}` — strict-format.md §4 fixes the canonical order"
                        );
                        prev_pos = pos;
                    }
                }

                /// strict-format.md §5 — `### {title} ({TICKET}) — _{status}_`
                /// pattern with em-dash and italic-wrapped status.
                #[test]
                fn task_heading_pattern_matches_spec() {
                    let md = render_markdown(&fixture_bundle());
                    assert!(
                        md.contains("### First (T-A) — _todo_"),
                        "task heading drift: strict-format.md §5 requires `### {{title}} ({{TICKET}}) — _{{status}}_`"
                    );
                    assert!(md.contains("### Second (T-B) — _in_progress_"));
                    assert!(md.contains("### Third (T-C) — _done_"));
                }

                /// strict-format.md §5 — envelope bullets render in §4
                /// canonical order. We populate the fixture envelope with
                /// all 19 fields and check ordering by find-position.
                #[test]
                fn envelope_bullets_render_in_canonical_order() {
                    let md = render_markdown(&fixture_bundle());
                    let task_a = extract_task_block(&md, "### First (T-A)");
                    let mut prev_pos = 0usize;
                    let mut prev_field = "<start>";
                    for f in ENVELOPE_FIELDS {
                        let needle = format!("- `{f}`:");
                        let pos = task_a.find(&needle).unwrap_or_else(|| {
                            panic!(
                                "envelope bullet `{f}` missing for task T-A — strict-format.md §5 requires it for fully-populated envelopes.\nblock:\n{task_a}"
                            )
                        });
                        assert!(
                            pos > prev_pos,
                            "envelope bullet `{f}` precedes `{prev_field}` — strict-format.md §5 fixes the order"
                        );
                        prev_pos = pos;
                        prev_field = f;
                    }
                }

                /// strict-format.md §5 — bullet value encodings:
                /// strings → `"..."`, integers → bare, arrays/objects → JSON.
                #[test]
                fn envelope_bullet_value_encodings_match_spec() {
                    let md = render_markdown(&fixture_bundle());
                    let blk = extract_task_block(&md, "### First (T-A)");
                    assert!(
                        blk.contains("- `version`: 1"),
                        "integer must render bare: {blk}"
                    );
                    assert!(
                        blk.contains("- `intent`: \"first intent\""),
                        "string must render JSON-quoted: {blk}"
                    );
                    assert!(
                        blk.contains("- `success_criteria`: [\"a\",\"b\"]"),
                        "array must render JSON one-line: {blk}"
                    );
                    // serde_json::to_string sorts object keys
                    // alphabetically — that's the canonical form the
                    // strict importer must accept.
                    assert!(
                        blk.contains(
                            "- `context_refs`: [{\"id\":\"DEC-1\",\"kind\":\"decision\"}]"
                        ),
                        "object array must render as JSON one-line with sorted keys: {blk}"
                    );
                }

                /// strict-format.md §6 — mermaid block uses `graph LR`,
                /// two-space indent, single-headed `-->` arrows. Section
                /// is omitted entirely when zero edges.
                #[test]
                fn dependency_graph_uses_canonical_mermaid_form() {
                    let md = render_markdown(&fixture_bundle());
                    let dag = extract_section(&md, "## Dependency Graph");
                    assert!(
                        dag.contains("```mermaid"),
                        "fence language must be mermaid: {dag}"
                    );
                    assert!(
                        dag.contains("graph LR\n"),
                        "first line must be `graph LR` (no flowchart): {dag}"
                    );
                    assert!(
                        dag.contains("  T-B --> T-A"),
                        "edge T-B→T-A missing or wrong indent: {dag}"
                    );
                    assert!(dag.contains("  T-C --> T-B"), "edge T-C→T-B missing: {dag}");
                    assert!(
                        !dag.contains("flowchart"),
                        "strict-format.md §6 forbids `flowchart`: {dag}"
                    );
                    assert!(
                        !dag.contains("graph TD"),
                        "strict-format.md §6 forbids `graph TD`: {dag}"
                    );
                }

                #[test]
                fn dependency_graph_section_omitted_when_zero_edges() {
                    let mut b = fixture_bundle();
                    for ub in &mut b.units {
                        for tb in &mut ub.tasks {
                            tb.task["depends_on"] = json!([]);
                        }
                    }
                    let md = render_markdown(&b);
                    assert!(
                        !md.contains("## Dependency Graph"),
                        "strict-format.md §6 says omit the section when zero edges; got:\n{md}"
                    );
                }

                /// strict-format.md §"Round-trip guarantees" — tied to
                /// `ENVELOPE_FIELDS`. If the spec doc lists a field the
                /// constant doesn't (or vice-versa), the legend test
                /// catches it. This test pins the count explicitly so
                /// schema growth is forced to update both spec and code.
                #[test]
                fn envelope_field_count_locked_at_19() {
                    assert_eq!(
                        ENVELOPE_FIELDS.len(),
                        19,
                        "strict-format.md §4 fixes the envelope at 19 fields; bumping requires ADR amendment"
                    );
                }

                fn extract_section<'a>(md: &'a str, heading: &str) -> &'a str {
                    let start = md
                        .find(heading)
                        .unwrap_or_else(|| panic!("heading {heading:?} missing in:\n{md}"));
                    let after = &md[start..];
                    let next_h2 = after[heading.len()..]
                        .find("\n## ")
                        .map(|p| p + heading.len() + 1);
                    match next_h2 {
                        Some(end) => &after[..end],
                        None => after,
                    }
                }

                fn extract_task_block<'a>(md: &'a str, heading: &str) -> &'a str {
                    let start = md
                        .find(heading)
                        .unwrap_or_else(|| panic!("task heading {heading:?} missing in:\n{md}"));
                    let after = &md[start..];
                    let next = after[heading.len()..]
                        .find("\n### ")
                        .or_else(|| after[heading.len()..].find("\n## "))
                        .map(|p| p + heading.len() + 1);
                    match next {
                        Some(end) => &after[..end],
                        None => after,
                    }
                }
            }
        }
    }
}

fn query_string(params: &[(&str, &Option<String>)]) -> String {
    let pairs: Vec<String> = params
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|val| format!("{}={}", k, urlenc(val))))
        .collect();
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

fn urlenc(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
}

/// Real entry point — separated from `main` so that `exit_from_error` can
/// classify any propagated error and exit with the correct code (FIX-CLI-008).
async fn run_main() -> Result<()> {
    let cli = Cli::parse();
    let fmt = cli.format.clone();
    let quiet = cli.quiet;
    // Auto-disable color when stdout is not a TTY or --no-color is set (US-CLAWKET-CLI-GF-006/007)
    let no_color = cli.no_color || !std::io::IsTerminal::is_terminal(&std::io::stdout());
    if no_color {
        // Set env var so any child processes and library output also disable color
        // SAFETY: single-threaded at this point (before any async tasks spawn)
        unsafe { std::env::set_var("NO_COLOR", "1") };
    }

    // Propagate --locale to subcommands / daemon via env (US-CLAWKET-I18N-030).
    // Subcommand handlers and the daemon already honour CLAWKET_LOCALE; the
    // global flag just promotes the value into that channel.
    if let Some(loc) = cli.locale.as_deref() {
        // SAFETY: single-threaded at this point (before any async tasks spawn).
        unsafe { std::env::set_var("CLAWKET_LOCALE", loc) };
    }

    // Propagate --tier (default tier policy) to subcommand handlers via env
    // so the per-subcommand --tier override remains authoritative when given
    // (US-CLAWKET-TIER-009 / TIER-045). v3.0 keeps this Within-Claude only —
    // see TIER-046 status line in `clawket doctor`.
    if let Some(t) = cli.tier.as_deref() {
        // SAFETY: see above.
        unsafe { std::env::set_var("CLAWKET_TIER", t) };
    }

    match cli.command {
        Command::Daemon { action } => {
            return daemon::run(action).await;
        }
        Command::Mcp => {
            return mcp::run().await;
        }
        Command::Doctor {
            json,
            plan,
            escalation,
        } => {
            return doctor::run(json, plan, escalation).await;
        }
        Command::Verify { dry_run } => {
            return verify::run(dry_run).await;
        }
        Command::Init { tutorial, cwd } => {
            return init::run(tutorial, cwd).await;
        }
        Command::Completions { ref shell } => {
            use clap::CommandFactory;
            use clap_complete::generate;
            use clap_complete::shells::{Bash, Elvish, Fish, PowerShell, Zsh};
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            match shell.as_str() {
                "bash" => generate(Bash, &mut cmd, name, &mut std::io::stdout()),
                "zsh" => generate(Zsh, &mut cmd, name, &mut std::io::stdout()),
                "fish" => generate(Fish, &mut cmd, name, &mut std::io::stdout()),
                "powershell" | "pwsh" => {
                    generate(PowerShell, &mut cmd, name, &mut std::io::stdout())
                }
                "elvish" => generate(Elvish, &mut cmd, name, &mut std::io::stdout()),
                other => {
                    eprintln!(
                        "ERROR: unsupported shell '{other}'. Choose: bash | zsh | fish | powershell | elvish"
                    );
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
        _ => {}
    }

    let c = client::make_client();
    let output = |val: &serde_json::Value| {
        if quiet {
            // In quiet mode, just print the ID field if present
            if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
                println!("{}", id);
            } else if let serde_json::Value::Array(arr) = val {
                for item in arr {
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                        println!("{}", id);
                    }
                }
            }
        } else {
            output_fmt(val, &fmt);
        }
    };

    match cli.command {
        Command::Daemon { .. } => unreachable!(),
        Command::Mcp => unreachable!(),
        Command::Doctor { .. } => unreachable!(),
        Command::Verify { .. } => unreachable!(),
        Command::Init { .. } => unreachable!(),
        Command::Completions { .. } => unreachable!(),

        Command::Dashboard { cwd, show } => {
            let cwd = cwd.unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            });
            // Resolve relative paths so they match registered project cwds (always absolute).
            let cwd = std::fs::canonicalize(&cwd)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or(cwd);
            let qs = format!("?cwd={}&show={}", urlenc(&cwd), urlenc(&show));
            let val = client::get(&c, &format!("/dashboard{qs}")).await?;
            // R3 DOGFOOD-009 fix: surface active-plan-count warning to stderr
            // before printing context to stdout. Hook consumers can grep stderr.
            if let Some(w) = val.get("active_plan_warning") {
                let count = w
                    .get("active_plan_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let level = w.get("level").and_then(|v| v.as_str()).unwrap_or("warn");
                eprintln!(
                    "[clawket dashboard] {} active plan count={} (>1) — PDD recommends ≤ 1 (≤ 2 in transition).",
                    level.to_uppercase(),
                    count
                );
            }
            // Print the context string directly (not JSON) for hook injection
            if let Some(ctx) = val.get("context").and_then(|v| v.as_str()) {
                print!("{ctx}");
            }
        }

        // ===== Project =====
        Command::Project { action } => match action {
            ProjectAction::Create {
                name,
                description,
                cwd,
                key,
            } => {
                let cwd = cwd.or_else(|| {
                    Some(
                        std::env::current_dir()
                            .unwrap()
                            .to_string_lossy()
                            .to_string(),
                    )
                });
                let val = client::request(
                    &c,
                    "POST",
                    "/projects",
                    Some(json!({
                        "name": name, "description": description, "cwd": cwd, "key": key
                    })),
                )
                .await?;
                output(&val);
            }
            ProjectAction::View { id } => {
                output(&client::get(&c, &format!("/projects/{id}")).await?)
            }
            ProjectAction::List => output(&client::get(&c, "/projects").await?),
            ProjectAction::Update {
                id,
                name,
                description,
                wiki_paths,
            } => {
                let mut body = json!({});
                if let Some(v) = name {
                    body["name"] = json!(v);
                }
                if let Some(v) = description {
                    body["description"] = json!(v);
                }
                if let Some(v) = wiki_paths {
                    let parsed: serde_json::Value =
                        serde_json::from_str(&v).unwrap_or_else(|_| json!([v]));
                    body["wiki_paths"] = parsed;
                }
                output(
                    &client::request(&c, "PATCH", &format!("/projects/{id}"), Some(body)).await?,
                );
            }
            ProjectAction::Delete { id } => {
                output(&client::request(&c, "DELETE", &format!("/projects/{id}"), None).await?);
            }
            ProjectAction::Disable { id } => {
                let val = client::request(
                    &c,
                    "PATCH",
                    &format!("/projects/{id}"),
                    Some(project_enabled_body(0)),
                )
                .await?;
                output(&val);
            }
            ProjectAction::Enable { id } => {
                let val = client::request(
                    &c,
                    "PATCH",
                    &format!("/projects/{id}"),
                    Some(project_enabled_body(1)),
                )
                .await?;
                output(&val);
            }
            ProjectAction::Resolve { cwd } => {
                let cwd = cwd.unwrap_or_else(|| {
                    std::env::current_dir()
                        .unwrap()
                        .to_string_lossy()
                        .to_string()
                });
                let cwd = std::fs::canonicalize(&cwd)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or(cwd);
                let path = format!("/projects/by-cwd{cwd}");
                let (status, val) = client::request_raw(&c, "GET", &path, None).await?;
                if status.as_u16() == 404 {
                    println!("null");
                } else if !status.is_success() {
                    let err = val
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("project resolve failed");
                    anyhow::bail!("{err}");
                } else {
                    output(&val);
                }
            }
            ProjectAction::Cwd { action } => match action {
                ProjectCwdAction::Add { id, path } => {
                    let cwd = path.unwrap_or_else(|| {
                        std::env::current_dir()
                            .unwrap()
                            .to_string_lossy()
                            .to_string()
                    });
                    output(
                        &client::request(
                            &c,
                            "POST",
                            &format!("/projects/{id}/cwds"),
                            Some(json!({"cwd": cwd})),
                        )
                        .await?,
                    );
                }
                ProjectCwdAction::Remove { id, path } => {
                    output(
                        &client::request(
                            &c,
                            "DELETE",
                            &format!("/projects/{id}/cwds"),
                            Some(json!({"cwd": path})),
                        )
                        .await?,
                    );
                }
                ProjectCwdAction::List { id } => {
                    let proj = client::get(&c, &format!("/projects/{id}")).await?;
                    if let Some(cwds) = proj.get("cwds") {
                        output(cwds);
                    } else {
                        output(&json!([]));
                    }
                }
            },
        },

        // ===== Plan =====
        Command::Plan { action } => match action {
            PlanAction::Create {
                title,
                project,
                description,
                source,
                source_path,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/plans",
                        Some(json!({
                            "project_id": project, "title": title, "description": description,
                            "source": source, "source_path": source_path,
                        })),
                    )
                    .await?,
                );
            }
            PlanAction::View { id } => output(&client::get(&c, &format!("/plans/{id}")).await?),
            PlanAction::List { project, status } => {
                let qs = query_string(&[("project_id", &project), ("status", &status)]);
                output(&client::get(&c, &format!("/plans{qs}")).await?);
            }
            PlanAction::Update {
                id,
                title,
                description,
                status,
            } => {
                let mut body = json!({});
                if let Some(v) = title {
                    body["title"] = json!(v);
                }
                if let Some(v) = description {
                    body["description"] = json!(v);
                }
                if let Some(v) = status {
                    body["status"] = json!(v);
                }
                output(&client::request(&c, "PATCH", &format!("/plans/{id}"), Some(body)).await?);
            }
            PlanAction::Delete { id } => {
                output(&client::request(&c, "DELETE", &format!("/plans/{id}"), None).await?);
            }
            PlanAction::Approve { id } => {
                output(&client::request(&c, "POST", &format!("/plans/{id}/approve"), None).await?);
            }
            PlanAction::Complete { id } => {
                output(
                    &client::request(
                        &c,
                        "PATCH",
                        &format!("/plans/{id}"),
                        Some(json!({"status": "completed"})),
                    )
                    .await?,
                );
            }
            PlanAction::Import {
                file,
                project,
                cwd,
                source,
                dry_run,
                strict,
            } => {
                // Default cwd to the CLI's actual working directory. The
                // daemon registers this against `--project` when the
                // project is new — relying on the daemon's own
                // `current_dir()` would register whatever directory the
                // daemon process was launched from, which is unrelated
                // to the user.
                let cwd = cwd.or_else(|| {
                    std::env::current_dir()
                        .ok()
                        .map(|p| p.to_string_lossy().into_owned())
                });
                if strict {
                    // Strict path reads markdown content inline so the
                    // daemon never has to re-resolve a relative path
                    // against its own cwd. `source` is intentionally
                    // dropped — the daemon stamps `strict-import` so the
                    // audit trail distinguishes hook-driven imports from
                    // the legacy file-based flow.
                    let _ = source;
                    let content = std::fs::read_to_string(&file)
                        .with_context(|| format!("read plan file: {file}"))?;
                    output(
                        &client::request(
                            &c,
                            "POST",
                            "/plans/import/strict",
                            Some(json!({
                                "content": content,
                                "project": project,
                                "cwd": cwd,
                                "dryRun": dry_run,
                            })),
                        )
                        .await?,
                    );
                } else {
                    output(&client::request(&c, "POST", "/plans/import", Some(json!({
                        "file": file, "project": project, "cwd": cwd, "source": source, "dryRun": dry_run,
                    }))).await?);
                }
            }
            PlanAction::Export {
                id,
                format,
                output: out_path,
                include_knowledge,
            } => {
                handle_plan_export(&c, &id, &format, out_path.as_deref(), include_knowledge)
                    .await?;
            }
        },

        // ===== Unit =====
        Command::Unit { action } => match action {
            UnitAction::Create {
                title,
                plan,
                goal,
                idx,
                mode,
            } => {
                if mode != "sequential" && mode != "parallel" {
                    eprintln!("Error: invalid value '{}' for '--mode'\n", mode);
                    eprintln!("  Valid values: sequential, parallel\n");
                    eprintln!("  sequential  Tasks execute one at a time (default)");
                    eprintln!(
                        "  parallel    Tasks can be executed by multiple agents simultaneously"
                    );
                    std::process::exit(1);
                }
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/units",
                        Some(json!({
                            "plan_id": plan, "title": title, "goal": goal, "idx": idx,
                            "execution_mode": mode,
                        })),
                    )
                    .await?,
                );
            }
            UnitAction::View { id } => output(&client::get(&c, &format!("/units/{id}")).await?),
            UnitAction::List { plan } => {
                let qs = query_string(&[("plan_id", &plan)]);
                output(&client::get(&c, &format!("/units{qs}")).await?);
            }
            UnitAction::Update {
                id,
                title,
                goal,
                mode,
            } => {
                let mut body = json!({});
                if let Some(v) = title {
                    body["title"] = json!(v);
                }
                if let Some(v) = goal {
                    body["goal"] = json!(v);
                }
                if let Some(ref v) = mode {
                    if v != "sequential" && v != "parallel" {
                        eprintln!("Error: invalid value '{}' for '--mode'\n", v);
                        eprintln!("  Valid values: sequential, parallel\n");
                        eprintln!("  sequential  Tasks execute one at a time");
                        eprintln!(
                            "  parallel    Tasks can be executed by multiple agents simultaneously"
                        );
                        std::process::exit(1);
                    }
                    body["execution_mode"] = json!(v);
                }
                output(&client::request(&c, "PATCH", &format!("/units/{id}"), Some(body)).await?);
            }
            UnitAction::Delete { id } => {
                output(&client::request(&c, "DELETE", &format!("/units/{id}"), None).await?);
            }
        },

        // ===== Cycle =====
        Command::Cycle { action } => match action {
            CycleAction::Create {
                title,
                project,
                unit,
                goal,
                idx,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/cycles",
                        Some(json!({
                            "project_id": project, "title": title, "unit_id": unit,
                            "goal": goal, "idx": idx,
                        })),
                    )
                    .await?,
                );
            }
            CycleAction::View { id } => output(&client::get(&c, &format!("/cycles/{id}")).await?),
            CycleAction::List { project, status } => {
                let qs = query_string(&[("project_id", &project), ("status", &status)]);
                output(&client::get(&c, &format!("/cycles{qs}")).await?);
            }
            CycleAction::Update {
                id,
                title,
                goal,
                status,
            } => {
                let mut body = json!({});
                if let Some(v) = title {
                    body["title"] = json!(v);
                }
                if let Some(v) = goal {
                    body["goal"] = json!(v);
                }
                if let Some(v) = status {
                    body["status"] = json!(v);
                }
                output(&client::request(&c, "PATCH", &format!("/cycles/{id}"), Some(body)).await?);
            }
            CycleAction::Delete { id } => {
                output(&client::request(&c, "DELETE", &format!("/cycles/{id}"), None).await?);
            }
            CycleAction::Activate { id } => {
                output(
                    &client::request(&c, "POST", &format!("/cycles/{id}/activate"), None).await?,
                );
            }
            CycleAction::Complete { id } => {
                output(
                    &client::request(
                        &c,
                        "PATCH",
                        &format!("/cycles/{id}"),
                        Some(json!({"status": "completed"})),
                    )
                    .await?,
                );
            }
        },

        // ===== Task =====
        Command::Task { action } => match action {
            TaskAction::Create {
                title,
                unit,
                body,
                assignee,
                idx,
                depends_on,
                parent_task,
                priority,
                complexity,
                estimated_edits,
                cycle,
                r#type,
                label,
                tier,
                scenario_id,
                evidence,
                batch_id,
            } => {
                // PDD A4: when --cycle is explicitly provided, --unit must also be provided
                if cycle.is_some() && unit.is_none() {
                    eprintln!(
                        "ERROR: --unit is required when --cycle is specified (PDD A4: every cycle belongs to exactly one unit)"
                    );
                    std::process::exit(2);
                }
                let cwd = std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string());
                let type_val = r#type;
                let labels_val: Option<serde_json::Value> = if label.is_empty() {
                    None
                } else {
                    Some(json!(label))
                };
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/tasks",
                        Some(json!({
                            "unit_id": unit, "title": title, "body": body.unwrap_or_default(),
                            "assignee": assignee, "idx": idx, "depends_on": depends_on,
                            "parent_task_id": parent_task, "priority": priority,
                            "complexity": complexity, "estimated_edits": estimated_edits,
                            "cycle_id": cycle, "cwd": cwd, "type": type_val,
                            "labels": labels_val,
                            "tier": tier,
                            "scenario_id": scenario_id,
                            "evidence": evidence,
                            "batch_id": batch_id,
                        })),
                    )
                    .await?,
                );
            }
            TaskAction::View { id } => {
                let task = client::get(&c, &format!("/tasks/{id}")).await?;
                output(&task);
                emit_drift_banner(&c, &id).await;
            }
            TaskAction::List {
                unit,
                plan,
                project,
                status,
                agent_id,
                cycle,
                no_cycle,
                label,
                tier,
                scenario_id,
                batch_id,
                evidence_empty,
                limit,
                offset,
            } => {
                let cycle_filter = if no_cycle {
                    Some("null".to_string())
                } else {
                    cycle
                };
                let evidence_empty_filter = if evidence_empty {
                    Some("1".to_string())
                } else {
                    None
                };
                let limit_filter = limit.map(|n| n.to_string());
                let offset_filter = offset.map(|n| n.to_string());
                let qs = query_string(&[
                    ("unit_id", &unit),
                    ("plan_id", &plan),
                    ("project_id", &project),
                    ("status", &status),
                    ("agent_id", &agent_id),
                    ("cycle_id", &cycle_filter),
                    ("label", &label),
                    ("tier", &tier),
                    ("scenario_id", &scenario_id),
                    ("batch_id", &batch_id),
                    ("evidence_empty", &evidence_empty_filter),
                    ("limit", &limit_filter),
                    ("offset", &offset_filter),
                ]);
                output(&client::get(&c, &format!("/tasks{qs}")).await?);
            }
            TaskAction::Update {
                id,
                title,
                body: task_body,
                status,
                assignee,
                session_id,
                agent,
                priority,
                complexity,
                estimated_edits,
                parent_task,
                cycle,
                agent_id,
                comment,
                tier,
                scenario_id,
                evidence,
                batch_id,
            } => {
                let mut payload = json!({});
                if let Some(v) = title {
                    payload["title"] = json!(v);
                }
                if let Some(v) = task_body {
                    payload["body"] = json!(v);
                }
                if let Some(v) = status {
                    payload["status"] = json!(v);
                }
                if let Some(ref v) = assignee {
                    payload["assignee"] = json!(v);
                }
                if let Some(v) = session_id {
                    payload["_session_id"] = json!(v);
                }
                payload["_agent"] = json!(agent);
                if let Some(v) = priority {
                    payload["priority"] = json!(v);
                }
                if let Some(v) = complexity {
                    payload["complexity"] = json!(v);
                }
                if let Some(v) = estimated_edits {
                    payload["estimated_edits"] = json!(v);
                }
                if let Some(v) = parent_task {
                    payload["parent_task_id"] = json!(v);
                }
                if let Some(v) = cycle {
                    payload["cycle_id"] = json!(v);
                }
                if let Some(v) = agent_id {
                    payload["agent_id"] = json!(v);
                }
                if let Some(ref text) = comment {
                    payload["_comment"] = json!(text);
                    payload["_author"] = json!(assignee.as_deref().unwrap_or(&agent));
                }
                if let Some(v) = tier {
                    payload["tier"] = json!(v);
                }
                if let Some(v) = scenario_id {
                    payload["scenario_id"] = json!(v);
                }
                if let Some(v) = evidence {
                    payload["evidence"] = json!(v);
                }
                if let Some(v) = batch_id {
                    payload["batch_id"] = json!(v);
                }
                output(
                    &client::request(&c, "PATCH", &format!("/tasks/{id}"), Some(payload)).await?,
                );
            }
            TaskAction::Delete { id } => {
                output(&client::request(&c, "DELETE", &format!("/tasks/{id}"), None).await?);
            }
            TaskAction::AppendBody { id, text } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        &format!("/tasks/{id}/body"),
                        Some(json!({"text": text})),
                    )
                    .await?,
                );
            }
            TaskAction::Search { query, mode, limit } => {
                let qs = format!("?q={}&mode={}&limit={limit}", urlenc(&query), urlenc(&mode));
                output(&client::get(&c, &format!("/tasks/search{qs}")).await?);
            }
            TaskAction::Complete {
                id,
                evidence,
                comment,
                agent,
            } => {
                task_transition(
                    &c,
                    &id,
                    "done",
                    comment.as_deref(),
                    Some(evidence.as_str()),
                    &agent,
                    output,
                )
                .await?;
            }
            TaskAction::Cancel { id, reason, agent } => {
                task_transition(
                    &c,
                    &id,
                    "cancelled",
                    reason.as_deref(),
                    None,
                    &agent,
                    output,
                )
                .await?;
            }
            TaskAction::Block { id, reason, agent } => {
                task_transition(&c, &id, "blocked", reason.as_deref(), None, &agent, output)
                    .await?;
            }
            TaskAction::Unblock { id, comment, agent } => {
                task_transition(&c, &id, "todo", comment.as_deref(), None, &agent, output).await?;
            }
            TaskAction::Decompose {
                id,
                max_depth,
                strategy,
                dry_run,
                accept,
            } => {
                handle_decompose(&c, &id, max_depth, &strategy, dry_run, accept.as_deref()).await?;
            }
            TaskAction::Tree {
                id,
                depth,
                envelope_summary,
                format,
            } => {
                handle_tree(&c, &id, depth, envelope_summary, &format).await?;
            }
            TaskAction::Ancestors {
                id,
                depth,
                no_envelope,
                format,
            } => {
                let mut path = format!("/tasks/{id}/ancestors");
                let mut qs: Vec<String> = Vec::new();
                if depth > 0 {
                    qs.push(format!("depth={depth}"));
                }
                if no_envelope {
                    qs.push("include_envelope=false".into());
                }
                if !qs.is_empty() {
                    path.push('?');
                    path.push_str(&qs.join("&"));
                }
                let val = client::get(&c, &path).await?;
                output_fmt(&val, &format);
            }
            TaskAction::Descendants {
                id,
                depth,
                order,
                no_envelope,
                format,
            } => {
                let mut qs = vec![format!("depth={depth}"), format!("order={order}")];
                if no_envelope {
                    qs.push("include_envelope=false".into());
                }
                let path = format!("/tasks/{id}/descendants?{}", qs.join("&"));
                let val = client::get(&c, &path).await?;
                output_fmt(&val, &format);
            }
            TaskAction::Stats { batch_id } => {
                // batch_id is constrained to 26-char Crockford base32 (validated
                // server-side), so percent-encoding is a no-op for valid inputs.
                let path = format!("/tasks/stats?batch_id={batch_id}");
                output(&client::get(&c, &path).await?);
            }
        },

        Command::Knowledge { action } => match action {
            ArtifactAction::Create {
                title,
                r#type,
                task,
                unit,
                plan,
                content,
                content_format,
                parent,
            } => {
                output(&client::request(&c, "POST", "/knowledge", Some(json!({
                    "type": r#type, "title": title, "task_id": task, "unit_id": unit,
                    "plan_id": plan, "content": content.unwrap_or_default(), "content_format": content_format,
                    "parent_id": parent,
                }))).await?);
            }
            ArtifactAction::View { id } => {
                output(&client::get(&c, &format!("/knowledge/{id}")).await?)
            }
            ArtifactAction::Update {
                id,
                title,
                content,
                content_format,
                created_by,
            } => {
                let mut payload = serde_json::Map::new();
                if let Some(v) = title {
                    payload.insert("title".into(), json!(v));
                }
                if let Some(v) = content {
                    payload.insert("content".into(), json!(v));
                }
                if let Some(v) = content_format {
                    payload.insert("content_format".into(), json!(v));
                }
                if let Some(v) = created_by {
                    payload.insert("created_by".into(), json!(v));
                }
                output(
                    &client::request(
                        &c,
                        "PATCH",
                        &format!("/knowledge/{id}"),
                        Some(serde_json::Value::Object(payload)),
                    )
                    .await?,
                );
            }
            ArtifactAction::List {
                task,
                unit,
                plan,
                r#type,
            } => {
                let type_opt = r#type;
                let qs = query_string(&[
                    ("task_id", &task),
                    ("unit_id", &unit),
                    ("plan_id", &plan),
                    ("type", &type_opt),
                ]);
                output(&client::get(&c, &format!("/knowledge{qs}")).await?);
            }
            ArtifactAction::Delete { id } => {
                output(&client::request(&c, "DELETE", &format!("/knowledge/{id}"), None).await?);
            }
            ArtifactAction::Search {
                query,
                mode,
                r#type,
                limit,
            } => {
                let type_val = r#type;
                let mut qs = format!("?q={}&mode={}&limit={}", urlenc(&query), mode, limit);
                if let Some(t) = &type_val {
                    qs.push_str(&format!("&type={}", urlenc(t)));
                }
                output(&client::get(&c, &format!("/knowledge/search{qs}")).await?);
            }
            ArtifactAction::Import {
                cwd,
                plan,
                unit,
                dry_run,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/knowledge/import",
                        Some(json!({
                            "cwd": cwd, "plan_id": plan, "unit_id": unit, "dry_run": dry_run,
                        })),
                    )
                    .await?,
                );
            }
            ArtifactAction::Export { cwd, plan, unit } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/knowledge/export",
                        Some(json!({
                            "cwd": cwd, "plan_id": plan, "unit_id": unit,
                        })),
                    )
                    .await?,
                );
            }
        },

        // ===== Run =====
        Command::Run { action } => match action {
            RunAction::Start {
                task,
                session_id,
                agent,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/runs",
                        Some(json!({
                            "task_id": task, "session_id": session_id, "agent": agent,
                        })),
                    )
                    .await?,
                );
            }
            RunAction::Finish { id, result, notes } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        &format!("/runs/{id}/finish"),
                        Some(json!({
                            "result": result, "notes": notes,
                        })),
                    )
                    .await?,
                );
            }
            RunAction::View { id } => output(&client::get(&c, &format!("/runs/{id}")).await?),
            RunAction::List { task, session_id } => {
                let qs = query_string(&[("task_id", &task), ("session_id", &session_id)]);
                output(&client::get(&c, &format!("/runs{qs}")).await?);
            }
        },

        // ===== Comment =====
        Command::Comment { action } => match action {
            CommentAction::Create {
                body,
                task,
                unit,
                plan,
                author,
                label,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/comments",
                        Some(json!({
                            "author": author, "body": body,
                            "task_id": task, "unit_id": unit, "plan_id": plan,
                            "label": label,
                        })),
                    )
                    .await?,
                );
            }
            CommentAction::List { task, unit, plan } => {
                let qs =
                    query_string(&[("task_id", &task), ("unit_id", &unit), ("plan_id", &plan)]);
                output(&client::get(&c, &format!("/comments{qs}")).await?);
            }
            CommentAction::Delete { id } => {
                output(&client::request(&c, "DELETE", &format!("/comments/{id}"), None).await?);
            }
            CommentAction::Update { id, body } => {
                output(
                    &client::request(
                        &c,
                        "PATCH",
                        &format!("/comments/{id}"),
                        Some(json!({ "body": body })),
                    )
                    .await?,
                );
            }
        },

        // ===== Question =====
        Command::Question { action } => match action {
            QuestionAction::Create {
                body,
                plan,
                unit,
                task,
                kind,
                origin,
                asked_by,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/questions",
                        Some(json!({
                            "plan_id": plan, "unit_id": unit, "task_id": task,
                            "kind": kind, "origin": origin, "body": body, "asked_by": asked_by,
                        })),
                    )
                    .await?,
                );
            }
            QuestionAction::Answer { id, text, by } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        &format!("/questions/{id}/answer"),
                        Some(json!({
                            "answer": text, "answered_by": by,
                        })),
                    )
                    .await?,
                );
            }
            QuestionAction::View { id } => {
                output(&client::get(&c, &format!("/questions/{id}")).await?)
            }
            QuestionAction::List {
                plan,
                unit,
                task,
                pending,
            } => {
                let pending_str = pending.map(|b| b.to_string());
                let qs = query_string(&[
                    ("plan_id", &plan),
                    ("unit_id", &unit),
                    ("task_id", &task),
                    ("pending", &pending_str),
                ]);
                output(&client::get(&c, &format!("/questions{qs}")).await?);
            }
        },

        // ===== Dashboard views (FIX-CLI-102) =====
        Command::Timeline { project } => {
            let qs = query_string(&[("project_id", &project)]);
            output(&client::get(&c, &format!("/dashboard/timeline{qs}")).await?);
        }
        Command::Board { project } => {
            let qs = query_string(&[("project_id", &project)]);
            output(&client::get(&c, &format!("/dashboard/board{qs}")).await?);
        }
        Command::Wiki { project } => {
            let qs = query_string(&[("project_id", &project)]);
            output(&client::get(&c, &format!("/dashboard/wiki{qs}")).await?);
        }
        Command::Summary { project } => {
            let qs = query_string(&[("project_id", &project)]);
            output(&client::get(&c, &format!("/dashboard/summary{qs}")).await?);
        }

        // ===== Watch (FIX-CLI-102) =====
        Command::Watch {
            project,
            task,
            cycle,
            format: _fmt,
        } => {
            let qs = query_string(&[
                ("project_id", &project),
                ("task_id", &task),
                ("cycle_id", &cycle),
            ]);
            output(&client::get(&c, &format!("/events{qs}")).await?);
        }

        // ===== Replay (FIX-CLI-102) =====
        Command::Replay { task, limit } => {
            let qs = format!("?task_id={}&limit={}", urlenc(&task), limit);
            output(&client::get(&c, &format!("/runs/replay{qs}")).await?);
        }

        // ===== Backup / Restore / Migrate (FIX-CLI-102) =====
        Command::Backup {
            output: out_path,
            project,
        } => {
            output(
                &client::request(
                    &c,
                    "POST",
                    "/backup",
                    Some(json!({ "output": out_path, "project_id": project })),
                )
                .await?,
            );
        }
        Command::Restore {
            input,
            merge,
            dry_run,
        } => {
            output(
                &client::request(
                    &c,
                    "POST",
                    "/restore",
                    Some(json!({ "input": input, "merge": merge, "dry_run": dry_run })),
                )
                .await?,
            );
        }
        Command::Migrate { dry_run } => {
            output(
                &client::request(&c, "POST", "/migrate", Some(json!({ "dry_run": dry_run })))
                    .await?,
            );
        }

        // ===== Config (FIX-CLI-102) =====
        Command::Config { action } => match action {
            ConfigAction::Get { key } => {
                output(&client::get(&c, &format!("/config/{}", urlenc(&key))).await?);
            }
            ConfigAction::Set { key, value } => {
                output(
                    &client::request(
                        &c,
                        "PUT",
                        &format!("/config/{}", urlenc(&key)),
                        Some(json!({ "value": value })),
                    )
                    .await?,
                );
            }
            ConfigAction::Unset { key } => {
                output(
                    &client::request(&c, "DELETE", &format!("/config/{}", urlenc(&key)), None)
                        .await?,
                );
            }
            ConfigAction::List => {
                output(&client::get(&c, "/config").await?);
            }
        },

        // ===== Self-update / version-check (FIX-CLI-102) =====
        Command::Update { dry_run, version } => {
            output(
                &client::request(
                    &c,
                    "POST",
                    "/self-update",
                    Some(json!({ "dry_run": dry_run, "version": version })),
                )
                .await?,
            );
        }
        Command::VersionCheck => {
            output(&client::get(&c, "/version-check").await?);
        }

        // ===== Knowledge shortcuts =====
        Command::FindSimilar {
            query,
            limit,
            project,
        } => {
            let qs = format!(
                "?q={}&limit={}{}",
                urlenc(&query),
                limit,
                project
                    .as_deref()
                    .map(|p| format!("&project_id={}", urlenc(p)))
                    .unwrap_or_default()
            );
            output(&client::get(&c, &format!("/tasks/similar{qs}")).await?);
        }
        Command::GetTaskContext { id } => {
            output(&client::get(&c, &format!("/tasks/{id}/context")).await?);
        }
        Command::GetRecentDecisions { project, limit } => {
            let mut path = String::from("/knowledge?type=decision");
            if let Some(pid) = project.as_deref() {
                path.push_str(&format!("&project_id={}", urlenc(pid)));
            }
            let val = client::get(&c, &path).await?;
            let mut arr: Vec<serde_json::Value> = val.as_array().cloned().unwrap_or_default();
            arr.sort_by(|a, b| {
                let aa = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                let bb = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                bb.cmp(aa)
            });
            arr.truncate(limit as usize);
            output(&serde_json::Value::Array(arr));
        }

        // ===== Discover-loop (PDD v3.0) =====
        Command::DiscoverLoop { action } => match action {
            // A. Plan/cycle/unit auto-generation ----
            DiscoverAction::Start {
                project,
                domain,
                round,
                areas,
                description,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/discover-loop/start",
                        Some(json!({
                            "project_id": project,
                            "domain": domain,
                            "round": round,
                            "unit_areas": areas,
                            "description": description,
                        })),
                    )
                    .await?,
                );
            }
            DiscoverAction::NextRound {
                previous_plan,
                domain,
                areas,
                round,
            } => {
                output(
                    &client::request(
                        &c,
                        "POST",
                        "/discover-loop/next-round",
                        Some(json!({
                            "previous_plan_id": previous_plan,
                            "domain": domain,
                            "unit_areas": areas,
                            "round": round,
                        })),
                    )
                    .await?,
                );
            }
            // B. Dispatch metadata + TSV schema validation ----
            DiscoverAction::DispatchPlan { plan, batch_size } => {
                let qs = format!("?plan_id={}&batch_size={}", urlenc(&plan), batch_size);
                output(&client::get(&c, &format!("/discover-loop/dispatch-plan{qs}")).await?);
            }
            DiscoverAction::VerifyTsv { path } => {
                let tsv = std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("failed to read TSV file {:?}: {e}", path))?;
                let result = client::request(
                    &c,
                    "POST",
                    "/discover-loop/verify-tsv",
                    Some(json!({ "tsv": tsv })),
                )
                .await?;
                // Exit 1 if validation failed (for scripting use).
                let valid = result
                    .get("valid")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                output(&result);
                if !valid {
                    std::process::exit(1);
                }
            }
            DiscoverAction::BatchId => {
                let result = client::request(&c, "POST", "/discover-loop/batch-id", None).await?;
                // Print just the batch_id string in quiet mode; full JSON otherwise.
                if quiet {
                    if let Some(bid) = result.get("batch_id").and_then(|v| v.as_str()) {
                        println!("{bid}");
                    }
                } else {
                    output(&result);
                }
            }
            // C. Bulk sync transcription ----
            DiscoverAction::Sync {
                path,
                unit,
                cycle,
                assignee,
            } => {
                let tsv = std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("failed to read TSV file {:?}: {e}", path))?;
                // R3 DOGFOOD-028 fix: auto-set CLAWKET_SYNC_CONTEXT for this
                // CLI process so any hook check that fires for child processes
                // spawned by the sync (or by the agent that invoked us during
                // the same shell turn) sees the gate trigger. R3 DOGFOOD-029
                // fix: unset it on completion (success or failure).
                let prev_sync_ctx = std::env::var("CLAWKET_SYNC_CONTEXT").ok();
                // SAFETY: single-threaded CLI startup phase; std::env::set_var
                // is sound here. The sync HTTP call is the only awaited future
                // afterwards, and we restore on completion.
                unsafe {
                    std::env::set_var("CLAWKET_SYNC_CONTEXT", "bulk-sync");
                }
                let sync_result = client::request(
                    &c,
                    "POST",
                    "/discover-loop/sync",
                    Some(json!({
                        "tsv": tsv,
                        "unit_id": unit,
                        "cycle_id": cycle,
                        "assignee": assignee,
                    })),
                )
                .await;
                // Always restore/unset, even on error (R3 DOGFOOD-029).
                unsafe {
                    match prev_sync_ctx {
                        Some(prev) => std::env::set_var("CLAWKET_SYNC_CONTEXT", prev),
                        None => std::env::remove_var("CLAWKET_SYNC_CONTEXT"),
                    }
                }
                output(&sync_result?);
            }
            // D. 3-way convergence query ----
            DiscoverAction::Status { plan, project } => {
                let qs = match (plan.as_deref(), project.as_deref()) {
                    (Some(p), _) => format!("?plan_id={}", urlenc(p)),
                    (_, Some(p)) => format!("?project_id={}", urlenc(p)),
                    _ => String::new(),
                };
                output(&client::get(&c, &format!("/discover-loop/status{qs}")).await?);
            }
            DiscoverAction::Converged { plan, project } => {
                let qs = match (plan.as_deref(), project.as_deref()) {
                    (Some(p), _) => format!("?plan_id={}", urlenc(p)),
                    (_, Some(p)) => format!("?project_id={}", urlenc(p)),
                    _ => String::new(),
                };
                let result = client::get(&c, &format!("/discover-loop/converged{qs}")).await?;
                let converged_val = result
                    .get("converged")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                output(&result);
                // Exit 1 when not yet converged (scriptable gate).
                if !converged_val {
                    std::process::exit(1);
                }
            }
            DiscoverAction::Rounds { project } => {
                output(
                    &client::get(&c, &format!("/discover-loop/rounds/{}", urlenc(&project)))
                        .await?,
                );
            }
        },
    }

    Ok(())
}

/// Thin wrapper: runs `run_main()` and converts any propagated error into
/// the appropriate exit code (FIX-CLI-008).
#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Install panic hook before anything else so panics exit 70 (EX_SOFTWARE).
    error::install_panic_hook();
    if let Err(e) = run_main().await {
        eprintln!("ERROR: {e:#}");
        error::exit_from_error(&e);
    }
}

async fn handle_decompose(
    c: &client::HttpClient,
    task_id: &str,
    max_depth: u32,
    strategy: &str,
    dry_run: bool,
    accept: Option<&str>,
) -> Result<()> {
    use commands::task::decompose::{
        AtomicGate, apply_accept, apply_size_cap, build_suggestions, check_atomic_size_hint,
        check_policy_violations, extract_success_criteria, is_manual_policy, parse_accept,
    };

    let task = client::get(c, &format!("/tasks/{task_id}")).await?;
    let parent_title = task
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("(untitled)")
        .to_string();
    let parent_unit_id = task
        .get("unit_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    // LM-265 / L1.3.c — read the strict-format size hint and policy from
    // the task row (mirrored from the envelope at import time, see
    // LM-263). Both columns have SQLite DEFAULTs (`small`/`auto`) so
    // legacy tasks that pre-date migration 008 land on safe values
    // rather than `null`.
    let atomic_size_hint = task
        .get("atomic_size_hint")
        .and_then(|v| v.as_str())
        .unwrap_or("small")
        .to_string();
    let decomposition_policy_field = task
        .get("decomposition_policy")
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_string();

    // Atomic gate: refuse before doing any envelope work. The strict-
    // format author already declared this task atomic — proposing
    // sub-tasks here would just produce noise that an `--accept` could
    // commit, violating the original scope contract.
    if let AtomicGate::Refuse {
        size_hint,
        suggestion,
    } = check_atomic_size_hint(&atomic_size_hint)
    {
        output_fmt(
            &json!({
                "error": "already_atomic",
                "parent_task": task_id,
                "atomic_size_hint": size_hint,
                "suggestion": suggestion,
            }),
            "json",
        );
        anyhow::bail!(
            "task {task_id} is atomic (atomic_size_hint=\"{size_hint}\") — refusing decompose"
        );
    }

    let env_resp = client::get(c, &format!("/tasks/{task_id}/envelope?resolve=true")).await?;
    let envelope = env_resp
        .get("resolved_envelope")
        .cloned()
        .or_else(|| env_resp.get("raw_envelope").cloned())
        .ok_or_else(|| {
            anyhow::anyhow!("task {task_id} has no envelope — `clawket task envelope set` first")
        })?;

    let criteria = extract_success_criteria(&envelope);
    let mut suggestions = build_suggestions(&parent_title, &criteria, strategy);
    let mut violations = check_policy_violations(&envelope, &mut suggestions, max_depth);
    // Apply the size-proportional cap *after* the envelope's own
    // max_subtasks: the smaller of the two wins, and we record both
    // truncation events so the user understands which knob caused the
    // cut.
    if let Some(v) = apply_size_cap(&mut suggestions, &atomic_size_hint) {
        violations.push(v);
    }

    // `decomposition_policy = "manual"` forces dry-run regardless of
    // caller flags. Surface this as a `forced_dry_run` violation so the
    // user can see why their `--accept ALL` was downgraded.
    let manual_policy = is_manual_policy(&decomposition_policy_field);
    let effective_dry_run = dry_run || manual_policy;
    if manual_policy {
        violations.push(json!({
            "field": "decomposition_policy",
            "severity": "warning",
            "message": "decomposition_policy=\"manual\" forces dry-run; \
                        --accept will not create sub-tasks. \
                        Re-author the plan with policy=\"auto\" to enable creation.",
        }));
    }

    let preview = json!({
        "parent_task": task_id,
        "parent_title": parent_title,
        "strategy": strategy,
        "max_depth": max_depth,
        "atomic_size_hint": atomic_size_hint,
        "decomposition_policy": decomposition_policy_field,
        "suggestions": suggestions,
        "violations": violations,
    });

    if effective_dry_run || accept.is_none() {
        output_fmt(&preview, "json");
        if violations
            .iter()
            .any(|v| v.get("severity").and_then(|s| s.as_str()) == Some("error"))
        {
            anyhow::bail!("decomposition has errors — fix the envelope before --accept");
        }
        if accept.is_some() && manual_policy {
            // The user passed `--accept` but decomposition_policy=manual
            // overrode it. Surface this on stderr so the discrepancy
            // between the requested action and what happened is visible.
            eprintln!(
                "decomposition_policy=\"manual\" — `--accept` ignored, dry-run preview only."
            );
        } else if accept.is_none() {
            eprintln!(
                "preview only — re-run with `--accept ALL` or `--accept 1,3` to create sub-tasks"
            );
        }
        return Ok(());
    }

    if violations
        .iter()
        .any(|v| v.get("severity").and_then(|s| s.as_str()) == Some("error"))
    {
        output_fmt(&preview, "json");
        anyhow::bail!("decomposition has errors — refusing to create sub-tasks");
    }

    let acc = parse_accept(accept.unwrap())?;
    let accepted = apply_accept(suggestions, &acc);
    if accepted.is_empty() {
        anyhow::bail!("--accept produced no surviving suggestions (all out of range?)");
    }

    let mut created = Vec::new();
    for s in &accepted {
        let title = s.get("title").and_then(|v| v.as_str()).unwrap_or_default();
        let rationale = s.get("rationale").and_then(|v| v.as_str()).unwrap_or("");
        let mut payload = serde_json::Map::new();
        payload.insert("title".into(), json!(title));
        payload.insert("body".into(), json!(rationale));
        payload.insert("parent_task_id".into(), json!(task_id));
        if let Some(ref u) = parent_unit_id {
            payload.insert("unit_id".into(), json!(u));
        }
        let resp = client::request(
            c,
            "POST",
            "/tasks",
            Some(serde_json::Value::Object(payload)),
        )
        .await?;
        created.push(json!({
            "id": resp.get("id").cloned().unwrap_or(json!(null)),
            "ticket": resp.get("ticket").cloned().unwrap_or(json!(null)),
            "title": title,
        }));
    }

    output_fmt(
        &json!({
            "parent_task": task_id,
            "created": created,
            "violations": violations,
        }),
        "json",
    );
    Ok(())
}

async fn handle_tree(
    c: &client::HttpClient,
    task_id: &str,
    depth: u32,
    envelope_summary: bool,
    format: &str,
) -> Result<()> {
    use commands::task::tree::{nodes_from_subtree, render_tree_lines};

    let include_env = if envelope_summary { "true" } else { "false" };
    let raw = client::get(
        c,
        &format!("/tasks/{task_id}/subtree?depth={depth}&order=dfs&include_envelope={include_env}"),
    )
    .await?;

    if format == "json" {
        output_fmt(&raw, "json");
        return Ok(());
    }

    let nodes = nodes_from_subtree(&raw);
    if nodes.is_empty() {
        anyhow::bail!("subtree empty — task {task_id} not found?");
    }
    for line in render_tree_lines(&nodes, envelope_summary) {
        println!("{line}");
    }
    Ok(())
}

/// Fetch a full plan bundle (plan + units + tasks + envelopes) and render
/// it as markdown / JSON / YAML. The DB is the single source of truth;
/// this command turns the rendered markdown into a generated view (LM-86).
async fn handle_plan_export(
    c: &client::HttpClient,
    plan_id: &str,
    format: &str,
    out_path: Option<&str>,
    include_knowledge: bool,
) -> Result<()> {
    use commands::plan::export::{
        PlanBundle, TaskBundle, UnitBundle, render_json, render_markdown,
    };

    let plan = client::get(c, &format!("/plans/{plan_id}")).await?;
    let units_raw = client::get(c, &format!("/units?plan_id={plan_id}")).await?;
    let units_arr = units_raw.as_array().cloned().unwrap_or_default();

    let mut units: Vec<UnitBundle> = Vec::with_capacity(units_arr.len());
    for unit in units_arr {
        let unit_id = match unit.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let tasks_raw = client::get(c, &format!("/tasks?unit_id={unit_id}")).await?;
        let tasks_arr = tasks_raw.as_array().cloned().unwrap_or_default();
        let mut tasks: Vec<TaskBundle> = Vec::with_capacity(tasks_arr.len());
        for task in tasks_arr {
            let task_id = match task.get("id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let envelope =
                match client::get(c, &format!("/tasks/{task_id}/envelope?resolve=true")).await {
                    Ok(resp) => resp
                        .get("resolved_envelope")
                        .cloned()
                        .or_else(|| resp.get("raw_envelope").cloned())
                        .unwrap_or(serde_json::Value::Null),
                    Err(_) => serde_json::Value::Null,
                };
            tasks.push(TaskBundle { task, envelope });
        }
        units.push(UnitBundle { unit, tasks });
    }

    let knowledge = if include_knowledge {
        client::get(c, &format!("/knowledge?plan_id={plan_id}"))
            .await
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let bundle = PlanBundle {
        plan,
        units,
        knowledge,
    };

    let rendered: String = match format {
        "md" | "markdown" => render_markdown(&bundle),
        "json" => serde_json::to_string_pretty(&render_json(&bundle))?,
        "yaml" => {
            let mut buf = String::new();
            json_to_yaml(&render_json(&bundle), 0, &mut buf);
            buf
        }
        other => anyhow::bail!("unsupported --format `{other}` (md | json | yaml)"),
    };

    if let Some(path) = out_path {
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).context("failed to create --output parent dir")?;
        }
        std::fs::write(path, rendered).context("failed to write --output file")?;
        eprintln!("wrote {path}");
    } else {
        print!("{rendered}");
    }
    Ok(())
}

/// Minimal JSON→YAML emitter mirroring `print_yaml` semantics but writing
/// into a string buffer so the caller can `--output FILE` it. Keeps the
/// emitter footprint to zero deps; adequate for the export shape.
fn json_to_yaml(val: &serde_json::Value, indent: usize, out: &mut String) {
    use serde_json::Value;
    let pad = "  ".repeat(indent);
    match val {
        Value::Null => out.push_str(&format!("{pad}null\n")),
        Value::Bool(b) => out.push_str(&format!("{pad}{b}\n")),
        Value::Number(n) => out.push_str(&format!("{pad}{n}\n")),
        Value::String(s) => {
            if s.contains('\n') || s.contains(':') || s.contains('#') {
                out.push_str(&format!("{pad}{:?}\n", s));
            } else {
                out.push_str(&format!("{pad}{s}\n"));
            }
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str(&format!("{pad}[]\n"));
                return;
            }
            for item in arr {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{pad}-\n"));
                        json_to_yaml(item, indent + 1, out);
                    }
                    _ => {
                        let mut tmp = String::new();
                        json_to_yaml(item, 0, &mut tmp);
                        out.push_str(&format!("{pad}- {}", tmp.trim_start()));
                    }
                }
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str(&format!("{pad}{{}}\n"));
                return;
            }
            for (k, v) in map {
                match v {
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{pad}{k}:\n"));
                        json_to_yaml(v, indent + 1, out);
                    }
                    _ => {
                        let mut tmp = String::new();
                        json_to_yaml(v, 0, &mut tmp);
                        out.push_str(&format!("{pad}{k}: {}", tmp.trim_start()));
                    }
                }
            }
        }
    }
}

/// PATCH body for `clawket project enable|disable`. Single-purpose so the
/// dispatcher and the unit test agree on the wire shape — daemon accepts
/// `{"enabled": 0 | 1}` (int per `daemon/src/routes/projects.rs`).
fn project_enabled_body(enabled: i64) -> serde_json::Value {
    serde_json::json!({"enabled": enabled})
}

#[cfg(test)]
mod project_enable_disable_tests {
    use super::project_enabled_body;
    use serde_json::json;

    #[test]
    fn disable_emits_enabled_zero() {
        assert_eq!(project_enabled_body(0), json!({"enabled": 0}));
    }

    #[test]
    fn enable_emits_enabled_one() {
        assert_eq!(project_enabled_body(1), json!({"enabled": 1}));
    }
}

/// Best-effort drift banner for `task view`. Failures (no envelope,
/// target_repo unregistered, git unavailable) are silent — the surrounding
/// command must still succeed even if drift cannot be computed.
async fn emit_drift_banner(c: &client::HttpClient, task_id: &str) {
    let drift = match client::get(c, &format!("/tasks/{task_id}/drift")).await {
        Ok(v) => v,
        Err(_) => return,
    };
    let color =
        std::env::var("NO_COLOR").is_err() && std::io::IsTerminal::is_terminal(&std::io::stderr());
    if let Some(banner) = commands::execute::drift_warning::format(&drift, color) {
        eprintln!("{banner}");
    }
}
