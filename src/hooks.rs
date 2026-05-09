//! `chist install-hook` / `chist uninstall-hook`.
//!
//! Merges a Stop / SubagentStop entry into `~/.claude/settings.json`. The
//! hook command is `bash -c '... &'` so Claude Code's hook driver sees a
//! near-instant exit (the actual sync runs in the background). We identify
//! entries owned by chist solely by the substring `chist sync` in the
//! `command` field — no schema-level marker is needed and Claude Code's
//! parser ignores unknown fields anyway.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Background-fire shell wrapper. `</dev/null` detaches stdin, `>/dev/null
/// 2>&1` discards output, `&` puts the command in the background — bash
/// returns 0 immediately, so the hook never blocks the user's turn.
pub const HOOK_COMMAND: &str = "bash -c 'chist sync >/dev/null 2>&1 </dev/null &'";

/// Events we attach to. Stop covers main-agent turn completions; SubagentStop
/// covers Task-launched subagent finishes (whose jsonl files live under a
/// nested subagents/ directory).
pub const HOOK_EVENTS: &[&str] = &["Stop", "SubagentStop"];

/// Resolve the settings.json path. `CHIST_SETTINGS_JSON` overrides for tests.
pub fn settings_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CHIST_SETTINGS_JSON") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home.join(".claude").join("settings.json"))
}

pub fn install() -> Result<()> {
    let path = settings_path()?;
    let mut root = read_or_default(&path)?;
    let (added, already) = merge_hooks(&mut root)?;

    if !added.is_empty() {
        backup_then_write(&path, &root)?;
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "action": "install_hook",
            "settings_path": path.to_string_lossy(),
            "added": added,
            "already_present": already,
            "command": HOOK_COMMAND,
        }))?
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = settings_path()?;
    if !path.exists() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "action": "uninstall_hook",
                "settings_path": path.to_string_lossy(),
                "removed": Vec::<String>::new(),
                "note": "settings.json not found",
            }))?
        );
        return Ok(());
    }
    let mut root = read_or_default(&path)?;
    let removed = strip_hooks(&mut root);

    if !removed.is_empty() {
        backup_then_write(&path, &root)?;
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "action": "uninstall_hook",
            "settings_path": path.to_string_lossy(),
            "removed": removed,
        }))?
    );
    Ok(())
}

/// Insert a chist-owned entry into hooks.<Event> for each missing event.
/// Returns (added_events, already_present_events).
pub fn merge_hooks(root: &mut Value) -> Result<(Vec<String>, Vec<String>)> {
    let obj = root
        .as_object_mut()
        .context("settings.json root must be a JSON object")?;
    let hooks_v = obj
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    let hooks = hooks_v
        .as_object_mut()
        .context("settings.json `hooks` must be an object")?;

    let mut added = Vec::new();
    let mut already = Vec::new();
    for event in HOOK_EVENTS {
        let arr_v = hooks
            .entry((*event).to_string())
            .or_insert_with(|| json!([]));
        let arr = arr_v
            .as_array_mut()
            .with_context(|| format!("settings.json hooks.{event} must be an array"))?;

        if arr.iter().any(entry_owns_chist_hook) {
            already.push((*event).to_string());
            continue;
        }
        arr.push(json!({
            "hooks": [
                { "type": "command", "command": HOOK_COMMAND }
            ]
        }));
        added.push((*event).to_string());
    }
    Ok((added, already))
}

/// Remove every chist-owned hook from hooks.<Event>. Preserves user-owned
/// entries and unrelated hooks within the same entry. Returns the list of
/// events from which something was removed.
pub fn strip_hooks(root: &mut Value) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(obj) = root.as_object_mut() else {
        return removed;
    };
    let Some(hooks_v) = obj.get_mut("hooks") else {
        return removed;
    };
    let Some(hooks) = hooks_v.as_object_mut() else {
        return removed;
    };

    for event in HOOK_EVENTS {
        let Some(arr_v) = hooks.get_mut(*event) else {
            continue;
        };
        let Some(arr) = arr_v.as_array_mut() else {
            continue;
        };

        let before = total_chist_hooks_in_array(arr);
        // Within each entry, drop just the chist commands. If the entry's
        // `hooks` array goes empty, drop the whole entry too.
        arr.retain_mut(|entry| {
            let Some(inner_v) = entry.get_mut("hooks") else {
                return true;
            };
            let Some(inner) = inner_v.as_array_mut() else {
                return true;
            };
            inner.retain(|h| !command_is_chist(h));
            !inner.is_empty()
        });

        if before > 0 {
            removed.push((*event).to_string());
        }
        if arr.is_empty() {
            hooks.remove(*event);
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
    removed
}

fn entry_owns_chist_hook(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| hooks.iter().any(command_is_chist))
        .unwrap_or(false)
}

fn command_is_chist(hook: &Value) -> bool {
    hook.get("command")
        .and_then(|c| c.as_str())
        .map(|s| s.contains("chist sync"))
        .unwrap_or(false)
}

fn total_chist_hooks_in_array(arr: &[Value]) -> usize {
    arr.iter()
        .filter_map(|e| e.get("hooks").and_then(|h| h.as_array()))
        .flat_map(|h| h.iter())
        .filter(|h| command_is_chist(h))
        .count()
}

fn read_or_default(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if s.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&s)
        .with_context(|| format!("failed to parse {} as JSON", path.display()))
}

fn backup_then_write(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let bak = path.with_extension("json.bak");
        std::fs::copy(path, &bak).with_context(|| {
            format!("failed to back up {} to {}", path.display(), bak.display())
        })?;
    }
    let pretty = serde_json::to_string_pretty(value)?;
    std::fs::write(path, pretty)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_empty_settings() {
        let mut root = json!({});
        let (added, already) = merge_hooks(&mut root).unwrap();
        assert_eq!(added, vec!["Stop", "SubagentStop"]);
        assert!(already.is_empty());
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(
            stop[0]["hooks"][0]["command"].as_str().unwrap(),
            HOOK_COMMAND
        );
    }

    #[test]
    fn merge_is_idempotent() {
        let mut root = json!({});
        merge_hooks(&mut root).unwrap();
        let (added, already) = merge_hooks(&mut root).unwrap();
        assert!(added.is_empty(), "second run should add nothing");
        assert_eq!(already, vec!["Stop", "SubagentStop"]);
        // No duplicate entries.
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(root["hooks"]["SubagentStop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn merge_preserves_unrelated_user_hooks() {
        let mut root = json!({
            "permissions": {"allow": [], "deny": [], "defaultMode": "auto"},
            "hooks": {
                "Stop": [
                    { "hooks": [
                        { "type": "command", "command": "echo 'user hook fires'" }
                    ]}
                ],
                "PreToolUse": [
                    { "matcher": "Bash",
                      "hooks": [
                        { "type": "command", "command": "log-bash-call" }
                    ]}
                ]
            }
        });
        let (added, _) = merge_hooks(&mut root).unwrap();
        assert_eq!(added, vec!["Stop", "SubagentStop"]);

        // User's existing Stop entry is preserved alongside ours.
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(
            stop[0]["hooks"][0]["command"].as_str().unwrap(),
            "echo 'user hook fires'"
        );

        // PreToolUse untouched.
        let pretool = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pretool.len(), 1);
        assert_eq!(pretool[0]["matcher"], "Bash");
    }

    #[test]
    fn strip_removes_only_chist_hooks() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    { "hooks": [
                        { "type": "command", "command": "echo user" }
                    ]},
                    { "hooks": [
                        { "type": "command", "command": HOOK_COMMAND }
                    ]}
                ],
                "SubagentStop": [
                    { "hooks": [
                        { "type": "command", "command": HOOK_COMMAND }
                    ]}
                ]
            }
        });
        let removed = strip_hooks(&mut root);
        assert_eq!(removed, vec!["Stop", "SubagentStop"]);
        // User entry preserved.
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo user");
        // SubagentStop array became empty → key removed.
        assert!(root["hooks"].get("SubagentStop").is_none());
    }

    #[test]
    fn strip_removes_chist_command_alongside_user_command_in_same_entry() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    { "hooks": [
                        { "type": "command", "command": "echo user" },
                        { "type": "command", "command": HOOK_COMMAND }
                    ]}
                ]
            }
        });
        strip_hooks(&mut root);
        let inner = root["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0]["command"], "echo user");
    }

    #[test]
    fn strip_no_op_when_chist_absent() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    { "hooks": [
                        { "type": "command", "command": "echo user" }
                    ]}
                ]
            }
        });
        let removed = strip_hooks(&mut root);
        assert!(removed.is_empty());
    }
}
