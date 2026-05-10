English | [简体中文](./README.zh-CN.md)

# chist — full-text search for Claude Code conversation history

Indexes the session jsonl files under `~/.claude/projects/*/` into SQLite FTS5 so an AI (or you) can locate a past conversation by topic and jump back into it.

## Install

### 1. The `chist` binary

```sh
cargo install --path .
# or build locally
cargo build --release
```

Binary lives at `~/.cargo/bin/chist` (after `cargo install`) or `target/release/chist`.

### 2. The Claude Code skill (recommended)

The skill teaches Claude Code when to call `chist` for you ("what was that conversation about X?" → it runs the search and reads back results). Install via [vercel-labs/skills](https://github.com/vercel-labs/skills):

```sh
# Global — ~/.claude/skills/, available across all projects
npx skills add -g Azero-NG/chist

# Or per-project — ./.claude/skills/
npx skills add Azero-NG/chist
```

The `owner/repo` shorthand picks up every skill under `skills/` in this repo (currently just `claude-history`).

Manual install if you'd rather not use npx:

```sh
mkdir -p ~/.claude/skills/claude-history
cp skills/claude-history/SKILL.md ~/.claude/skills/claude-history/
```

## Usage

```sh
# First time: build the index (~13s for ~2900 sessions / ~9GB jsonl)
chist rebuild

# Register Stop / SubagentStop hooks so Claude Code keeps the index warm
chist install-hook

# Search
chist search "rust async retry"
chist search "claude-mem" --limit 5 --format text
chist search "向量数据库" --cwd /Users/me/mine/llm --since 30d

# Trigger an incremental sync manually (the hook normally handles this)
chist sync

# Inspect index state
chist stats
```

Output is JSON by default with `session_id` / `cwd` / `title` / `snippet` / `score` / `resume_command`.

`resume_command` is shaped like `cd '<cwd>' && claude --resume <session-id>` — paste straight into a terminal.

## Options

```
chist search <query> [options]
  --cwd <prefix>       restrict to sessions whose cwd starts with <prefix>
  --since <date>       7d / 2026-04-15 / RFC3339
  --until <date>
  --limit <n>          default 20
  --format json|text   default json
  --no-scan            (legacy; now a no-op — sync is hook-driven)
  --no-config          ignore exclude/filter rules in ~/.config/chist/config.toml
  --snippet-tokens <N> tokens of context around each hit (FTS5 caps at 1..=64)
```

## Search-time filters (config.toml)

Optional config file at `~/.config/chist/config.toml`. If absent, no rules apply. Rules **only run at search time** — changing them does not require a rebuild.

Lookup order:

```
$CHIST_CONFIG          # explicit path; mainly for tests / one-off overrides
$XDG_CONFIG_HOME/chist/config.toml
~/.config/chist/config.toml   # default
```

Full schema (every field optional, omitted keys take their default):

```toml
[search]
# Tokens of context the FTS5 snippet() builds around each match. FTS5 clamps
# this to 1..=64. Default 16. The CLI flag --snippet-tokens overrides it.
snippet_tokens = 16

[exclude]
# Drop sessions whose cwd matches any prefix on a directory boundary:
#   "/Users/me/scratch" excludes "/Users/me/scratch" and "/Users/me/scratch/foo"
#   but does NOT touch "/Users/me/scratchpad". Sessions with NULL cwd are kept.
cwds = [
    "/Users/me/scratch",
    "/tmp",
]

# Same boundary-prefix matching, but against the directory under ~/.claude/projects.
project_dirs = [
    # "-Users-me-scratch",
]

# Plain string-prefix exclusion against jsonl file_path (no directory boundary).
file_paths = [
    # "/Users/me/.claude/projects/-Users-me-scratch/",
]

# Exact session_id blacklist. Subagent sessions are stored as "<parent>::<agent>".
session_ids = [
    # "11111111-1111-1111-1111-111111111111",
]

# Drop matched blocks by `role` ("user" / "assistant"). Note: tool_result and
# thinking are *block_kinds*, not roles — exclude them via block_kinds below.
roles = []

# Drop matched blocks by `block_kind` ("text" / "tool_use" / "tool_result" / "thinking").
# Example: keep tool output noise out of recall.
block_kinds = ["tool_result"]

[filter]
# Only return sessions with at least N messages — drops misfires / aborted chats.
min_message_count = 0

# Stricter variant on user messages — drops sessions you abandoned mid-typing.
min_user_message_count = 0
```

**Interaction with CLI flags**

| Dimension | CLI | Config |
|---|---|---|
| `--cwd <prefix>` | inclusion filter (only that prefix) | `exclude.cwds` is an exclusion; both apply |
| `--since` / `--until` | per-query time window | config has no default time filter |

**One-shot bypass**: `--no-config` skips every config rule for that invocation (useful when you actually need to dig into `/scratch`).

**Did config apply?**: the JSON output's `filters.config_applied` says whether config rules were honored (file present and `--no-config` not passed). Useful for telling "few results because of config" apart from "few results because nothing matched".

## Incremental updates (Stop hook)

`chist search` no longer scans on the query path — incremental updates are driven by Claude Code's Stop / SubagentStop hooks running `chist sync` in the background.

### Install

```sh
chist install-hook       # writes into ~/.claude/settings.json
chist uninstall-hook     # reverses it; only chist-owned entries are removed
```

`install-hook` appends one entry to each of `hooks.Stop` and `hooks.SubagentStop` in `~/.claude/settings.json`:

```json
{
  "hooks": [
    { "type": "command",
      "command": "bash -c 'chist sync >/dev/null 2>&1 </dev/null &'" }
  ]
}
```

Idempotent: running it twice does not produce duplicates. Any existing `Stop` / `SubagentStop` hooks the user has are preserved alongside chist's. The previous file is backed up to `settings.json.bak` before write.

### Behavior

- The hook command returns 0 immediately (`bash -c '... &'`); Claude Code never blocks a turn on it.
- The background process runs `chist sync`: walkdir + mtime/size diff + reindex changed files + drop missing ones, ~hundreds of milliseconds.
- **30s cooldown**: bursts of messages only trigger sync on the first one; subsequent invocations exit immediately on the cooldown gate ("a little late is fine").
- Concurrent syncs: SQLite WAL serializes writes; the cooldown gate dedupes at the entrance. No explicit file lock.
- Subagent jsonl files go through the same sync path.

### Debugging

The hook discards stderr by design, but every sync writes one line to `~/.cache/chist/sync.log`:

```
2026-05-09T09:50:11+08:00  pid=12345  done: 3r/0d/0f in 142ms (2965 on disk, 2962 indexed)
2026-05-09T09:50:14+08:00  pid=12389  skipped (cooldown, last_sync was 3s ago)
2026-05-09T09:51:02+08:00  pid=12450  error: ...
```

Format: `<local time> pid=<PID> <status>` where status is `done: <reindexed>r/<deleted>d/<failed>f in <ms>ms (...)`, `skipped (cooldown, ...)`, or `error: ...`.

To run one manually (bypass cooldown and the hook): `chist sync --force`.

## How it behaves

- Incremental updates: see "Incremental updates (Stop hook)" above. `chist search` itself only reads the DB.
- Subagent sessions: jsonl files containing `subagents/` in their path get their own entries — they don't overwrite the parent session.
- Index content: `text` / `thinking` / `tool_use` name+args / `tool_result` output. Each block capped at 100KB.
- Tokenizer: default `jieba` — 1- and 2-character CJK queries hit. Switch to `trigram` via config; see below.

## Tokenizer

The `[tokenizer]` section of `config.toml` controls how content and queries are segmented:

```toml
[tokenizer]
backend = "jieba"        # default; Chinese word segmentation; 1-2 char CJK queries hit
# backend = "trigram"    # old default; 3-char sliding window; CJK needs ≥3 chars
# backend = "unicode61"  # whitespace/punctuation only; worst CJK recall
```

| backend | "前端" hits | "实现" hits | Index size | Binary size |
|---|---|---|---|---|
| `jieba` (default) | ✓ | ✓ | small (per-word) | +5MB (dict embedded) |
| `trigram` | ✗ | ✗ | large (trigram blowup) | none |
| `unicode61` | ✗ (no CJK split) | ✗ | small | none |

Switching backend **requires a rebuild**: the tokenizer is a write-side and read-side contract, recorded in the DB at `meta.tokenizer_id`.

```sh
# Switch to trigram:
echo '[tokenizer]
backend = "trigram"' >> ~/.config/chist/config.toml
chist rebuild
```

If the config and the index disagree (config changed without rebuild), `chist search` proceeds with the **index's** tokenizer and prints a warning to stderr:

```
warning: config requests tokenizer `trigram` but index was built with `jieba`. Searching with `jieba`. Run `chist rebuild` to switch.
```

## Index location

```
~/.cache/chist/index.db        # macOS: ~/Library/Caches/chist/
```

Direct SQL is fine if you want to poke at it:

```sh
sqlite3 ~/.cache/chist/index.db "SELECT count(*) FROM sessions"
sqlite3 ~/.cache/chist/index.db ".schema messages_fts"
```

## Contributor notes

This repo ships a commit-msg hook that enforces English commit messages. Enable it once after cloning:

```sh
git config core.hooksPath .githooks
```

GitHub does not auto-enable shipped hooks for security reasons.
