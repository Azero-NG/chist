---
name: claude-history
description: 在用户过往所有 Claude Code 会话里全文搜索，找到指定历史 session 并给出可粘贴的 resume 命令。当用户提到"上次我们聊过…的对话"、"我之前在哪个项目里讨论过…"、"找回之前关于…的对话"、"切回那个 session"、"resume that conversation about…" 时使用。
---

# claude-history search skill

`chist` 是一个本地 CLI 二进制，索引 `~/.claude/projects/*/` 下所有 session jsonl 并提供全文检索。结果带可粘贴的 `claude --resume` 命令。

## 调用

```
chist search "<query>" [--cwd <prefix>] [--since <date>] [--until <date>] [--limit <n>] [--no-scan]
```

- `--cwd`: 限定到某项目根目录前缀（如 `--cwd ~/projects/myapp`）
- `--since` / `--until`: 接受 `7d`（N 天前）/ `2026-04-15` / RFC3339
- `--limit`: 默认 20，建议给 AI 用时设 5
- `--no-scan`: 跳过增量 mtime 扫描，仅查现有索引（连续多次查询时使用，省 ~500ms-2s）

输出 JSON。每条结果含：
- `session_id` — 一行内的唯一键（subagent 会带 `::agent-…` 后缀）
- `claude_session_id` — 真实的 session UUID（subagent 等同其父）
- `is_subagent` — 是否子 agent session
- `cwd` — 项目工作目录
- `title` / `title_source` — 显示标题及其来源
- `started_at` / `last_activity` — RFC3339 时间
- `message_count` — 消息条数
- `snippet` — 命中片段，匹配词包在 `<<>>` 之间
- `score` — 相关性分（越大越相关）
- `resume_command` — 可粘贴：`cd '<cwd>' && claude --resume <session_id>`

## 何时使用

**应该用：**
- 用户问"我们之前聊过那个 X 怎么处理的"
- 用户想切回某个 session 但忘了 ID/项目
- 用户要求"在所有 Claude 历史里搜 Y"
- 用户提到一个特定项目并想找其对话

**不该用：**
- 当前 session 内容（用户在场，他们能直接看）
- 一般代码搜索（用 Grep/Glob）
- 找文件内容（用 Read）

## 与用户的交互

1. 跑一次 `chist search "<关键词>" --limit 5`
2. 给出 top 3-5 个候选：标题、相对时间（如"3 天前"）、cwd、snippet
3. 让用户挑一个，然后**展示** `resume_command` 让他自己复制。
4. **不要替用户执行 resume** —— 那会切走当前会话上下文。

## 查询技巧

- 中英混合：trigram tokenizer 自动处理，原样输入即可
- 含 `-` `/` `:` 等特殊字符的 query 会自动转义
- 2 字 CJK 命中率较低（trigram 限制），建议加上下文：`"前端"` → `"前端项目"` 或 `"前端框架"`
- 多个关键词隐式 AND：`rust async retry` 会找同时含三个的会话

## 安装与维护

二进制位置：`~/.cargo/bin/chist`（cargo install）或工作目录下 `target/release/chist`。
索引位置：`~/.cache/chist/index.db`（SQLite + FTS5）。

- 首次跑 `chist rebuild` 全量建索引（约 2 分钟，2900+ session × ~9GB jsonl）
- 之后每次 `chist search` 自动增量；若觉得索引不准可再次 `chist rebuild`
- 看索引状态：`chist stats`
