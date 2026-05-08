use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Aggregated session metadata.
///
/// `session_id` is unique per jsonl file (so subagent files don't collide with
/// their parent). `claude_session_id` is the value `claude --resume` accepts —
/// for main sessions this equals `session_id`, for subagents it equals the
/// parent session's UUID (since you cannot resume a subagent directly).
#[derive(Debug, Default, Clone)]
pub struct ParsedSession {
    pub session_id: String,
    pub claude_session_id: String,
    pub is_subagent: bool,
    pub file_path: PathBuf,
    pub file_mtime: i64,
    pub file_size: i64,
    pub cwd: Option<String>,
    pub project_dir: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<i64>,
    pub last_activity: Option<i64>,
    pub message_count: i64,
    pub user_message_count: i64,
    pub custom_title: Option<String>,
    pub ai_title: Option<String>,
    pub first_user_message: Option<String>,
}

/// One indexable text block within a message.
#[derive(Debug, Clone)]
pub struct ParsedBlock {
    pub role: String,
    pub block_kind: String,
    pub content: String,
    pub msg_index: i64,
    pub timestamp: Option<i64>,
}

const MAX_BLOCK_BYTES: usize = 100_000; // truncate giant tool outputs

#[derive(Deserialize)]
struct LineHeader {
    #[serde(rename = "type")]
    ty: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    timestamp: Option<String>,
    message: Option<Value>,
    #[serde(rename = "customTitle")]
    custom_title: Option<String>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
}

pub fn parse_file(path: &Path) -> Result<(ParsedSession, Vec<ParsedBlock>)> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("stat {} failed", path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let is_subagent = path
        .components()
        .any(|c| c.as_os_str() == "subagents");
    let mut session = ParsedSession {
        is_subagent,
        file_path: path.to_path_buf(),
        file_mtime: mtime,
        file_size: meta.len() as i64,
        project_dir: detect_project_dir(path),
        ..Default::default()
    };

    let f = File::open(path).with_context(|| format!("open {} failed", path.display()))?;
    let reader = BufReader::with_capacity(64 * 1024, f);

    let mut blocks = Vec::new();
    let mut msg_index: i64 = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue, // skip unreadable lines
        };
        if line.trim().is_empty() {
            continue;
        }
        // Decode permissively; one bad line should not break the whole session.
        let header: LineHeader = match serde_json::from_str(&line) {
            Ok(h) => h,
            Err(_) => continue,
        };

        if let Some(s) = header.session_id.as_deref() {
            if session.claude_session_id.is_empty() {
                session.claude_session_id = s.to_string();
            }
        }
        if let Some(c) = header.cwd.as_deref() {
            if session.cwd.is_none() {
                session.cwd = Some(c.to_string());
            }
        }
        if let Some(g) = header.git_branch.as_deref() {
            if session.git_branch.is_none() {
                session.git_branch = Some(g.to_string());
            }
        }

        let ts_unix = header.timestamp.as_deref().and_then(parse_iso_to_unix);
        if let Some(ts) = ts_unix {
            session.started_at = Some(session.started_at.map_or(ts, |s| s.min(ts)));
            session.last_activity = Some(session.last_activity.map_or(ts, |s| s.max(ts)));
        }

        match header.ty.as_deref() {
            Some("custom-title") => {
                if let Some(t) = header.custom_title {
                    session.custom_title = Some(t);
                }
            }
            Some("ai-title") => {
                if let Some(t) = header.ai_title {
                    session.ai_title = Some(t);
                }
            }
            Some("user") => {
                let blocks_added = extract_message_blocks(
                    "user",
                    header.message.as_ref(),
                    msg_index,
                    ts_unix,
                    &mut blocks,
                );
                if blocks_added > 0 {
                    session.message_count += 1;
                    session.user_message_count += 1;
                    if session.first_user_message.is_none() {
                        if let Some(text) = blocks
                            .iter()
                            .rev()
                            .take(blocks_added)
                            .find(|b| b.block_kind == "text")
                            .map(|b| b.content.clone())
                        {
                            session.first_user_message = Some(truncate_chars(&text, 200));
                        }
                    }
                    msg_index += 1;
                }
            }
            Some("assistant") => {
                let blocks_added = extract_message_blocks(
                    "assistant",
                    header.message.as_ref(),
                    msg_index,
                    ts_unix,
                    &mut blocks,
                );
                if blocks_added > 0 {
                    session.message_count += 1;
                    msg_index += 1;
                }
            }
            _ => {}
        }
    }

    // Synthesize per-file unique session_id.
    let stem = derive_session_id_from_filename(path).unwrap_or_default();
    if is_subagent {
        let parent = if !session.claude_session_id.is_empty() {
            session.claude_session_id.clone()
        } else {
            // Walk up: <projects>/<encoded>/<uuid>/subagents/<agent>.jsonl
            path.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        };
        session.session_id = if parent.is_empty() {
            stem.clone()
        } else {
            format!("{}::{}", parent, stem)
        };
        if session.claude_session_id.is_empty() {
            session.claude_session_id = parent;
        }
    } else {
        session.session_id = if !session.claude_session_id.is_empty() {
            session.claude_session_id.clone()
        } else {
            stem
        };
    }

    Ok((session, blocks))
}

fn detect_project_dir(path: &Path) -> Option<String> {
    // For main sessions: <projects>/<encoded>/<uuid>.jsonl  -> <encoded>
    // For subagents:    <projects>/<encoded>/<uuid>/subagents/<x>.jsonl -> <encoded>
    let mut cur = path.parent()?;
    loop {
        let parent = cur.parent()?;
        if parent.file_name().and_then(|n| n.to_str()) == Some("projects") {
            return cur.file_name().and_then(|n| n.to_str()).map(String::from);
        }
        cur = parent;
    }
}

fn extract_message_blocks(
    role: &str,
    message: Option<&Value>,
    msg_index: i64,
    timestamp: Option<i64>,
    out: &mut Vec<ParsedBlock>,
) -> usize {
    let Some(msg) = message else { return 0 };
    let Some(content) = msg.get("content") else { return 0 };

    let mut added = 0;

    match content {
        Value::String(s) => {
            if !s.is_empty() {
                out.push(ParsedBlock {
                    role: role.to_string(),
                    block_kind: "text".to_string(),
                    content: truncate_bytes(s, MAX_BLOCK_BYTES),
                    msg_index,
                    timestamp,
                });
                added += 1;
            }
        }
        Value::Array(arr) => {
            for block in arr {
                let bt = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let text = match bt {
                    "text" => block.get("text").and_then(|v| v.as_str()).map(String::from),
                    "thinking" => block.get("thinking").and_then(|v| v.as_str()).map(String::from),
                    "tool_use" => {
                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let input = block.get("input").map(serde_json::to_string).and_then(|r| r.ok()).unwrap_or_default();
                        Some(format!("{} {}", name, input))
                    }
                    "tool_result" => {
                        // content can be string or array of {type:"text",text:...}
                        match block.get("content") {
                            Some(Value::String(s)) => Some(s.clone()),
                            Some(Value::Array(parts)) => {
                                let mut buf = String::new();
                                for p in parts {
                                    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                        if !buf.is_empty() {
                                            buf.push('\n');
                                        }
                                        buf.push_str(t);
                                    }
                                }
                                if buf.is_empty() {
                                    None
                                } else {
                                    Some(buf)
                                }
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(text) = text {
                    if !text.is_empty() {
                        out.push(ParsedBlock {
                            role: role.to_string(),
                            block_kind: bt.to_string(),
                            content: truncate_bytes(&text, MAX_BLOCK_BYTES),
                            msg_index,
                            timestamp,
                        });
                        added += 1;
                    }
                }
            }
        }
        _ => {}
    }

    added
}

fn derive_session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // Sessions are named <uuid>.jsonl. Subagent sessions live under
    // <session>/subagents/agent-XXXX.jsonl — keep their own filename as id.
    Some(stem.to_string())
}

fn parse_iso_to_unix(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp())
}

fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 16);
    out.push_str(&s[..end]);
    out.push_str("…[truncated]");
    out
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut count = 0;
    let mut end = 0;
    for (i, _) in s.char_indices() {
        if count >= max_chars {
            end = i;
            break;
        }
        count += 1;
        end = s.len();
    }
    if count >= max_chars && end < s.len() {
        let mut out = s[..end].to_string();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(".jsonl").tempfile().unwrap();
        for l in lines {
            writeln!(f, "{}", l).unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_user_string_content() {
        let f = write_tmp(&[
            r#"{"type":"user","sessionId":"abc","cwd":"/x","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"hello world"}}"#,
        ]);
        let (s, b) = parse_file(f.path()).unwrap();
        assert_eq!(s.session_id, "abc");
        assert_eq!(s.cwd.as_deref(), Some("/x"));
        assert_eq!(s.user_message_count, 1);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].content, "hello world");
        assert_eq!(b[0].role, "user");
        assert_eq!(b[0].block_kind, "text");
        assert_eq!(s.first_user_message.as_deref(), Some("hello world"));
    }

    #[test]
    fn parses_assistant_with_blocks() {
        let f = write_tmp(&[
            r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-01-01T00:00:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"plan it"},{"type":"text","text":"ok"},{"type":"tool_use","name":"Bash","input":{"cmd":"ls"}}]}}"#,
        ]);
        let (_, b) = parse_file(f.path()).unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].block_kind, "thinking");
        assert_eq!(b[0].content, "plan it");
        assert_eq!(b[1].block_kind, "text");
        assert_eq!(b[1].content, "ok");
        assert_eq!(b[2].block_kind, "tool_use");
        assert!(b[2].content.starts_with("Bash"));
        assert!(b[2].content.contains("ls"));
    }

    #[test]
    fn parses_tool_result_string_and_array() {
        let f = write_tmp(&[
            r#"{"type":"user","sessionId":"abc","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"out"}]}}"#,
            r#"{"type":"user","sessionId":"abc","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"part1"},{"type":"text","text":"part2"}]}]}}"#,
        ]);
        let (_, b) = parse_file(f.path()).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].block_kind, "tool_result");
        assert_eq!(b[0].content, "out");
        assert_eq!(b[1].content, "part1\npart2");
    }

    #[test]
    fn skips_corrupt_last_line() {
        let f = write_tmp(&[
            r#"{"type":"user","sessionId":"abc","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-01-01T00:00:01Z","message":{"role":"assist"#,
        ]);
        let (s, b) = parse_file(f.path()).unwrap();
        assert_eq!(s.message_count, 1);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn captures_titles() {
        let f = write_tmp(&[
            r#"{"type":"custom-title","customTitle":"my task","sessionId":"abc"}"#,
            r#"{"type":"ai-title","aiTitle":"AI summary","sessionId":"abc"}"#,
        ]);
        let (s, _) = parse_file(f.path()).unwrap();
        assert_eq!(s.custom_title.as_deref(), Some("my task"));
        assert_eq!(s.ai_title.as_deref(), Some("AI summary"));
    }

    #[test]
    fn timestamps_min_max() {
        let f = write_tmp(&[
            r#"{"type":"user","sessionId":"abc","timestamp":"2026-01-02T00:00:00Z","message":{"role":"user","content":"a"}}"#,
            r#"{"type":"user","sessionId":"abc","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"b"}}"#,
        ]);
        let (s, _) = parse_file(f.path()).unwrap();
        assert!(s.started_at.unwrap() < s.last_activity.unwrap());
    }
}
