// End-to-end: write fixture jsonl files into a fake `~/.claude/projects`
// layout, point chist at it via env override, build the index, and assert
// search results.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    // Use the binary cargo built for this test profile.
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // remove test exe name
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("chist");
    p
}

fn write(path: &std::path::Path, lines: &[&str]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut f = fs::File::create(path).unwrap();
    for l in lines {
        writeln!(f, "{}", l).unwrap();
    }
}

#[test]
fn search_finds_indexed_phrase_via_real_binary() {
    let tmp = tempfile::tempdir().unwrap();

    // Fake home/cache layout.
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let projects = home.join(".claude").join("projects");
    let proj_dir = projects.join("-Users-fake-foo");
    let jsonl = proj_dir.join("11111111-1111-1111-1111-111111111111.jsonl");
    write(
        &jsonl,
        &[
            r#"{"type":"user","sessionId":"11111111-1111-1111-1111-111111111111","cwd":"/Users/fake/foo","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"how do I implement async retry logic in Rust"}}"#,
            r#"{"type":"assistant","sessionId":"11111111-1111-1111-1111-111111111111","timestamp":"2026-04-15T10:00:30Z","message":{"role":"assistant","content":[{"type":"text","text":"Use tokio retry crate with exponential backoff."}]}}"#,
            r#"{"type":"ai-title","aiTitle":"async retry helper","sessionId":"11111111-1111-1111-1111-111111111111"}"#,
        ],
    );

    let bin = bin();
    assert!(bin.exists(), "chist binary not found at {:?}", bin);

    // Rebuild against fake home, write index into fake cache.
    let out = Command::new(&bin)
        .arg("rebuild")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("rebuild failed");
    assert!(
        out.status.success(),
        "rebuild stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Search.
    let out = Command::new(&bin)
        .arg("search")
        .arg("async retry")
        .arg("--limit")
        .arg("5")
        .arg("--format")
        .arg("json")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("search failed");
    assert!(
        out.status.success(),
        "search stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("output is JSON");

    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty(), "expected at least one result");
    let first = &results[0];
    assert_eq!(first["claude_session_id"], "11111111-1111-1111-1111-111111111111");
    assert_eq!(first["title"], "async retry helper");
    assert_eq!(first["title_source"], "ai_title");
    assert_eq!(first["cwd"], "/Users/fake/foo");
    let resume = first["resume_command"].as_str().unwrap();
    assert!(resume.contains("/Users/fake/foo"));
    assert!(resume.contains("11111111-1111-1111-1111-111111111111"));
}

#[test]
fn cwd_filter_restricts_results() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let projects = home.join(".claude").join("projects");
    write(
        &projects.join("a").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa.jsonl"),
        &[
            r#"{"type":"user","sessionId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","cwd":"/Users/fake/a","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"keyword sapphire"}}"#,
        ],
    );
    write(
        &projects.join("b").join("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb.jsonl"),
        &[
            r#"{"type":"user","sessionId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","cwd":"/Users/fake/b","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"keyword sapphire"}}"#,
        ],
    );

    let bin = bin();
    Command::new(&bin)
        .arg("rebuild")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();

    let out = Command::new(&bin)
        .arg("search")
        .arg("sapphire")
        .arg("--cwd")
        .arg("/Users/fake/a")
        .arg("--format")
        .arg("json")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["cwd"], "/Users/fake/a");
}

#[test]
fn sync_command_picks_up_new_session() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let projects = home.join(".claude").join("projects");
    let initial = projects.join("p").join("aaaaaaaa-1111-1111-1111-aaaaaaaaaaaa.jsonl");
    write(
        &initial,
        &[
            r#"{"type":"user","sessionId":"aaaaaaaa-1111-1111-1111-aaaaaaaaaaaa","cwd":"/Users/fake/p","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"existing topaz token"}}"#,
        ],
    );

    let bin = bin();

    // Initial rebuild.
    Command::new(&bin)
        .arg("rebuild")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();

    // New session shows up after rebuild → search shouldn't see it yet.
    let new_jsonl = projects.join("p").join("bbbbbbbb-2222-2222-2222-bbbbbbbbbbbb.jsonl");
    write(
        &new_jsonl,
        &[
            r#"{"type":"user","sessionId":"bbbbbbbb-2222-2222-2222-bbbbbbbbbbbb","cwd":"/Users/fake/p","timestamp":"2026-04-15T10:05:00Z","message":{"role":"user","content":"newly added emerald phrase"}}"#,
        ],
    );

    // chist sync --force (bypass cooldown) should reindex it.
    let out = Command::new(&bin)
        .arg("sync")
        .arg("--force")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("sync failed");
    assert!(
        out.status.success(),
        "sync stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["action"], "sync");
    assert_eq!(v["skipped_cooldown"], false);
    assert!(v["reindexed"].as_i64().unwrap() >= 1);

    // Now search finds the new session.
    let out = Command::new(&bin)
        .arg("search").arg("emerald").arg("--format").arg("json")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["claude_session_id"], "bbbbbbbb-2222-2222-2222-bbbbbbbbbbbb");

    // sync.log written under cache.
    let log_path = cache.join("chist").join("sync.log");
    let log_contents = fs::read_to_string(&log_path).unwrap();
    assert!(log_contents.contains("done"), "log: {log_contents}");
}

#[test]
fn sync_respects_cooldown() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let projects = home.join(".claude").join("projects");
    write(
        &projects.join("c").join("cccccccc-3333-3333-3333-cccccccccccc.jsonl"),
        &[
            r#"{"type":"user","sessionId":"cccccccc-3333-3333-3333-cccccccccccc","cwd":"/Users/fake/c","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"hi"}}"#,
        ],
    );

    let bin = bin();
    Command::new(&bin).arg("rebuild")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .output().unwrap();

    // First sync: cooldown gate may or may not skip depending on what rebuild
    // wrote — but with --force we know the cooldown bookkeeping is updated.
    Command::new(&bin).arg("sync").arg("--force")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .output().unwrap();

    // Immediate second sync without --force: should be skipped by cooldown.
    let out = Command::new(&bin).arg("sync")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["skipped_cooldown"], true, "stdout: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn install_and_uninstall_hook_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    fs::write(
        &settings,
        r#"{
  "permissions": {"allow": [], "deny": [], "defaultMode": "auto"},
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "echo user-stop" } ] }
    ]
  }
}"#,
    )
    .unwrap();

    let bin = bin();

    // Install.
    let out = Command::new(&bin).arg("install-hook")
        .env("CHIST_SETTINGS_JSON", &settings)
        .output().unwrap();
    assert!(out.status.success());
    let after_install: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    let stop = after_install["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2, "user hook + chist hook side by side");
    let sub = after_install["hooks"]["SubagentStop"].as_array().unwrap();
    assert_eq!(sub.len(), 1);
    // .bak created.
    let bak = settings.with_extension("json.bak");
    assert!(bak.exists());

    // Idempotent: second install adds nothing.
    Command::new(&bin).arg("install-hook")
        .env("CHIST_SETTINGS_JSON", &settings)
        .output().unwrap();
    let after_install2: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(after_install2["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert_eq!(after_install2["hooks"]["SubagentStop"].as_array().unwrap().len(), 1);

    // Uninstall: only chist entries removed; user hook preserved.
    Command::new(&bin).arg("uninstall-hook")
        .env("CHIST_SETTINGS_JSON", &settings)
        .output().unwrap();
    let after_uninstall: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    let stop = after_uninstall["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 1);
    assert_eq!(stop[0]["hooks"][0]["command"], "echo user-stop");
    assert!(after_uninstall["hooks"].get("SubagentStop").is_none());
}

#[test]
fn config_excludes_cwd_prefix_at_search_time() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    let cfg_dir = tmp.path().join("config");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(&cfg_dir).unwrap();

    let projects = home.join(".claude").join("projects");
    write(
        &projects.join("a").join("dddddddd-dddd-dddd-dddd-dddddddddddd.jsonl"),
        &[
            r#"{"type":"user","sessionId":"dddddddd-dddd-dddd-dddd-dddddddddddd","cwd":"/Users/fake/keep","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"unique cobalt token"}}"#,
        ],
    );
    write(
        &projects.join("b").join("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee.jsonl"),
        &[
            r#"{"type":"user","sessionId":"eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee","cwd":"/Users/fake/scratch/throwaway","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"unique cobalt token"}}"#,
        ],
    );

    let cfg_file = cfg_dir.join("config.toml");
    fs::write(
        &cfg_file,
        r#"
[exclude]
cwds = ["/Users/fake/scratch"]
"#,
    )
    .unwrap();

    let bin = bin();

    Command::new(&bin)
        .arg("rebuild")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();

    // With config: only the /keep session should come back.
    let out = Command::new(&bin)
        .arg("search").arg("cobalt").arg("--format").arg("json")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .env("CHIST_CONFIG", &cfg_file)
        .output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 1, "exclude rule should drop /scratch session");
    assert_eq!(r[0]["cwd"], "/Users/fake/keep");
    assert_eq!(v["filters"]["config_applied"], true);

    // --no-config bypass: both sessions returned.
    let out = Command::new(&bin)
        .arg("search").arg("cobalt").arg("--no-config").arg("--format").arg("json")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .env("CHIST_CONFIG", &cfg_file)
        .output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 2, "--no-config should ignore exclude rules");
    assert_eq!(v["filters"]["config_applied"], false);
}

#[test]
fn config_excludes_block_kind_at_search_time() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    let cfg_dir = tmp.path().join("config");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(&cfg_dir).unwrap();

    let projects = home.join(".claude").join("projects");
    // Two sessions: one matches via assistant text, the other only via a
    // tool_result block. Excluding block_kind=tool_result should drop the second.
    write(
        &projects.join("p").join("ffffffff-ffff-ffff-ffff-ffffffffffff.jsonl"),
        &[
            r#"{"type":"user","sessionId":"ffffffff-ffff-ffff-ffff-ffffffffffff","cwd":"/Users/fake/p1","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","sessionId":"ffffffff-ffff-ffff-ffff-ffffffffffff","timestamp":"2026-04-15T10:00:30Z","message":{"role":"assistant","content":[{"type":"text","text":"unique magenta phrase here"}]}}"#,
        ],
    );
    write(
        &projects.join("p").join("99999999-9999-9999-9999-999999999999.jsonl"),
        &[
            r#"{"type":"user","sessionId":"99999999-9999-9999-9999-999999999999","cwd":"/Users/fake/p2","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"unique magenta phrase here"}]}}"#,
        ],
    );

    let cfg_file = cfg_dir.join("config.toml");
    fs::write(
        &cfg_file,
        r#"
[exclude]
block_kinds = ["tool_result"]
"#,
    )
    .unwrap();

    let bin = bin();
    Command::new(&bin)
        .arg("rebuild")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();

    let out = Command::new(&bin)
        .arg("search").arg("magenta").arg("--format").arg("json")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .env("CHIST_CONFIG", &cfg_file)
        .output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 1, "tool_result-only session should be dropped");
    assert_eq!(r[0]["cwd"], "/Users/fake/p1");
}

#[test]
fn subagent_session_does_not_overwrite_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let projects = home.join(".claude").join("projects");
    let parent_jsonl = projects.join("p").join("cccccccc-cccc-cccc-cccc-cccccccccccc.jsonl");
    let sub_jsonl = projects
        .join("p")
        .join("cccccccc-cccc-cccc-cccc-cccccccccccc")
        .join("subagents")
        .join("agent-deadbeef.jsonl");
    write(
        &parent_jsonl,
        &[r#"{"type":"user","sessionId":"cccccccc-cccc-cccc-cccc-cccccccccccc","cwd":"/Users/fake/p","timestamp":"2026-04-15T10:00:00Z","message":{"role":"user","content":"parent unique apple"}}"#],
    );
    write(
        &sub_jsonl,
        &[r#"{"type":"user","sessionId":"cccccccc-cccc-cccc-cccc-cccccccccccc","cwd":"/Users/fake/p","timestamp":"2026-04-15T10:01:00Z","message":{"role":"user","content":"subagent unique banana"}}"#],
    );

    let bin = bin();
    Command::new(&bin)
        .arg("rebuild")
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();

    // Parent term must hit the parent row.
    let out = Command::new(&bin)
        .arg("search").arg("apple").arg("--format").arg("json")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["is_subagent"], false);

    // Subagent term must hit the subagent row.
    let out = Command::new(&bin)
        .arg("search").arg("banana").arg("--format").arg("json")
        .env("HOME", &home).env("XDG_CACHE_HOME", &cache)
        .output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["is_subagent"], true);
    assert_eq!(r[0]["claude_session_id"], "cccccccc-cccc-cccc-cccc-cccccccccccc");
}
