# chist — Claude Code 历史对话搜索

把 `~/.claude/projects/*/` 下的 session jsonl 索引成 SQLite FTS5，给 AI 提供全文检索能力，让你能根据描述快速切回某个历史对话。

## 安装

```sh
cargo install --path .
# 或本地构建
cargo build --release
```

二进制：`~/.cargo/bin/chist`（cargo install 后），或 `target/release/chist`。

## 用法

```sh
# 首次：建索引（约 2 分钟，2900+ session × ~9GB jsonl）
chist rebuild

# 查询
chist search "rust async retry"
chist search "claude-mem" --limit 5 --format text
chist search "向量数据库" --cwd /Users/me/mine/llm --since 30d

# 查索引状态
chist stats
```

输出 JSON 格式（默认）；含 `session_id` / `cwd` / `title` / `snippet` / `score` / `resume_command`。

`resume_command` 形如 `cd '<cwd>' && claude --resume <session-id>`，可直接粘到终端。

## 选项

```
chist search <query> [options]
  --cwd <prefix>       项目根目录前缀过滤
  --since <date>       7d / 2026-04-15 / RFC3339
  --until <date>
  --limit <n>          默认 20
  --format json|text   默认 json
  --no-scan            跳过增量 mtime 扫，仅查现有索引
```

## 作为 Claude Code skill

```sh
mkdir -p ~/.claude/skills/claude-history
cp skill/SKILL.md ~/.claude/skills/claude-history/
```

之后在 Claude Code 里描述场景（"上次我们聊那个 X 的对话怎么找"）会自动触发。

## 行为

- 增量扫描：`chist search` 默认在查询前 stat 全部 jsonl，找出 mtime 变化的重索引；30 秒内重复调用自动跳过（cooldown）
- subagent session：路径含 `subagents/` 的 jsonl 单独建条目，不被父 session 覆盖
- 索引内容：`text` / `thinking` / `tool_use` 名称+参数 / `tool_result` 输出，每块上限 100KB
- Tokenizer：`trigram`，中英混合直接搜；2 字 CJK 命中率较低（trigram 限制）

## 索引位置

```
~/.cache/chist/index.db        # macOS: ~/Library/Caches/chist/
```

可用 `sqlite3` 直接查表：

```sh
sqlite3 ~/.cache/chist/index.db "SELECT count(*) FROM sessions"
sqlite3 ~/.cache/chist/index.db ".schema messages_fts"
```
