use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 1;

/// Single source of truth for the FTS5 schema. Used both at first-time
/// `init_schema` and when `rebuild` drops + recreates the FTS table to wipe
/// segment fragmentation.
pub const FTS5_CREATE_SQL: &str = "
    CREATE VIRTUAL TABLE messages_fts USING fts5(
        content,
        role        UNINDEXED,
        block_kind  UNINDEXED,
        session_id  UNINDEXED,
        msg_index   UNINDEXED,
        timestamp   UNINDEXED,
        tokenize='trigram'
    );
";

/// Root directory for chist's on-disk state (index.db, sync.log, ...).
///
/// Resolution order:
///   1. `$CHIST_CACHE_DIR` — explicit override, used by tests and by power
///      users who want to relocate the index.
///   2. `$XDG_CACHE_HOME/chist` — picked up on Linux *and* macOS, since
///      `dirs::cache_dir()` ignores XDG on macOS and that breaks tests that
///      stage a fake $HOME under tempdir.
///   3. `dirs::cache_dir()/chist` — `~/.cache/chist` on Linux,
///      `~/Library/Caches/chist` on macOS.
pub fn cache_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CHIST_CACHE_DIR") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("chist"));
        }
    }
    let cache = dirs::cache_dir().context("could not resolve cache directory")?;
    Ok(cache.join("chist"))
}

pub fn db_path() -> Result<PathBuf> {
    Ok(cache_dir()?.join("index.db"))
}

pub fn open() -> Result<Connection> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create cache dir {}", parent.display()))?;
    }
    open_at(&path)
}

pub fn open_at(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open SQLite at {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT
        );

        CREATE TABLE IF NOT EXISTS sessions (
            session_id          TEXT PRIMARY KEY,
            claude_session_id   TEXT NOT NULL,
            is_subagent         INTEGER NOT NULL DEFAULT 0,
            file_path           TEXT NOT NULL,
            file_mtime          INTEGER NOT NULL,
            file_size           INTEGER,
            cwd                 TEXT,
            project_dir         TEXT,
            git_branch          TEXT,
            started_at          INTEGER,
            last_activity       INTEGER,
            message_count       INTEGER,
            user_message_count  INTEGER,
            custom_title        TEXT,
            ai_title            TEXT,
            first_user_message  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_cwd ON sessions(cwd);
        CREATE INDEX IF NOT EXISTS idx_sessions_last_activity ON sessions(last_activity DESC);
        CREATE INDEX IF NOT EXISTS idx_sessions_mtime ON sessions(file_mtime);
        CREATE INDEX IF NOT EXISTS idx_sessions_file_path ON sessions(file_path);
        CREATE INDEX IF NOT EXISTS idx_sessions_is_subagent ON sessions(is_subagent);

        "#,
    )?;
    // Created via the central FTS5_CREATE_SQL so rebuild can DROP + recreate
    // from the exact same shape.
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        conn.execute_batch(FTS5_CREATE_SQL)?;
    }

    let current: Option<i64> = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .ok();
    match current {
        Some(v) if v == SCHEMA_VERSION => {}
        Some(v) => anyhow::bail!(
            "index schema version {} is newer/older than supported {}; run `chist rebuild`",
            v,
            SCHEMA_VERSION
        ),
        None => {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?)",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
    }
    Ok(())
}

/// Drop and recreate the FTS5 virtual table — fast wipe that also discards
/// any accumulated segment fragmentation. Used by `rebuild`.
pub fn recreate_fts_table(conn: &Connection) -> Result<()> {
    conn.execute_batch("DROP TABLE IF EXISTS messages_fts;")?;
    conn.execute_batch(FTS5_CREATE_SQL)?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v = conn
        .query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get::<_, String>(0))
        .ok();
    Ok(v)
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

pub fn print_stats() -> Result<()> {
    let path = db_path()?;
    if !path.exists() {
        println!("{{");
        println!("  \"db_path\": \"{}\",", path.display());
        println!("  \"exists\": false");
        println!("}}");
        return Ok(());
    }
    let conn = open()?;
    let session_count: i64 = conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;
    let fts_rows: i64 =
        conn.query_row("SELECT count(*) FROM messages_fts", [], |r| r.get(0))?;
    let last_scan = get_meta(&conn, "last_full_scan_at")?.unwrap_or_else(|| "null".to_string());
    let db_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    println!("{{");
    println!("  \"db_path\": \"{}\",", path.display());
    println!("  \"db_size_bytes\": {},", db_bytes);
    println!("  \"indexed_sessions\": {},", session_count);
    println!("  \"indexed_message_blocks\": {},", fts_rows);
    println!("  \"last_full_scan_at\": {}", last_scan);
    println!("}}");
    Ok(())
}
