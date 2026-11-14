# Bootstrap Task: Fill Project Development Guidelines

**You (the AI) are running this task. The developer does not read this file.**

The developer just ran `trellis init` on this project for the first time.
`.trellis/` now exists with empty spec scaffolding, and this bootstrap task
exists under `.trellis/tasks/`. When they want to work on it, they should start
this task from a session that provides Trellis session identity.

**Your job**: help them populate `.trellis/spec/` with the team's real
coding conventions. Every future AI session — this project's
`trellis-implement` and `trellis-check` sub-agents — auto-loads spec files
listed in per-task jsonl manifests. Empty spec = sub-agents write generic
code. Real spec = sub-agents match the team's actual patterns.

Don't dump instructions. Open with a short greeting, figure out if the repo
has any existing convention docs (CLAUDE.md, .cursorrules, etc.), and drive
the rest conversationally.

---

## Status (update the checkboxes as you complete each item)

- [x] Fill guidelines for the Rust component layer (`.trellis/spec/plugin/`)
- [x] Fill guidelines for the sandbox view layer (`.trellis/spec/view/`)
- [x] Add code examples (每条契约都带真实签名、错误矩阵、正反例与测试断言点)

> **偏离自检出的初始计划**：`trellis init` 把 `refs/ApiMocktle-java-plugin` 与 `refs/openidea`
> 当成本项目的包。两者是只读上游参考检出（被根 `.gitignore` 忽略、被 Cargo workspace 排除），
> 不是本仓库交付的代码。因此 `config.yaml` 的 `packages:` 已移除（Trellis 进入单仓模式），
> `.trellis/spec/` 改为 `plugin/` 与 `view/` 两层，全部对齐根目录这个单一 crate 的真实行为。

---

## Spec files to populate

### Package: 单仓，两层

- Rust 组件层：`.trellis/spec/plugin/`
  - `index.md` — 仓库形态、文件地图、开工检查、设计决策、质量门
  - `hosting-contract.md` — 清单/命令/视图/提供器/事件/能力/设置键
  - `budget-and-queue.md` — 预算链、分批、队列状态机、状态项、通知
  - `yapi-contract.md` — 5 个端点、响应形状、`api_doc` 字段、分类自愈
  - `scanner-contract.md` — 词法/结构/项目级扫描、参数分类、Schema 生成、位置与去重
  - `quality-guidelines.md` — panic 禁令、日志、文案、依赖纪律、Rust 风格
  - `build-and-release.md` — 构建、打包、导入表复核、安装与授权陷阱
- 沙箱视图层：`.trellis/spec/view/`
  - `index.md` — 单文件页面的性质、结构、设计决策、样式约定
  - `message-protocol.md` — 双向 `op` 协议、启动注入、`report` 双通道


### Thinking guides (already populated)

`.trellis/spec/guides/` contains general thinking guides pre-filled with
best practices. Customize only if something clearly doesn't fit this project.

---

## How to fill the spec

### Step 1: Import from existing convention files first (preferred)

Search the repo for existing convention docs. If any exist, read them and
extract the relevant rules into the matching `.trellis/spec/` files —
usually much faster than documenting from scratch.

| File / Directory | Tool |
|------|------|
| `CLAUDE.md` / `CLAUDE.local.md` | Claude Code |
| `AGENTS.md` | Codex / Claude Code / agent-compatible tools |
| `.cursorrules` | Cursor |
| `.cursor/rules/*.mdc` | Cursor (rules directory) |
| `.windsurfrules` | Windsurf |
| `.clinerules` | Cline |
| `.roomodes` | Roo Code |
| `.github/copilot-instructions.md` | GitHub Copilot |
| `.vscode/settings.json` → `github.copilot.chat.codeGeneration.instructions` | VS Code Copilot |
| `CONVENTIONS.md` / `.aider.conf.yml` | aider |
| `CONTRIBUTING.md` | General project conventions |
| `.editorconfig` | Editor formatting rules |

### Step 2: Analyze the codebase for anything not covered by existing docs

Scan real code to discover patterns. Before writing each spec file:
- Find 2-3 real examples of each pattern in the codebase.
- Reference real file paths (not hypothetical ones).
- Document anti-patterns the team clearly avoids.

### Step 3: Document reality, not ideals

**Critical**: write what the code *actually does*, not what it should do.
Sub-agents match the spec, so aspirational patterns that don't exist in the
codebase will cause sub-agents to write code that looks out of place.

If the team has known tech debt, document the current state — improvement
is a separate conversation, not a bootstrap concern.

---

## Quick explainer of the runtime (share when they ask "why do we need spec at all")

- Every AI coding task spawns two sub-agents: `trellis-implement` (writes
  code) and `trellis-check` (verifies quality).
- Each task has `implement.jsonl` / `check.jsonl` manifests listing which
  spec files to load.
- The platform hook auto-injects those spec files + the task's `prd.md`
  into every sub-agent prompt, so the sub-agent codes/reviews per team
  conventions without anyone pasting them manually.
- Source of truth: `.trellis/spec/`. That's why filling it well now pays
  off forever.

---

## Completion

When the developer confirms the checklist items above are done with real
examples (not placeholders), guide them to run:

```bash
python ./.trellis/scripts/task.py finish
python ./.trellis/scripts/task.py archive 00-bootstrap-guidelines
```

After archive, every new developer who joins this project will get a
`00-join-<slug>` onboarding task instead of this bootstrap task.

---

## Suggested opening line

"Welcome to Trellis! Your init just set me up to help you fill the project
spec — a one-time setup so every future AI session follows the team's
conventions instead of writing generic code. Before we start, do you have
any existing convention docs (CLAUDE.md, .cursorrules, CONTRIBUTING.md,
etc.) I can pull from, or should I scan the codebase from scratch?"
