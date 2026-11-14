---
name: view-message-protocol
description: 页面与组件之间的双向 op 协议、启动注入与 toast 双通道
paths:
  - src/page.html
---

# 视图 ↔ 组件 消息协议

页面与 Rust 之间只有一个通道：字符串化的 JSON。协议是双向的、无请求 id 的、尽力而为的。
任何一侧新增 `op` 都必须同步另一侧与本文档。

---

## 1. Scope / Trigger

- 新增/改名/删除任一方向的 `op`
- 改 `op` 的字段名、类型、可空性
- 改 `render` 注入的启动数据
- 改 `report()` 的 toast 语义

---

## 2. Signatures

```rust
// 组件 → 页面（唯一发送入口）
fn post(message: Value);                                  // 内部 host::post_view_message(VIEW, ...)
// 页面 → 组件（页面侧唯一发送入口）
const send = (payload) => { if (api.postMessage) api.postMessage(JSON.stringify(payload)); };
```

页面拿到的宿主能力只有两个：

```js
const api = window.openidea || {};
api.onMessage(handle);        // 订阅组件推来的消息，回调收到 string 或 object
api.postMessage(string);      // 发给组件，必须自己 JSON.stringify
```

---

## 3. Contracts

### 页面向组件（`Views::handle` 的 `ViewEvent::Message`）

| `op` | 附加字段 | 组件行为 | 回应 |
| - | - | - | - |
| `scan` | — | `dashboard_payload(ctx)`，走缓存（命中则不重扫） | `items` |
| `refresh` | — | 清 `cache` 与 `routes`，再 `dashboard_payload` | `items` |
| `export-all` | `project?: string` | `start_export(ctx, None, project)` | `toast` |
| `export-one` | `index: number`、`project?: string` | `export_one(ctx, index, project)` → `export_single` | `toast` |
| `projects` | — | `list_projects()`（用当前设置的 base/token） | `projects` |
| `open` | `index: number` | `scan()` 后 `host::open_location(file, line, col)`，**无回应** | — |

规则：

- `project` 缺失或为空串时，组件自己决定：只有一个项目就直接用它，多个则弹 `host::pick`。
  页面用 `target()` 生成该字段：`projectBox.value || undefined`，`undefined` 会被 `JSON.stringify` 丢掉，
  正好表达"没选"。
- `index` 是 `items` 数组里的**全局下标**（见 `index.md` 决策二）。
  越界时 → `Error::Invalid("接口不存在，请重新扫描")`。
- `op` 为未知值或 JSON 解析失败 → 静默 `return Ok(())`，不报错。协议要能容忍旧页面。
- `index` 缺失时取 `u64::MAX as usize`，即必然越界，走同一条 `Invalid` 路径。

### 组件向页面（页面的 `handle`）

| `op` | 字段 | 页面行为 |
| - | - | - |
| `items` | `items[]`、`count`、`files`、`truncated`、`configured`、可选 `error` | 有 `error` → 状态栏标红显示、清空列表；否则渲染列表 + 状态栏摘要 |
| `projects` | `items[{id,name}]`、可选 `error` | 重建下拉选项；`error` 时首项文案"未连接"并提示；无项目时 `disabled` |
| `toast` | `text`、`ok` | 只更新状态栏文本，`ok === false` 用 warn 色 |

`items[]` 的每一项（来自 `scan::to_json`，字段名一个字都不能改）：

```json
{
  "method": "POST",          // 缺省 "ANY"
  "path": "/api/users/create",
  "summary": "创建用户。",
  "handler": "UserController#create",
  "class": "UserController", // JS 里必须写 item["class"]，class 是保留字
  "framework": "Spring",
  "file": "/proj/src/UserController.java",
  "line": 7,                 // 1-based
  "params": ["q"],           // 仅 query 参数名
  "body": "CreateUser"       // 无请求体时 null
}
```

`items` 顶层还带 `count` / `files` / `truncated` / `configured`。`configured` 为 false 时
页脚 pill 显示"未配置服务器"——它只是提示，不阻止浏览。

### 启动注入

```rust
let start = json!({
    "configured": config().is_ok(),                        // base 与 token 都非空
    "base": host::setting_string("serverUrl").unwrap_or_default(),
});
let start = start.to_string().replace('<', "\\u003c");
let page = PAGE.replacen(r#""__START__""#, &start, 1);
```

页面侧：

```js
const START = "__START__";
// ...
try {
  const start = JSON.parse(START);
  $("cfg").textContent = start.configured ? "服务器已配置" : "未配置服务器";
} catch (_) { /* the plugin replaces __START__ on render */ }
```

`try/catch` 是必需的：直接用浏览器打开这份 HTML 时占位符没被替换，`JSON.parse("__START__")` 会抛。
启动注入只用于**首屏**，`configured` 的后续变化由 `items` 消息带回。

### 初始化顺序

```js
api.onMessage(handle);
say("扫描中…");
send({ op: "scan" });
send({ op: "projects" });
```

顺序有意义：先订阅再发，否则首条消息可能丢。两条消息都会各自等待组件响应，互不阻塞。

### 视图级按钮（宿主渲染，不走消息）

`render` 里声明的两个按钮由宿主画在视图头部，触发的是 `ViewEvent::Button`：

| 按钮 id | 标题 | 图标 | `handle` 分支 |
| - | - | - | - |
| `refresh` | 重新扫描 | `refresh` | 清缓存 → `post(dashboard_payload)` |
| `export` | 导出全部 | `run` | `start_export(ctx, None, None)` → `report` |

页面内的 `#rescan` / `#exportAll` 是另一套（发 `refresh` / `export-all`）。
**同一个能力有两个入口是现状**，改行为要两处一起改。

### `report` 的双通道

```rust
fn report(&mut self, result: Result<String, Error>) {
    let (ok, text) = match result { Ok(t) => (true, t), Err(e) => (false, e.to_string()) };
    post(json!({ "op": "toast", "text": text, "ok": ok }));
    host::notify(if ok { Info } else { Warning }, "ApiMocktle", &text);
}
```

导出类操作**同时**走 toast（页面内联提示）与系统通知。
只改一处会造成"点了按钮页面没反应"或"通知中心被刷屏"。

---

## 4. Validation & Error Matrix

| 条件 | 组件侧 | 页面侧表现 |
| - | - | - |
| 消息不是合法 JSON | `serde_json::from_str` 失败 → `Ok(())` | 无变化（页面无感知） |
| `op` 未知 | `_ => {}` | 无变化 |
| `index` 越界 / 缺失 | `Error::Invalid("接口不存在，请重新扫描")` → `report` | 状态栏 warn 文本 |
| 未打开项目 | `dashboard_payload` 返回 `{op:"items", error:"先打开一个项目", items:[], count:0}` | 状态栏显示该文本，列表空 |
| 扫描失败 | 同上，`error` 为错误文本 | 同上 |
| `serverUrl`/`token` 缺失 | `projects` 回 `{op:"projects", items:[], error:"先在设置 › ApiMocktle 里填写服务器地址"}` | 下拉禁用，首项"未连接" |
| 项目列表请求失败 | 同上，`error` 为 `Error::to_string()` | 同上 |
| 视图 id 不匹配 | `Error::NotFound(view)` | 宿主日志，页面不加载 |

---

## 5. Good / Base / Bad Cases

**Good：完整的一次浏览**

页面加载 → `scan` 回 `items`（`count=2`，`configured=true`）→ 渲染分组列表 → 用户输入过滤词 →
本地过滤（不发消息）→ 点某行 → 详情区显示 path/参数/来源 → 点"跳到源码" → `{op:"open", index}` →
`host::open_location` 打开文件并定位。

**Base：服务器没配**

`configured=false` → 页脚 pill 显示"未配置服务器"，但列表照常显示（扫描不需要网络）。

**Bad：用 `refresh` 做首屏加载**

首屏发 `refresh` 会白白清空一次缓存，大项目上多花一次完整扫描。
首屏必须用 `scan`（走缓存），只有用户明确点"重新扫描"才用 `refresh`。

**Bad：给 `toast` 加 UI 而不加通道**

在页面里对 `toast` 做弹窗/toast 组件，会让同一句话同时出现在三处（页面弹窗 + 状态栏 + 系统通知）。
现有设计是"状态栏一行 + 系统通知一条"，保持它。

---

## 6. Tests Required

| 用例 | 位置 | 断言点 |
| - | - | - |
| 首屏 HTML 契约 | `src/lib.rs::tests::the_view_answers_scan_messages` | `Body::Html` 含 `ApiMocktle`、`导出全部`；**不含** `发送请求` |
| `scan` 往返 | 同上 | 发 `{"op":"scan"}` 后 `host.view_messages()` 末条 `name == "dashboard"`、`op == "items"`、`count == 2` |
| `export-all` 往返 | 新增（建议） | 发 `{"op":"export-all","project":"p1"}` 后出现 `/api/interface/save` 请求，且最后一条消息是 `op == "toast"`、`ok == true` |
| `projects` 往返 | 新增（建议） | 发 `{"op":"projects"}` 后末条 `op == "projects"` 且 `items[0].id == "p1"` |
| 非法输入容忍 | 新增（建议） | 发 `"not json"` 与 `{"op":"nope"}` 均返回 `Ok(())`，且不产生新消息 |

`host.view_messages()` 返回 `Vec<(view_id, payload)>`，用 `pop()` 取最后一条。

---

## 7. Wrong vs Correct

### 页面假设自己会被重渲染

#### Wrong

```js
// 每次收到数据都希望宿主重画整页，把"选中项"存在 DOM 里靠 class 找回来
send({ op: "scan" });
```

#### Correct

```js
let items = [];      // 页面自己持有数据
let selected = -1;   // 页面自己持有选中态

const handle = (raw) => { /* 更新 items / selected，再局部重绘 */ };
```

`render` 只发生一次。组件推消息，页面自己更新——这是整个视图层的地基。

### 拼 HTML 忘了转义

#### Wrong

```js
out.push('<h2>' + item.path + '</h2>');
```

路径里含 `<` 或引号时结构被破坏；来源不可控（源码字符串）时还是注入面。

#### Correct

```js
const esc = (s) => String(s == null ? "" : s)
  .replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
out.push('<h2>' + esc(item.path) + '</h2>');
```

所有进入 `innerHTML` 的插值都必须过 `esc()`，包括 `summary`、`handler`、`file`、`body`。
