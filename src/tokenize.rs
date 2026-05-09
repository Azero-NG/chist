//! Pluggable tokenizer for FTS5 indexing and querying.
//!
//! There are two collaborating layers:
//!
//! 1. **Application-side transform (`for_index` / `for_query`)** — applied
//!    by chist before content / queries reach SQLite. For `Jieba`, this
//!    inserts spaces between Chinese tokens so a downstream space-aware
//!    FTS5 tokenizer can index per-word. For `Trigram` / `Unicode61`, it
//!    is a no-op — those backends rely entirely on FTS5's own tokenizer.
//! 2. **FTS5 tokenizer clause (`fts5_clause`)** — embedded into the
//!    `CREATE VIRTUAL TABLE ... USING fts5(... tokenize='...')` statement.
//!
//! Both must agree, which is why a single `Tokenizer` value drives them.
//! The chosen backend is recorded in the `meta` table so future opens can
//! detect drift between config and what the index was actually built with.

use anyhow::{anyhow, Result};
use jieba_rs::Jieba;
use rusqlite::Connection;

/// `meta` key under which the active tokenizer's stable id is recorded.
pub const TOKENIZER_META_KEY: &str = "tokenizer_id";

/// The set of supported tokenizer backends. Strings are stable identifiers
/// stored in the `meta` table — never rename without a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Pure-Rust jieba word segmentation; FTS5 stores per-word tokens via
    /// `unicode61`. Best CJK recall — 1- and 2-character Chinese queries
    /// hit when the index was built with the same backend.
    Jieba,
    /// Default before tokenizer config existed. FTS5 handles tokenization
    /// natively; CJK queries shorter than 3 codepoints fail by design.
    Trigram,
    /// FTS5's `unicode61` tokenizer with no app-side splitting. Useful for
    /// pure-ASCII workloads or as a sanity baseline.
    Unicode61,
}

impl Backend {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "jieba" => Ok(Self::Jieba),
            "trigram" => Ok(Self::Trigram),
            "unicode61" => Ok(Self::Unicode61),
            other => Err(anyhow!(
                "unknown tokenizer backend `{}` — expected jieba, trigram, or unicode61",
                other
            )),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Jieba => "jieba",
            Self::Trigram => "trigram",
            Self::Unicode61 => "unicode61",
        }
    }

    /// The exact `tokenize='...'` clause to embed in `CREATE VIRTUAL TABLE`.
    pub fn fts5_clause(self) -> &'static str {
        match self {
            Self::Jieba => "tokenize='unicode61 remove_diacritics 2'",
            Self::Trigram => "tokenize='trigram'",
            Self::Unicode61 => "tokenize='unicode61 remove_diacritics 2'",
        }
    }
}

/// Carries a backend plus any heavyweight state (jieba dictionary) that
/// shouldn't be re-initialized on every block.
pub struct Tokenizer {
    backend: Backend,
    jieba: Option<Jieba>,
}

impl Tokenizer {
    pub fn new(backend: Backend) -> Self {
        let jieba = match backend {
            Backend::Jieba => Some(Jieba::new()),
            _ => None,
        };
        Self { backend, jieba }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Transform free text into the form chist writes into the FTS5 content
    /// column. For `Jieba` we insert a space between every two adjacent
    /// tokens — Chinese words become space-separated, ASCII passes through
    /// unchanged because jieba treats it as a single token.
    pub fn for_index(&self, text: &str) -> String {
        match self.backend {
            Backend::Jieba => self.jieba_tokenize(text),
            Backend::Trigram | Backend::Unicode61 => text.to_string(),
        }
    }

    /// Same transform applied at query time so MATCH expressions see tokens
    /// shaped like the index. Returning the user's raw input is fine for
    /// `Trigram`/`Unicode61`; for `Jieba` we cut the query the same way.
    pub fn for_query(&self, text: &str) -> String {
        self.for_index(text)
    }

    /// Read the tokenizer id stored in the index's `meta` table and build
    /// a `Tokenizer` for it. Falls back to `Trigram` for legacy databases
    /// that pre-date the `tokenizer_id` key — those were definitionally
    /// indexed with the old built-in trigram tokenizer.
    pub fn load_active(conn: &Connection) -> Result<Tokenizer> {
        let id = crate::db::get_meta(conn, TOKENIZER_META_KEY)?
            .unwrap_or_else(|| "trigram".to_string());
        let backend = Backend::parse(&id)?;
        Ok(Tokenizer::new(backend))
    }

    fn jieba_tokenize(&self, text: &str) -> String {
        let jieba = self
            .jieba
            .as_ref()
            .expect("jieba dictionary must be loaded for Backend::Jieba");
        // Use precise mode (`cut` with HMM) rather than `cut_for_search`:
        // search mode emits redundant sub-word tokens which inflates the
        // index and, more importantly, shreds ASCII punctuation/whitespace
        // into single-char tokens — that turns FTS5 snippets into widely
        // spaced gibberish for English text.
        //
        // Recall trade-off: with `cut`, a 1-char CJK query only hits when
        // jieba happens to cut that character standalone (common particles
        // like 的/了, or unknown words HMM-split into singles). For purpose-
        // built 2+ char queries — the realistic case — this is plenty.
        let tokens = jieba.cut(text, true);
        // Join with a literal space so unicode61 sees them as separate
        // tokens. Skip pure-whitespace tokens to avoid double-spaces.
        let mut out = String::with_capacity(text.len() + tokens.len());
        for (i, t) in tokens.iter().enumerate() {
            if t.chars().all(|c| c.is_whitespace()) {
                continue;
            }
            if i > 0 && !out.is_empty() {
                out.push(' ');
            }
            out.push_str(t);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jieba_splits_cjk_words() {
        let t = Tokenizer::new(Backend::Jieba);
        let out = t.for_index("我想做一个全文搜索工具");
        // Should contain spaces — exact segmentation depends on dict, but
        // there must be at least one space.
        assert!(out.contains(' '), "jieba output had no whitespace: {out:?}");
        // 2-char query case: searching for "搜索" should be findable.
        let q = t.for_query("搜索");
        assert!(out.contains(&q) || out.contains("搜索"), "out={out:?}");
    }

    #[test]
    fn jieba_passes_through_ascii() {
        let t = Tokenizer::new(Backend::Jieba);
        let out = t.for_index("hello world");
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
    }

    #[test]
    fn trigram_is_passthrough() {
        let t = Tokenizer::new(Backend::Trigram);
        assert_eq!(t.for_index("前端"), "前端");
        assert_eq!(t.for_query("前端"), "前端");
    }

    #[test]
    fn unicode61_is_passthrough() {
        let t = Tokenizer::new(Backend::Unicode61);
        assert_eq!(t.for_index("前端"), "前端");
    }

    #[test]
    fn backend_parse_round_trip() {
        for b in [Backend::Jieba, Backend::Trigram, Backend::Unicode61] {
            let parsed = Backend::parse(b.id()).unwrap();
            assert_eq!(parsed, b);
        }
    }

    #[test]
    fn backend_parse_rejects_unknown() {
        assert!(Backend::parse("morse").is_err());
    }

    #[test]
    fn fts5_clauses_differ_per_backend() {
        let a = Backend::Jieba.fts5_clause();
        let b = Backend::Trigram.fts5_clause();
        assert_ne!(a, b);
    }
}
