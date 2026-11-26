<!-- TRELLIS:START -->
# Trellis Instructions

These instructions are for AI assistants working in this project.

This project is managed by Trellis. The working knowledge you need lives under `.trellis/`:

- `.trellis/workflow.md` — development phases, when to create tasks, skill routing
- `.trellis/spec/` — package- and layer-scoped coding guidelines (read before writing code in a given layer)
- `.trellis/workspace/` — per-developer journals and session traces
- `.trellis/tasks/` — active and archived tasks (PRDs, research, jsonl context)

If a Trellis command is available on your platform (e.g. `/trellis:finish-work`, `/trellis:continue`), prefer it over manual steps. Not every platform exposes every command.

If you're using Codex or another agent-capable tool, additional project-scoped helpers may live in:
- `.agents/skills/` — reusable Trellis skills
- `.codex/agents/` — optional custom subagents

Managed by Trellis. Edits outside this block are preserved; edits inside may be overwritten by a future `trellis update`.

<!-- TRELLIS:END -->

---

# 发布硬规则

## 同一个 registry PR 绝不升版本号

review 打回来，**改代码、重发同一个版本**；不要每被打回一次就烧一个新版本号。

判据只有一条：**这个版本有没有真的到达用户**。

| 情形 | 正确动作 |
| - | - |
| registry PR 未合并（被打回 / 等 review） | 改代码 → 重打**同一版本** tag → 强推 → 重传 release 附件 → registry 里只更新 `sha256` |
| registry PR 已合并（版本已上市场） | 才升号。已发布的签名包不可变 |

**一次 PR = 一个版本号。** PR 里 commit 可以很多，版本号只能一个。

### 为什么

0.3.9 → 0.3.16 共 **8 个版本号**全烧在同一个未合并的 PR #363 上。
用户一个都没见到，registry 条目每轮改三处，维护者评审成本翻倍。

### 重发同一版本

```sh
# 改代码 + 测试通过后
git tag -d v0.3.9
git push origin :refs/tags/v0.3.9
git tag -a v0.3.9 -m "ApiMocktle 0.3.9"
git push origin v0.3.9
pwsh script/release.ps1          # 重出附件，sha256 会变
# registry PR 里只改 sha256；version 和 package URL 都不动（版本号没变）
```

详细步骤与完整上下文见 `.trellis/spec/plugin/build-and-release.md` 第 0 节。
