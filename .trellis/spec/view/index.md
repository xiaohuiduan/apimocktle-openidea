---
name: sandbox-view-overview
description: 沙箱单文件页面的性质、结构、设计决策与样式约定
---

# 沙箱视图层开发指南（view）

> 本目录约束 `src/page.html`：一个在宿主沙箱 iframe 里运行的**单文件页面**，
> 通过消息与 Rust 组件通信。没有构建步骤、没有依赖、没有外部资源。

---

## 这是什么东西

`src/page.html` 被 `include_str!` 编进 WASM 组件（`const PAGE: &str = include_str!("page.html")`），
`Views::render` 时把一段启动 JSON 注入进去，整份 HTML 作为 `Body::Html(page)` 交给宿主。
宿主把它放进沙箱 iframe 显示，页面用 `window.openidea` 提供的两个方法跟组件对话。

因此有三条不可协商的性质：

1. **改页面必须重新编译**。它不是可热更新的静态资源。
2. **不能引用任何外部资源**。没有 CDN、没有外链字体、没有 `<img src="http...">`。
   沙箱不保证网络，且加载失败会静默退化成一堆无样式文本。
3. **页面自持状态**。`render` 只发生一次（除非有人显式 `refresh_view`），
   后续所有变化都由组件推消息驱动。页面不能假设"重渲染会带回我的状态"。

---

## 文件结构（自上而下就是加载顺序）

| 区块 | 内容 | 注意 |
| - | - | - |
| `<head>` + `<style>` | CSS 变量、深浅色两套、列表/详情/状态栏布局 | 颜色只通过变量引用，别在规则里写死 hex |
| `<header>` | 搜索框 `#q`、项目下拉 `#project`、计数 `#count`、`#rescan`、`#exportAll` | `#exportAll` 是主按钮 |
| `<main>` | `#list`（左，按控制器分组的接口列表）+ `#detail`（右，选中项详情） | 两栏弹性布局，`#list` 稍宽 |
| `<footer>` | `#status`（状态文本）+ `#cfg`（服务器是否已配置的 pill） | 状态用 `.ok` / `.warn` 两个 class 表达 |
| `<script>` | 单文件 IIFE，无依赖 | 全部逻辑在这里，约 130 行 |

---

## 开工前检查

- [ ] 我加进 `innerHTML` 的内容，是否每个插值都过了 `esc()`？
- [ ] 我新增的消息 `op`，Rust 侧 `Views::handle` 是否有对应分支？反向同理。
- [ ] 我新增的启动期数据，是否加进了 `render` 里的 `start` JSON，且页面有 `try/catch` 兜底？
- [ ] 我用到的 DOM 节点，是否在 HTML 里真的存在（拼错的 id 不会报错，只会静默不生效）？
- [ ] 我的样式是否只用了现有 CSS 变量？新增颜色要在 `:root` 和 `prefers-color-scheme: light` 两处都定义。

---

## 设计决策

### 决策一：单文件、零依赖

分文件就得打包，打包就得引入构建链，而组件的构建只有 `cargo build`。
单文件换来的是"改一行 CSS 只需要一次 cargo build"，代价是文件较长（约 240 行）。
保持这个取舍：**不要引入 vue/react/任何编译器**。

### 决策二：分组只是视觉，index 是全局的

列表按控制器分组渲染，但每行的 `index` 始终是 `items` 数组里的**原始下标**：

```js
items.forEach((item, index) => { /* 过滤后仍是这个 index */ });
```

这样过滤、分组、排序都不会让 `export-one` / `open` 发错接口。
任何"用分组内序号代替全局 index"的改动都是 bug。

### 决策三：详情区用 `innerHTML`，列表用 DOM API

列表行数量可能上百，用 `createElement` + `textContent` 避免反复解析 HTML；
详情区只有一份，用字符串拼接更快也更易读，但**必须 `esc()` 每个插值**。
这个分工是刻意的，不要"统一成一种"。

### 决策四：`__START__` 占位符而不是模板语法

`render` 用字符串替换注入启动数据：

```rust
let start = json!({ "configured": config().is_ok(), "base": host::setting_string("serverUrl").unwrap_or_default() })
    .to_string()
    .replace('<', "\\u003c");      // 防止 </script> 提前闭合
let page = PAGE.replacen(r#""__START__""#, &start, 1);
```

页面侧：

```js
const START = "__START__";        // 注意：占位符含引号，替换后这里变成真正的 JSON 对象字面量
```

替换的是**带引号的字面量**，所以注入后 `const START = {"configured":true,...};` 是合法 JS。
`<` 转义成 `\u003c` 是为让 `serverUrl` 里可能出现的 `</script>` 不截断脚本块。

---

## 样式约定

- 全部颜色走变量（`--bg` `--fg` `--accent` `--line` …），深色是默认，`prefers-color-scheme: light` 覆盖。
- HTTP 动词徽章按 method 上色：`.verb.GET` `.verb.POST` `.verb.PUT` `.verb.DELETE` `.verb.PATCH`，
  未知方法落在 `.verb.ANY`。
- 路径与字段名用 `--mono` 字体栈（`ui-monospace, "Cascadia Mono", ...`），正文用系统 UI 字体栈。
- 不使用 emoji，不使用图标字体。
- 数字用 `font-variant-numeric: tabular-nums`，避免计数变化时抖动。

---

## 常见错误

**以为 `item.class` 能读。** `class` 是保留字场景，数据里叫 `"class"`，必须写 `item["class"]`。
现有代码就是这么写的，别"顺手"改成点号。

**在 `onMessage` 里假设 payload 一定是对象。** 宿主可能把消息当字符串递进来，
所以 `handle` 先判断 `typeof raw === "string"` 再 `JSON.parse`，解析失败直接 `return`。

**以为 `send()` 一定成功。** `api.postMessage` 在没有宿主环境（比如有人直接拿浏览器打开这个 HTML）
时不存在，`send` 里已经 `if (api.postMessage)` 兜过；新增调用一律走 `send`。

**在沙箱里用 `fetch` / `localStorage`。** 沙箱不提供，全部数据必须经消息从组件取。
需要持久化的东西（导出队列、分类缓存）已经在组件侧用 `host::storage`。

---

## 质量门

页面没有独立测试，验证靠这三层：

1. **Rust 侧协议测试**（`src/lib.rs::tests::the_view_answers_scan_messages`）断言 `render` 出的 HTML
   含 `ApiMocktle` 与 `导出全部`，且不含被移除的功能文案。
2. **消息往返测试**：发 `{"op":"scan"}` 后能从 `host.view_messages()` 取到 `op == "items"` 的 payload。
3. **真实 IDE 冒烟**：`pwsh script/package.ps1` → 装 zip → 打开 Spring 项目 →
   工具窗口应列出接口、搜索能过滤、点行能出详情、跳源码能定位。

改页面后**必须至少跑一次第 3 步**——HTML/JS 的错不会让 `cargo test` 变红。
