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
