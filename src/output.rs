use serde::Serialize;

#[derive(Serialize)]
pub struct SearchOutput {
    pub query: String,
    pub filters: Filters,
    pub results: Vec<ResultItem>,
    pub stats: Stats,
}

#[derive(Serialize)]
pub struct Filters {
    pub cwd: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: usize,
}

#[derive(Serialize)]
pub struct ResultItem {
    pub session_id: String,
    pub claude_session_id: String,
    pub is_subagent: bool,
    pub cwd: Option<String>,
    pub title: String,
    pub title_source: String,
    pub started_at: Option<String>,
    pub last_activity: Option<String>,
    pub message_count: i64,
    pub snippet: String,
    pub matched_role: String,
    pub matched_block_kind: String,
    pub score: f64,
    pub resume_command: String,
}

#[derive(Serialize)]
pub struct Stats {
    pub total_matches: i64,
    pub indexed_sessions: i64,
    pub scan_duration_ms: u128,
    pub reindex_count: usize,
    pub query_duration_ms: u128,
}

pub fn render_text(out: &SearchOutput) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "query: {}    matches: {} (showing {})    scan {}ms / reindex {} / query {}ms\n",
        out.query,
        out.stats.total_matches,
        out.results.len(),
        out.stats.scan_duration_ms,
        out.stats.reindex_count,
        out.stats.query_duration_ms
    ));
    s.push_str(&"-".repeat(80));
    s.push('\n');
    for (i, r) in out.results.iter().enumerate() {
        s.push_str(&format!(
            "{:>2}. {}{}\n    {} • {} • {} msgs\n    {}\n    $ {}\n\n",
            i + 1,
            r.title,
            if r.is_subagent { "  [subagent]" } else { "" },
            r.cwd.clone().unwrap_or_else(|| "(unknown cwd)".into()),
            r.last_activity.clone().unwrap_or_else(|| "?".into()),
            r.message_count,
            r.snippet.replace('\n', " "),
            r.resume_command,
        ));
    }
    s
}
