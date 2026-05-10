---
name: claude-history
description: Full-text search across the user's past Claude Code sessions; locates a historical session and prints a paste-ready resume command. Use when the user mentions "that conversation we had about…", "which project did I discuss X in?", "find my old chat about…", "switch back to that session", "resume that conversation about…", or analogous requests in any language.
---

# claude-history search skill

`chist` is a local CLI binary that indexes every session jsonl under `~/.claude/projects/*/` and exposes full-text search. Results carry a paste-ready `claude --resume` command.

## Invocation

```
chist search "<query>" [--cwd <prefix>] [--since <date>] [--until <date>] [--limit <n>] [--snippet-tokens <N>]
```

- `--cwd`: restrict to sessions whose cwd starts with this prefix (e.g. `--cwd ~/projects/myapp`).
- `--since` / `--until`: accept `7d` (N days ago), `2026-04-15`, or RFC3339.
- `--limit`: default 20; when invoked from an AI, 5 is usually enough.
- `--snippet-tokens <N>`: tokens of context shown around each hit (FTS5 caps at 1..=64; default 16).

Output is JSON. Each result carries:

- `session_id` — unique key in the index (subagent rows append `::agent-…`).
- `claude_session_id` — the real session UUID (a subagent shares its parent's UUID).
- `is_subagent` — whether this row is a subagent session.
- `cwd` — the session's working directory.
- `title` / `title_source` — display title and where it came from (`custom_title` / `ai_title` / `first_user_message`).
- `started_at` / `last_activity` — RFC3339 timestamps.
- `message_count` — total messages in the session.
- `snippet` — best-rank match excerpt, with the matched term wrapped in `<< >>`.
- `matches` — up to 5 hits per session (`snippet` / `role` / `block_kind` / `score`), sorted by relevance. The first entry duplicates the top-level `snippet` and `score`.
- `score` — relevance (higher is better).
- `resume_command` — paste-ready: `cd '<cwd>' && claude --resume <session_id>`.

## When to use

**Use it when:**
- The user asks "what did we say about X last time?"
- The user wants to switch back to a session but forgot the id / project.
- The user asks to search across "all my Claude history" for Y.
- The user mentions a specific project and wants to find its conversations.

**Don't use it for:**
- The current session's content — the user already has it on screen.
- General code search — use Grep / Glob.
- Reading a known file — use Read.

## Interaction pattern

1. Run `chist search "<keywords>" --limit 5`.
2. Surface the top 3–5 candidates: title, relative time (e.g. "3 days ago"), cwd, snippet.
3. Let the user pick one, then **show** the `resume_command` for them to copy.
4. **Do not run resume yourself** — that would tear down the current session's context.

## Query tips

- Mixed CJK + English queries pass through unchanged; the indexer handles segmentation (default backend is jieba, which makes 1- and 2-character Chinese queries hit).
- Punctuation like `-` `/` `:` inside a token is auto-escaped — type the query verbatim.
- Multiple whitespace-separated keywords are implicit AND: `rust async retry` finds sessions containing all three.
- For very rare 1-character CJK queries, prefer adding context (`"端"` → `"前端"` / `"端口"`).

## Install & maintenance

- Binary location: `~/.cargo/bin/chist` (after `cargo install`) or `target/release/chist` from the repo.
- Index location: `~/.cache/chist/index.db` (SQLite + FTS5). On macOS this resolves to `~/Library/Caches/chist/index.db`.

Lifecycle:

- First run: `chist rebuild` to build the full index (~13s for ~3000 sessions / ~9GB jsonl).
- Routine: incremental updates are driven by Claude Code's Stop / SubagentStop hooks once `chist install-hook` has been run; the index stays warm in the background.
- Manual catch-up: `chist sync` (or `chist sync --force` to bypass the 30s cooldown).
- Inspect state: `chist stats`.
- Switching tokenizer (`[tokenizer] backend` in `~/.config/chist/config.toml`) requires a `chist rebuild` afterwards.
