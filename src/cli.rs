use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "chist", version, about = "Search across past Claude Code conversations")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Full-text search across all indexed Claude Code sessions
    Search(SearchOpts),
    /// Wipe and rebuild the index from scratch
    Rebuild(RebuildOpts),
    /// Incrementally update the index (walkdir + mtime/size diff).
    /// Designed to be invoked from a Stop / SubagentStop hook.
    Sync(SyncOpts),
    /// Install Stop/SubagentStop hooks into ~/.claude/settings.json
    InstallHook,
    /// Remove the chist Stop/SubagentStop hooks from ~/.claude/settings.json
    UninstallHook,
    /// Print index status (session count, last scan, db size)
    Stats,
}

#[derive(Parser, Debug, Clone)]
pub struct SyncOpts {
    /// Bypass the cooldown gate; run even if a sync ran very recently.
    #[arg(long)]
    pub force: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct RebuildOpts {
    /// Emit per-phase timing logs to stderr.
    #[arg(short, long)]
    pub verbose: bool,

    /// Print a progress line every N parsed files (implies --verbose).
    #[arg(long, default_value_t = 0)]
    pub progress_every: usize,
}

#[derive(Parser, Debug, Clone)]
pub struct SearchOpts {
    /// Search query (FTS5 syntax; quote phrases)
    pub query: String,

    /// Restrict results to sessions whose cwd starts with this prefix
    #[arg(long)]
    pub cwd: Option<String>,

    /// Lower bound on last_activity. Accepts ISO8601, YYYY-MM-DD, or 'Nd' (N days ago)
    #[arg(long)]
    pub since: Option<String>,

    /// Upper bound on last_activity. Same formats as --since
    #[arg(long)]
    pub until: Option<String>,

    /// Maximum number of session results to return
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Json)]
    pub format: Format,

    /// Skip the mtime catchup scan; query the index as-is. Useful when
    /// running several searches back-to-back.
    #[arg(long)]
    pub no_scan: bool,

    /// Ignore ~/.config/chist/config.toml exclude/filter rules for this run.
    #[arg(long)]
    pub no_config: bool,

    /// Override `[search] snippet_tokens` for this query. Number of tokens
    /// of context shown around each match (FTS5 clamps to 1..=64).
    #[arg(long)]
    pub snippet_tokens: Option<i64>,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum Format {
    Json,
    Text,
}
