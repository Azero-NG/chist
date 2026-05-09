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
# 首次：建索引（约 13 秒，2900+ session × ~9GB jsonl）
chist rebuild

# 注册 Stop / SubagentStop hook，让 Claude Code 每个 turn 结束自动增量更新
chist install-hook

# 查询
chist search "rust async retry"
chist search "claude-mem" --limit 5 --format text
chist search "向量数据库" --cwd /Users/me/mine/llm --since 30d

# 手动触发一次增量同步（hook 兜底；通常不需要直接调）
chist sync

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
  --no-scan            （历史选项；已 no-op，增量同步改由 hook 驱动）
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

## 增量更新（Stop hook）

`chist search` 不再做查询时同步 — 索引由 Claude Code 的 Stop / SubagentStop hook 触发后台 `chist sync` 来跟进。

### 安装

```sh
chist install-hook       # 写入 ~/.claude/settings.json
chist uninstall-hook     # 反向操作；只移除 chist 自己写入的条目
```

`install-hook` 在 `~/.claude/settings.json` 的 `hooks.Stop` 与 `hooks.SubagentStop` 数组里各 append 一项：

```json
{
  "hooks": [
    { "type": "command",
      "command": "bash -c 'chist sync >/dev/null 2>&1 </dev/null &'" }
  ]
}
```

幂等：重复运行不会产生重复条目。已有的 `Stop` / `SubagentStop` 配置会被保留（与 chist 自己的并列）。写入前先备份 `settings.json.bak`。

### 行为

- hook 命令立即返回 0（`bash -c '... &'`），CC 不会因此卡 turn
- 后台进程跑 `chist sync`：walkdir + mtime/size 比对 + reindex 变更的 + 清失踪的，过程几百 ms
- **30 秒 cooldown**：连发消息只在首条触发；之后命中 cooldown 直接退出（"晚一点就晚一点"）
- 多个 sync 并行竞争：靠 SQLite WAL 写锁串行 + cooldown 入口去重；不引入文件锁
- subagent jsonl 跟主 jsonl 用同一套 sync 路径

### 调试

后台进程的 stderr 被 hook 写法丢弃，但每次 sync 会写一行到 `~/.cache/chist/sync.log`：

```
2026-05-09T09:50:11+08:00  pid=12345  done: 3r/0d/0f in 142ms (2965 on disk, 2962 indexed)
2026-05-09T09:50:14+08:00  pid=12389  skipped (cooldown, last_sync was 3s ago)
2026-05-09T09:51:02+08:00  pid=12450  error: ...
```

格式：`<本地时间> pid=<PID> <状态>`。状态为 `done: <reindexed>r/<deleted>d/<failed>f in <ms>ms (...)`、`skipped (cooldown, ...)` 或 `error: ...`。

要手动跑一次（绕过 cooldown 与 hook）：`chist sync --force`。

## 作为 Claude Code skill

```sh
mkdir -p ~/.claude/skills/claude-history
cp skill/SKILL.md ~/.claude/skills/claude-history/
```

之后在 Claude Code 里描述场景（"上次我们聊那个 X 的对话怎么找"）会自动触发。

## 行为

- 增量更新：见上方"增量更新（Stop hook）"。`chist search` 本身只查 DB
- subagent session：路径含 `subagents/` 的 jsonl 单独建条目，不被父 session 覆盖
- 索引内容：`text` / `thinking` / `tool_use` 名称+参数 / `tool_result` 输出，每块上限 100KB
- Tokenizer：默认 `jieba`，单/双字 CJK 直接命中；可在 config 切回 `trigram`，详见下节

## 分词器（Tokenizer）

`config.toml` 里的 `[tokenizer]` 段决定索引和查询如何切词：

```toml
[tokenizer]
backend = "jieba"        # 默认；中文分词，1-2 字 CJK 也能命中
# backend = "trigram"    # 旧默认；3-char 滑窗，CJK 至少 3 字才能匹配
# backend = "unicode61"  # 仅按空白/标点切，中文召回最差
```

| backend | "前端" 命中 | "实现" 命中 | 索引大小 | 二进制大小 |
|---|---|---|---|---|
| `jieba` (默认) | ✓ | ✓ | 小（按词索引） | +5MB（词典内嵌） |
| `trigram` | ✗ | ✗ | 大（trigram 膨胀） | 无附加 |
| `unicode61` | ✗（CJK 不切） | ✗ | 小 | 无附加 |

切换 backend **必须 rebuild**：分词器是索引时和查询时都参与的协议，存在 DB 的 `meta.tokenizer_id` 里。

```sh
# 切到 trigram：
echo '[tokenizer]
backend = "trigram"' >> ~/.config/chist/config.toml
chist rebuild
```

如果配置和索引不一致（改了 config 但没 rebuild），`chist search` 会以 **索引为准**（DB 实际分词器）继续工作，并在 stderr 提示：

```
warning: config requests tokenizer `trigram` but index was built with `jieba`. Searching with `jieba`. Run `chist rebuild` to switch.
```

## 索引位置

```
~/.cache/chist/index.db        # macOS: ~/Library/Caches/chist/
```

可用 `sqlite3` 直接查表：

```sh
sqlite3 ~/.cache/chist/index.db "SELECT count(*) FROM sessions"
sqlite3 ~/.cache/chist/index.db ".schema messages_fts"
```
