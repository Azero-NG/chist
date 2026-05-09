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
  --no-config          忽略 ~/.config/chist/config.toml 的过滤规则
```

## 配置过滤

可选配置文件：默认放在 `~/.config/chist/config.toml`。文件不存在时所有规则视为空，**只在搜索阶段生效**（不影响索引内容，规则改了不需要 rebuild）。

路径优先级：

```
$CHIST_CONFIG          # 完整路径，集成测试与一次性覆盖用
$XDG_CONFIG_HOME/chist/config.toml
~/.config/chist/config.toml   # 默认
```

完整字段（全部可选，缺省取默认值）：

```toml
[exclude]
# 排除这些 cwd 下的所有 session（前缀按目录边界匹配：
# "/Users/me/scratch" 排除 /Users/me/scratch 与 /Users/me/scratch/foo,
# 但不会误伤 /Users/me/scratchpad）。NULL cwd 的 session 不受影响。
cwds = [
    "/Users/me/scratch",
    "/tmp",
]

# 排除 ~/.claude/projects 下的目录名（同样目录边界前缀匹配）
project_dirs = [
    # "-Users-me-scratch",
]

# 排除 jsonl 文件路径前缀（原始字符串前缀，不做目录边界处理）
file_paths = [
    # "/Users/me/.claude/projects/-Users-me-scratch/",
]

# 按 session UUID 精确黑名单（chist 表里的 session_id；subagent 写作 "<父>::<agent>"）
session_ids = [
    # "11111111-1111-1111-1111-111111111111",
]

# 按消息 role 精确排除：可选值 "user" / "assistant"
# （tool_result / thinking 不会出现在 role 字段里，应改用 block_kinds）
roles = []

# 按 block_kind 精确排除：可选值 "text" / "tool_use" / "tool_result" / "thinking"
# 例如不想让命令输出参与召回：
block_kinds = ["tool_result"]

[filter]
# 仅返回总消息数 ≥ N 的 session，过滤误开/废弃会话
min_message_count = 0

# 仅返回 user 消息数 ≥ N 的 session（更严格，过滤被中断的会话）
min_user_message_count = 0
```

**与 CLI 参数的关系**

| 维度 | CLI | 配置 |
|---|---|---|
| `--cwd <prefix>` | 包含过滤（仅返回该前缀） | `exclude.cwds`：排除规则；与 CLI 叠加 |
| `--since` / `--until` | 单次查询时间窗口 | 配置不提供默认时间过滤 |

**临时旁路**：加 `--no-config` 即可在某次查询里忽略全部配置规则（比如真要去 scratch 里翻一下）。

**生效状态**：JSON 输出中的 `filters.config_applied` 字段标记本次查询是否实际应用了配置（文件存在且未 `--no-config`）。结果偏少时可以据此判断是配置静默裁剪还是真没匹配。

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
