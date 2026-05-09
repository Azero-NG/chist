use crate::db;
use crate::parse::{parse_file, ParsedBlock, ParsedSession};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

pub fn projects_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home.join(".claude").join("projects"))
}

/// Walk ~/.claude/projects/*/*.jsonl recursively (subagent sessions live in
/// nested subagents/ dirs).
pub fn discover_jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    for entry in WalkDir::new(root)
        .follow_links(false)
        .max_depth(8)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|s| s.to_str()) == Some("jsonl")
        {
            out.push(entry.path().to_path_buf());
        }
    }
    out
}

#[derive(Default, Debug)]
pub struct ScanReport {
    pub total_on_disk: usize,
    pub indexed_before: usize,
    pub reindexed: usize,
    pub deleted: usize,
    pub failed: usize,
    pub elapsed_ms: u128,
    pub skipped_cooldown: bool,
    pub cooldown_age_secs: i64,
}

impl ScanReport {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "action": "sync",
            "skipped_cooldown": self.skipped_cooldown,
            "cooldown_age_secs": self.cooldown_age_secs,
            "total_on_disk": self.total_on_disk,
            "indexed_before": self.indexed_before,
            "reindexed": self.reindexed,
            "deleted": self.deleted,
            "failed": self.failed,
            "elapsed_ms": self.elapsed_ms,
        })
    }
}

/// Cooldown window: if the last sync finished within this many seconds, the
/// next invocation is a no-op. Tames Stop/SubagentStop hooks that fire on
/// every turn — back-to-back messages don't each schedule a walkdir.
pub const SYNC_COOLDOWN_SECS: i64 = 30;

/// CLI entrypoint for `chist sync`. Writes a one-line audit log to
/// `~/.cache/chist/sync.log` so background invocations from Stop hooks can
/// be debugged after the fact (stderr is discarded by the hook).
pub fn run_sync(opts: crate::cli::SyncOpts) -> Result<()> {
    let conn = db::open()?;
    let report = match incremental_sync(&conn, opts.force) {
        Ok(r) => r,
        Err(e) => {
            // Log the error before bubbling — so the hook leaves a trace.
            let _ = append_sync_log(&format!("error: {e:#}"));
            return Err(e);
        }
    };

    let line = if report.skipped_cooldown {
        format!(
            "skipped (cooldown, last_sync was {}s ago)",
            report.cooldown_age_secs
        )
    } else {
        format!(
            "done: {}r/{}d/{}f in {}ms ({} on disk, {} indexed)",
            report.reindexed,
            report.deleted,
            report.failed,
            report.elapsed_ms,
            report.total_on_disk,
            report.indexed_before
        )
    };
    let _ = append_sync_log(&line);

    println!("{}", serde_json::to_string_pretty(&report.to_json())?);
    Ok(())
}

/// Walk all jsonl files, reindex changed ones, drop deleted ones. Internal
/// API kept on `&Connection` so tests can drive it without spawning the CLI.
pub fn incremental_sync(conn: &Connection, force: bool) -> Result<ScanReport> {
    let started = Instant::now();
    let mut report = ScanReport::default();

    // Cooldown gate (skipped under --force).
    if !force {
        if let Some(prev) =
            db::get_meta(conn, "last_full_scan_at")?.and_then(|v| v.parse::<i64>().ok())
        {
            let now = chrono::Utc::now().timestamp();
            let age = now - prev;
            if age < SYNC_COOLDOWN_SECS {
                report.indexed_before = conn
                    .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0) as usize;
                report.elapsed_ms = started.elapsed().as_millis();
                report.skipped_cooldown = true;
                report.cooldown_age_secs = age;
                return Ok(report);
            }
        }
    }

    let root = projects_root()?;
    let on_disk = discover_jsonl_files(&root);
    report.total_on_disk = on_disk.len();

    // Build map of indexed paths → (mtime, size). Both fields are checked so
    // that mtime collisions (sub-second writes, restored timestamps) still get
    // caught when the size differs.
    let mut indexed: HashMap<PathBuf, (i64, i64)> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT file_path, file_mtime, file_size FROM sessions")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (p, m, s) = row?;
            indexed.insert(PathBuf::from(p), (m, s));
        }
    }
    report.indexed_before = indexed.len();

    // Build the tokenizer once per sync — Jieba::new allocates the ~5MB
    // dictionary, so caching across reindex_file calls matters when many
    // files changed at once.
    let tokenizer = crate::tokenize::Tokenizer::load_active(conn)?;

    // Reindex new or modified files.
    for path in &on_disk {
        let meta = std::fs::metadata(path).ok();
        let mtime_disk = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size_disk = meta.as_ref().map(|m| m.len() as i64).unwrap_or(0);
        let needs = match indexed.get(path) {
            None => true,
            Some(&(known_mtime, known_size)) => {
                known_mtime != mtime_disk || known_size != size_disk
            }
        };
        if needs {
            match reindex_file(conn, path, &tokenizer) {
                Ok(_) => report.reindexed += 1,
                Err(_) => report.failed += 1,
            }
        }
    }

    // Drop sessions whose files no longer exist.
    let on_disk_set: std::collections::HashSet<&PathBuf> = on_disk.iter().collect();
    let to_delete: Vec<PathBuf> = indexed
        .keys()
        .filter(|p| !on_disk_set.contains(p))
        .cloned()
        .collect();
    if !to_delete.is_empty() {
        let tx = conn.unchecked_transaction()?;
        for p in &to_delete {
            // Each indexed row owns one session_id; clean both tables.
            let session_ids: Vec<String> = tx
                .prepare("SELECT session_id FROM sessions WHERE file_path = ?")?
                .query_map([p.to_string_lossy()], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            for sid in &session_ids {
                tx.execute("DELETE FROM messages_fts WHERE session_id = ?", [sid])?;
            }
            tx.execute(
                "DELETE FROM sessions WHERE file_path = ?",
                [p.to_string_lossy()],
            )?;
            report.deleted += 1;
        }
        tx.commit()?;
    }

    db::set_meta(
        conn,
        "last_full_scan_at",
        &chrono::Utc::now().timestamp().to_string(),
    )?;

    report.elapsed_ms = started.elapsed().as_millis();
    Ok(report)
}

/// Append a single timestamped line to `~/.cache/chist/sync.log`. Errors are
/// swallowed by the caller because Stop-hook runs already discard stderr —
/// failing to log shouldn't take down a sync that otherwise succeeded.
fn append_sync_log(message: &str) -> Result<()> {
    let dir = db::cache_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("sync.log");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)?;
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
    let pid = std::process::id();
    writeln!(f, "{ts}  pid={pid}  {message}")?;
    Ok(())
}

/// Reindex a single jsonl file atomically. The `tokenizer` argument must
/// match the one recorded in `meta.tokenizer_id` so MATCH at query time
/// sees the same token shapes as what we write here.
pub fn reindex_file(
    conn: &Connection,
    path: &Path,
    tokenizer: &crate::tokenize::Tokenizer,
) -> Result<()> {
    let (session, blocks) = parse_file(path)?;
    if session.session_id.is_empty() {
        // Nothing parseable; record minimal stub so we don't keep retrying.
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;

    // Wipe any prior rows for this session_id and file_path.
    tx.execute(
        "DELETE FROM messages_fts WHERE session_id = ?",
        [&session.session_id],
    )?;
    tx.execute(
        "DELETE FROM sessions WHERE session_id = ? OR file_path = ?",
        params![&session.session_id, &path.to_string_lossy()],
    )?;

    insert_session(&tx, &session)?;
    insert_blocks(&tx, &session.session_id, &blocks, tokenizer)?;

    tx.commit()?;
    Ok(())
}

fn insert_session(conn: &Connection, s: &ParsedSession) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (
            session_id, claude_session_id, is_subagent, file_path, file_mtime, file_size,
            cwd, project_dir, git_branch, started_at, last_activity,
            message_count, user_message_count,
            custom_title, ai_title, first_user_message
        ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            s.session_id,
            s.claude_session_id,
            s.is_subagent as i64,
            s.file_path.to_string_lossy(),
            s.file_mtime,
            s.file_size,
            s.cwd,
            s.project_dir,
            s.git_branch,
            s.started_at,
            s.last_activity,
            s.message_count,
            s.user_message_count,
            s.custom_title,
            s.ai_title,
            s.first_user_message,
        ],
    )?;
    Ok(())
}

fn insert_blocks(
    conn: &Connection,
    session_id: &str,
    blocks: &[ParsedBlock],
    tokenizer: &crate::tokenize::Tokenizer,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO messages_fts (content, role, block_kind, session_id, msg_index, timestamp)
         VALUES (?, ?, ?, ?, ?, ?)",
    )?;
    for b in blocks {
        let content = tokenizer.for_index(&b.content);
        stmt.execute(params![
            content,
            b.role,
            b.block_kind,
            session_id,
            b.msg_index,
            b.timestamp,
        ])?;
    }
    Ok(())
}

pub fn rebuild(opts: crate::cli::RebuildOpts) -> Result<()> {
    let verbose = opts.verbose || opts.progress_every > 0;
    let started = Instant::now();
    macro_rules! log {
        ($($arg:tt)*) => {
            if verbose {
                eprintln!("[t={:>7.3}s] {}", started.elapsed().as_secs_f64(), format_args!($($arg)*));
            }
        };
    }
    log!("rebuild starting");

    let t = Instant::now();
    let conn = db::open()?;
    log!("db opened, schema ready ({}ms)", t.elapsed().as_millis());

    // Tokenizer comes from config (default: jieba). Stamp the choice into
    // meta before wiping so subsequent search/sync invocations see it; if
    // rebuild fails midway we still want the meta to reflect the table that
    // was just (re)created.
    let cfg = crate::config::load()?;
    let backend = crate::tokenize::Backend::parse(&cfg.tokenizer.backend)?;
    let tokenizer = crate::tokenize::Tokenizer::new(backend);
    log!("tokenizer backend: {}", backend.id());

    let t = Instant::now();
    // Fast wipe: DROP + recreate the FTS5 table (segment files vanish in O(1),
    // accumulated fragmentation goes with them). Plain DELETE on `sessions`
    // gets SQLite's truncate optimization.
    db::recreate_fts_table(&conn, backend.fts5_clause())?;
    conn.execute_batch("DELETE FROM sessions;")?;
    db::set_meta(&conn, "last_full_scan_at", "0")?;
    db::set_meta(&conn, crate::tokenize::TOKENIZER_META_KEY, backend.id())?;
    log!("tables cleared ({}ms)", t.elapsed().as_millis());

    let t = Instant::now();
    let root = projects_root()?;
    let on_disk = discover_jsonl_files(&root);
    let total = on_disk.len();
    log!("walkdir found {} jsonl files ({}ms)", total, t.elapsed().as_millis());

    // Parallel parse → bounded channel → single-thread writer.
    //
    // SQLite is fundamentally a single-writer store under WAL, so spinning up
    // multiple connections to write concurrently doesn't help. Instead we let
    // rayon saturate CPU on JSONL parsing (the actual bottleneck) and drain
    // results sequentially on the main thread inside one big transaction.
    use rayon::prelude::*;
    use std::sync::mpsc;

    let (tx, rx) =
        mpsc::sync_channel::<(crate::parse::ParsedSession, Vec<crate::parse::ParsedBlock>)>(128);
    let producer_paths = on_disk;

    let producer_started = Instant::now();
    let producer = std::thread::spawn(move || {
        producer_paths
            .into_par_iter()
            .for_each_with(tx, |tx, path| {
                if let Ok(pair) = crate::parse::parse_file(&path) {
                    let _ = tx.send(pair);
                }
            });
    });
    log!("parse worker pool spawned");

    let tx_db = conn.unchecked_transaction()?;
    log!("BEGIN transaction");

    let mut written: usize = 0;
    let mut first_recv_at: Option<std::time::Duration> = None;
    let mut total_recv_wait_ns: u128 = 0;
    let mut total_insert_ns: u128 = 0;
    let progress_every = opts.progress_every;
    loop {
        let recv_start = Instant::now();
        let pair = match rx.recv() {
            Ok(p) => p,
            Err(_) => break,
        };
        total_recv_wait_ns += recv_start.elapsed().as_nanos();
        if first_recv_at.is_none() {
            first_recv_at = Some(started.elapsed());
            log!(
                "first parsed result received ({}ms after start)",
                first_recv_at.unwrap().as_millis()
            );
        }
        let (s, b) = pair;
        if s.session_id.is_empty() {
            continue;
        }
        let ins_start = Instant::now();
        insert_session(&tx_db, &s)?;
        insert_blocks(&tx_db, &s.session_id, &b, &tokenizer)?;
        total_insert_ns += ins_start.elapsed().as_nanos();
        written += 1;
        if progress_every > 0 && written % progress_every == 0 {
            log!(
                "progress: {}/{} written  (cum insert={:.2}s  cum recv-wait={:.2}s)",
                written,
                total,
                total_insert_ns as f64 / 1e9,
                total_recv_wait_ns as f64 / 1e9
            );
        }
    }
    log!(
        "drain done: {} written, cum insert {:.2}s, cum recv-wait {:.2}s",
        written,
        total_insert_ns as f64 / 1e9,
        total_recv_wait_ns as f64 / 1e9
    );

    let t = Instant::now();
    tx_db.commit()?;
    log!("COMMIT done ({}ms)", t.elapsed().as_millis());

    producer.join().expect("parse worker panicked");
    log!(
        "parse worker joined ({:.2}s wall in producer thread)",
        producer_started.elapsed().as_secs_f64()
    );

    db::set_meta(
        &conn,
        "last_full_scan_at",
        &chrono::Utc::now().timestamp().to_string(),
    )?;

    let elapsed_ms = started.elapsed().as_millis();
    log!("DONE — total {}ms", elapsed_ms);

    println!("{{");
    println!("  \"action\": \"rebuild\",");
    println!("  \"total_on_disk\": {},", total);
    println!("  \"indexed\": {},", written);
    println!("  \"failed\": {},", total.saturating_sub(written));
    println!("  \"elapsed_ms\": {}", elapsed_ms);
    println!("}}");
    Ok(())
}
