# Thinking Guides

> **Purpose**: Expand your thinking to catch things you might not have considered.

---

## Why Thinking Guides?

**Most bugs and tech debt come from "didn't think of that"**, not from lack of skill:

- Didn't think about what happens at layer boundaries → cross-layer bugs
- Didn't think about code patterns repeating → duplicated code everywhere
- Didn't think about edge cases → runtime errors
- Didn't think about future maintainers → unreadable code

These guides help you **ask the right questions before coding**.

---

## Available Guides

| Guide | Purpose | When to Use |
|-------|---------|-------------|
| [Code Reuse Thinking Guide](./code-reuse-thinking-guide.md) | Identify patterns and reduce duplication | When you notice repeated patterns |
| [Cross-Layer Thinking Guide](./cross-layer-thinking-guide.md) | Think through data flow across layers | Features spanning multiple layers |

---

## Quick Reference: Thinking Triggers

### When to Think About Cross-Layer Issues

- [ ] Feature touches 3+ layers (API, Service, Component, Database)
- [ ] Data format changes between layers
- [ ] Multiple consumers need the same data
- [ ] You're not sure where to put some logic
- [ ] You are adding an event kind, JSONL record, RPC payload, or config field
- [ ] UI / command code starts casting raw payload fields directly

→ Read [Cross-Layer Thinking Guide](./cross-layer-thinking-guide.md)

### When to Think About Code Reuse

- [ ] You're writing similar code to something that exists
- [ ] You see the same pattern repeated 3+ times
- [ ] You're adding a new field to multiple places
- [ ] **You're modifying any constant or config**
- [ ] **You're creating a new utility/helper function** ← Search first!
- [ ] Two files read the same untyped payload field with local casts
- [ ] Multiple branches update the same derived state from `kind` / `action`

→ Read [Code Reuse Thinking Guide](./code-reuse-thinking-guide.md)

### When Verifying AI Cross-Review Results

- [ ] Reviewer claims "user input can be malicious" → Check the actual data source (internal manifest? user config? external API?)
- [ ] Reviewer flags "missing validation" → Is the data from a trusted internal source?
- [ ] Reviewer says "behavior change" → Read the code comments — is it intentional design?
- [ ] Reviewer identifies a "bug" in test → Mentally delete the feature being tested — does the test still pass? If yes → tautological test

**Common AI reviewer false-positive patterns**:
1. **Trust boundary confusion**: Treating internal data (bundled JSON manifests) as untrusted external input
2. **Ignoring design comments**: Flagging intentional behavior documented in code comments as bugs
3. **Variable misreading**: Not tracing a variable to its actual definition (e.g., Map keyed by path vs name)

**Verification rule**: Every CRITICAL/WARNING finding must be verified against the actual code before prioritizing. Budget ~35% false-positive rate for AI reviews.

---

### When Working In This Repo (project-specific)

这个仓库只有一个 Rust crate + 一份沙箱 HTML，跨层改动集中在少数几个接缝上。动手前先对号入座：

- [ ] 我要改 `plugin.toml`、命令 id、视图 id、提供器或设置键 → 读 [../plugin/hosting-contract.md](../plugin/hosting-contract.md)
- [ ] 我要动循环、网络请求、存储、分批、定时续传 → 读 [../plugin/budget-and-queue.md](../plugin/budget-and-queue.md)
- [ ] 我要改注解识别、路径拼接、参数分类、JSON Schema → 读 [../plugin/scanner-contract.md](../plugin/scanner-contract.md)
- [ ] 我要改 `/api/...` 端点或接口文档字段 → 读 [../plugin/yapi-contract.md](../plugin/yapi-contract.md)
- [ ] 我要改 `src/page.html` 或任一 `op` 消息 → 读 [../view/message-protocol.md](../view/message-protocol.md)
- [ ] 我想加一个依赖 / 写 `unwrap` → 读 [../plugin/quality-guidelines.md](../plugin/quality-guidelines.md)
- [ ] 我要升级 `openidea-plugin` tag 或打包发布 → 读 [../plugin/build-and-release.md](../plugin/build-and-release.md)

**数值改动三处自查**（这个项目最容易漏的一类）：`schemaDepth` 的默认值同时存在于
`scan::Options::default()`、`lib::scan_options()`、`settings.schema.json`；
`BATCH`/`BUDGET`/`timeout_ms` 构成一条预算链，动一环要看另外两环。

**两个"看起来是 bug 但不是"的行为**（改前先确认意图）：Cookie/Form 参数被识别后丢弃；
`@FeignClient(value=...)` 的服务名不当路径前缀。理由都在 spec 里，别凭直觉"修"。

---

## Pre-Modification Rule (CRITICAL)

> **Before changing ANY value, ALWAYS search first!**

```bash
# Search for the value you're about to change
grep -r "value_to_change" .
```

This single habit prevents most "forgot to update X" bugs.

---

## How to Use This Directory

1. **Before coding**: Skim the relevant thinking guide
2. **During coding**: If something feels repetitive or complex, check the guides
3. **After bugs**: Add new insights to the relevant guide (learn from mistakes)

---

## Contributing

Found a new "didn't think of that" moment? Add it to the relevant guide.

---

**Core Principle**: 30 minutes of thinking saves 3 hours of debugging.
