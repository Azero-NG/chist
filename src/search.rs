use crate::cli::{Format, SearchOpts};
use crate::config::{self, Config};
use crate::db;
use crate::output::{Filters, ResultItem, SearchOutput, Stats};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rusqlite::params_from_iter;
use std::time::Instant;

pub fn run(opts: SearchOpts) -> Result<()> {
    let conn = db::open()?;
    // Search no longer triggers a synchronous scan — incremental updates are
    // driven by the Stop/SubagentStop hook running `chist sync` in the
    // background. `--no-scan` is now a no-op kept for backward compatibility.
    let _ = opts.no_scan;

    // Load ~/.config/chist/config.toml unless the user explicitly bypassed it.
    // Missing file → empty rules; malformed file is surfaced as an error so
    // the user notices typos.
    let cfg = if opts.no_config {
        Config::default()
    } else {
        config::load()?
    };
    let cfg_active = !opts.no_config && config::config_path().map(|p| p.exists()).unwrap_or(false);

    let since_ts = opts
        .since
        .as_deref()
        .map(parse_user_date)
        .transpose()
        .context("invalid --since")?;
    let until_ts = opts
        .until
        .as_deref()
        .map(parse_user_date)
        .transpose()
        .context("invalid --until")?;

    // FTS5 has its own query DSL with operators (-, AND, OR, NEAR, etc).
    // To make the CLI predictable for arbitrary user input we sanitize into
    // a phrase or whitespace-AND query of quoted tokens.
    let fts_query = sanitize_fts_query(&opts.query);

    let q_start = Instant::now();

    // FTS5 auxiliary functions (bm25/snippet/rank) can only be referenced in
    // a SELECT that has a direct MATCH on the fts table — they fail under
    // aggregates or window expressions. So we use a two-stage SELECT:
    //
    //   1. Inner: pull matched rows ordered by rank; over-fetch (LIMIT 500)
    //      so we have enough variety after deduping by session.
    //   2. Outer: JOIN sessions, GROUP BY session_id (SQLite picks the row
    //      with min rowid per group, which is the best-rank row because the
    //      inner is sorted by rank).
    let inner_limit = (opts.limit * 25).max(200) as i64;
    let mut sql = String::from(
        "SELECT
            s.session_id, s.claude_session_id, s.is_subagent, s.cwd, s.project_dir,
            s.started_at, s.last_activity, s.message_count,
            s.custom_title, s.ai_title, s.first_user_message,
            MIN(m.rank) AS best_score,
            m.snip AS snip,
            m.role AS matched_role,
            m.block_kind AS matched_kind
        FROM (
            SELECT session_id, role, block_kind,
                   snippet(messages_fts, 0, '<<', '>>', '…', 16) AS snip,
                   rank
            FROM messages_fts
            WHERE messages_fts MATCH ?",
    );
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query.clone())];

    // Inner-stage exclusions: drop matched blocks of unwanted role/block_kind
    // before they reach the rank-ordered LIMIT, so we don't waste the over-fetch
    // budget on noise the user already declared uninteresting.
    append_not_in(&mut sql, &mut binds, "role", &cfg.exclude.roles);
    append_not_in(&mut sql, &mut binds, "block_kind", &cfg.exclude.block_kinds);

    sql.push_str(" ORDER BY rank LIMIT ?");
    binds.push(Box::new(inner_limit));

    sql.push_str(
        ") m
        JOIN sessions s ON s.session_id = m.session_id
        WHERE 1=1",
    );

    if let Some(cwd) = &opts.cwd {
        sql.push_str(" AND (s.cwd = ? OR s.cwd LIKE ?)");
        binds.push(Box::new(cwd.clone()));
        binds.push(Box::new(format!("{}/%", cwd.trim_end_matches('/'))));
    }
    if let Some(since) = since_ts {
        sql.push_str(" AND s.last_activity >= ?");
        binds.push(Box::new(since));
    }
    if let Some(until) = until_ts {
        sql.push_str(" AND s.last_activity <= ?");
        binds.push(Box::new(until));
    }

    // Outer-stage exclusions from config.
    append_prefix_exclusions(&mut sql, &mut binds, "s.cwd", &cfg.exclude.cwds);
    append_prefix_exclusions(&mut sql, &mut binds, "s.project_dir", &cfg.exclude.project_dirs);
    append_path_prefix_exclusions(&mut sql, &mut binds, "s.file_path", &cfg.exclude.file_paths);
    append_not_in(&mut sql, &mut binds, "s.session_id", &cfg.exclude.session_ids);
    if cfg.filter.min_message_count > 0 {
        sql.push_str(" AND s.message_count >= ?");
        binds.push(Box::new(cfg.filter.min_message_count));
    }
    if cfg.filter.min_user_message_count > 0 {
        sql.push_str(" AND s.user_message_count >= ?");
        binds.push(Box::new(cfg.filter.min_user_message_count));
    }

    sql.push_str(" GROUP BY s.session_id ORDER BY best_score ASC, s.last_activity DESC LIMIT ?");
    binds.push(Box::new(opts.limit as i64));

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => anyhow::bail!("search query rejected by FTS5: {}", e),
    };
    let rows = stmt.query_map(params_from_iter(binds.iter().map(|b| b.as_ref())), |r| {
        Ok(SqlRow {
            session_id: r.get(0)?,
            claude_session_id: r.get(1)?,
            is_subagent: r.get::<_, i64>(2)? != 0,
            cwd: r.get(3)?,
            _project_dir: r.get(4)?,
            started_at: r.get(5)?,
            last_activity: r.get(6)?,
            message_count: r.get(7)?,
            custom_title: r.get(8)?,
            ai_title: r.get(9)?,
            first_user_message: r.get(10)?,
            best_score: r.get(11)?,
            snippet: r.get(12)?,
            matched_role: r.get(13)?,
            matched_kind: r.get(14)?,
        })
    })?;

    let results: Vec<ResultItem> = rows
        .filter_map(|r| r.ok())
        .map(|r| build_result(r))
        .collect();

    let query_duration = q_start.elapsed();

    // Cheaper than COUNT(DISTINCT) on the matched rowset; reuse what we got.
    let total_matches = if results.len() < opts.limit {
        results.len() as i64
    } else {
        conn.query_row(
            "SELECT count(DISTINCT session_id) FROM messages_fts WHERE messages_fts MATCH ?1",
            [&fts_query],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(results.len() as i64)
    };

    let total_indexed: i64 =
        conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;

    let out = SearchOutput {
        query: opts.query.clone(),
        filters: Filters {
            cwd: opts.cwd.clone(),
            since: opts.since.clone(),
            until: opts.until.clone(),
            limit: opts.limit,
            config_applied: cfg_active,
        },
        results,
        stats: Stats {
            total_matches,
            indexed_sessions: total_indexed,
            // scan_duration_ms / reindex_count remain in the JSON schema for
            // backward compatibility but are always 0 now that search no
            // longer drives incremental scans.
            scan_duration_ms: 0,
            reindex_count: 0,
            query_duration_ms: query_duration.as_millis(),
        },
    };

    match opts.format {
        Format::Json => {
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Format::Text => {
            print!("{}", crate::output::render_text(&out));
        }
    }
    Ok(())
}

struct SqlRow {
    session_id: String,
    claude_session_id: String,
    is_subagent: bool,
    cwd: Option<String>,
    _project_dir: Option<String>,
    started_at: Option<i64>,
    last_activity: Option<i64>,
    message_count: i64,
    custom_title: Option<String>,
    ai_title: Option<String>,
    first_user_message: Option<String>,
    best_score: f64,
    snippet: String,
    matched_role: String,
    matched_kind: String,
}

fn build_result(r: SqlRow) -> ResultItem {
    let (title, title_source) = pick_title(&r);
    let resume_command = build_resume_command(r.cwd.as_deref(), &r.claude_session_id);
    ResultItem {
        session_id: r.session_id,
        claude_session_id: r.claude_session_id,
        is_subagent: r.is_subagent,
        cwd: r.cwd,
        title,
        title_source,
        started_at: r.started_at.and_then(unix_to_iso),
        last_activity: r.last_activity.and_then(unix_to_iso),
        message_count: r.message_count,
        snippet: r.snippet,
        matched_role: r.matched_role,
        matched_block_kind: r.matched_kind,
        // bm25 returns negative numbers (closer to 0 = better). Flip sign so
        // larger = better in the JSON output.
        score: -r.best_score,
        resume_command,
    }
}

fn pick_title(r: &SqlRow) -> (String, String) {
    if let Some(t) = r.custom_title.as_deref().filter(|s| !s.is_empty()) {
        return (t.to_string(), "custom_title".into());
    }
    if let Some(t) = r.ai_title.as_deref().filter(|s| !s.is_empty()) {
        return (t.to_string(), "ai_title".into());
    }
    if let Some(t) = r.first_user_message.as_deref().filter(|s| !s.is_empty()) {
        return (t.to_string(), "first_user_message".into());
    }
    ("(no title)".into(), "none".into())
}

fn build_resume_command(cwd: Option<&str>, session_id: &str) -> String {
    let cwd = cwd.unwrap_or("");
    if cwd.is_empty() {
        format!("claude --resume {}", session_id)
    } else {
        let escaped = shell_escape::escape(cwd.into());
        format!("cd {} && claude --resume {}", escaped, session_id)
    }
}

fn unix_to_iso(ts: i64) -> Option<String> {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|dt: DateTime<Utc>| dt.to_rfc3339())
}

/// Append `AND <column> NOT IN (?, ?, ...)` for an exact-match exclusion list.
/// `column` is a hardcoded SQL fragment from this crate, never user input.
fn append_not_in(
    sql: &mut String,
    binds: &mut Vec<Box<dyn rusqlite::ToSql>>,
    column: &str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }
    sql.push_str(" AND ");
    sql.push_str(column);
    sql.push_str(" NOT IN (");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push('?');
        binds.push(Box::new(v.clone()));
    }
    sql.push(')');
}

/// Append a path-prefix exclusion. Each rule blocks both an exact match and
/// any descendant path, while leaving NULL values (sessions without a cwd)
/// unaffected — without `COALESCE` a NULL column would sink the whole row.
fn append_prefix_exclusions(
    sql: &mut String,
    binds: &mut Vec<Box<dyn rusqlite::ToSql>>,
    column: &str,
    rules: &[String],
) {
    for rule in rules {
        let trimmed = rule.trim_end_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        sql.push_str(&format!(
            " AND COALESCE({0}, '') <> ? AND COALESCE({0}, '') NOT LIKE ?",
            column
        ));
        binds.push(Box::new(trimmed.to_string()));
        binds.push(Box::new(format!("{}/%", trimmed)));
    }
}

/// Append a plain "starts-with" exclusion (no trailing-slash handling). Used
/// for raw `file_path` rules where the user may want to block by absolute
/// file prefix rather than directory boundary.
fn append_path_prefix_exclusions(
    sql: &mut String,
    binds: &mut Vec<Box<dyn rusqlite::ToSql>>,
    column: &str,
    rules: &[String],
) {
    for rule in rules {
        if rule.is_empty() {
            continue;
        }
        sql.push_str(&format!(" AND COALESCE({0}, '') NOT LIKE ?", column));
        binds.push(Box::new(format!("{}%", rule)));
    }
}

/// Convert raw user input into a safe FTS5 MATCH expression.
///
/// We split on whitespace, drop FTS5-meaningful operators, and double-quote
/// each token. Multi-token input becomes an implicit AND. Hyphens, slashes
/// and other punctuation inside a token are preserved by quoting.
fn sanitize_fts_query(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "\"\"".into();
    }
    let mut tokens: Vec<String> = Vec::new();
    for part in raw.split_whitespace() {
        // Strip leading/trailing punctuation that has FTS meaning.
        let cleaned: String = part
            .chars()
            .filter(|c| !matches!(c, '"' | '*' | '(' | ')'))
            .collect();
        if cleaned.is_empty() {
            continue;
        }
        // Double-quote the token so '-', '/', ':' etc. are taken literally.
        tokens.push(format!("\"{}\"", cleaned));
    }
    if tokens.is_empty() {
        "\"\"".into()
    } else {
        tokens.join(" ")
    }
}

/// Accepts ISO8601 (`2026-04-15T10:00:00Z`), date-only (`2026-04-15`), or
/// relative (`7d`, `30d`).
fn parse_user_date(s: &str) -> Result<i64> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix('d') {
        let n: i64 = num.parse().context("invalid day count")?;
        return Ok(Utc::now().timestamp() - n * 86_400);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp());
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = d.and_hms_opt(0, 0, 0).context("date conversion failed")?;
        return Ok(ndt.and_utc().timestamp());
    }
    anyhow::bail!("could not parse date: {}", s)
}
