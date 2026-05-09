use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Search-time filtering rules loaded from `~/.config/chist/config.toml`.
///
/// Pure search-side: indexing keeps everything, the CLI applies these to
/// trim what comes back. CLI flags (`--cwd`, `--since`, `--until`) compose
/// with config rules; pass `--no-config` to bypass.
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(default)]
pub struct Config {
    pub tokenizer: TokenizerConfig,
    pub search: SearchConfig,
    pub exclude: ExcludeRules,
    pub filter: FilterRules,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct SearchConfig {
    /// Number of tokens of context FTS5's `snippet()` builds around each
    /// match. Larger = more context per hit (and longer JSON). FTS5 clamps
    /// to [1, 64]; we follow suit.
    pub snippet_tokens: i64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { snippet_tokens: 16 }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct TokenizerConfig {
    /// Tokenizer backend identifier — one of `jieba`, `trigram`, `unicode61`.
    /// `jieba` is the default because it gives the best CJK recall (single-
    /// and double-character Chinese queries hit) at the cost of a ~5MB
    /// dictionary embedded in the binary.
    pub backend: String,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            backend: "jieba".to_string(),
        }
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(default)]
pub struct ExcludeRules {
    /// Drop sessions whose `cwd` equals or is a path-prefix of any entry.
    pub cwds: Vec<String>,
    /// Drop sessions whose `project_dir` equals or is a path-prefix of any entry.
    pub project_dirs: Vec<String>,
    /// Drop sessions matching any of these `session_id`s exactly.
    pub session_ids: Vec<String>,
    /// Drop sessions whose `file_path` starts with any entry.
    pub file_paths: Vec<String>,
    /// Drop matched blocks whose `role` equals any entry. e.g. `["tool_result"]`.
    pub roles: Vec<String>,
    /// Drop matched blocks whose `block_kind` equals any entry. e.g. `["thinking"]`.
    pub block_kinds: Vec<String>,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(default)]
pub struct FilterRules {
    /// Only show sessions with at least this many total messages.
    pub min_message_count: i64,
    /// Only show sessions with at least this many user messages.
    pub min_user_message_count: i64,
}

/// Resolve the config file path. Precedence:
///   1. `$CHIST_CONFIG` (full path; mainly for tests and one-off overrides)
///   2. `$XDG_CONFIG_HOME/chist/config.toml`
///   3. `$HOME/.config/chist/config.toml`
pub fn config_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CHIST_CONFIG") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("chist").join("config.toml"));
        }
    }
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home.join(".config").join("chist").join("config.toml"))
}

/// Load config. Missing file → empty config. Present-but-malformed → error.
pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_parses() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.exclude.cwds.is_empty());
        assert_eq!(c.filter.min_message_count, 0);
        assert_eq!(c.tokenizer.backend, "jieba");
    }

    #[test]
    fn tokenizer_section_parses() {
        let src = r#"
            [tokenizer]
            backend = "trigram"
        "#;
        let c: Config = toml::from_str(src).unwrap();
        assert_eq!(c.tokenizer.backend, "trigram");
    }

    #[test]
    fn full_config_parses() {
        let src = r#"
            [exclude]
            cwds = ["/Users/me/scratch", "/tmp"]
            roles = ["tool_result"]
            block_kinds = ["thinking"]
            session_ids = ["abc123"]
            file_paths = ["/old/path"]
            project_dirs = ["-Users-me-scratch"]

            [filter]
            min_message_count = 3
            min_user_message_count = 1
        "#;
        let c: Config = toml::from_str(src).unwrap();
        assert_eq!(c.exclude.cwds, vec!["/Users/me/scratch", "/tmp"]);
        assert_eq!(c.exclude.roles, vec!["tool_result"]);
        assert_eq!(c.exclude.block_kinds, vec!["thinking"]);
        assert_eq!(c.exclude.session_ids, vec!["abc123"]);
        assert_eq!(c.exclude.file_paths, vec!["/old/path"]);
        assert_eq!(c.exclude.project_dirs, vec!["-Users-me-scratch"]);
        assert_eq!(c.filter.min_message_count, 3);
        assert_eq!(c.filter.min_user_message_count, 1);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // serde(default) on the structs means we don't fail on extra keys at
        // the struct level — but toml itself rejects unknown top-level keys
        // unless we use deny_unknown_fields, which we don't. Verify here.
        let src = r#"
            [exclude]
            cwds = []
            future_field = "ok"
        "#;
        let c: Config = toml::from_str(src).unwrap();
        assert!(c.exclude.cwds.is_empty());
    }

    #[test]
    fn partial_section_uses_defaults() {
        let src = r#"
            [filter]
            min_message_count = 5
        "#;
        let c: Config = toml::from_str(src).unwrap();
        assert_eq!(c.filter.min_message_count, 5);
        assert_eq!(c.filter.min_user_message_count, 0);
        assert!(c.exclude.cwds.is_empty());
    }
}
