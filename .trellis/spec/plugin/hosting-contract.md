---
name: hosting-contract
description: 清单、命令、视图、提供者、事件、能力与设置键的对外契约
paths:
  - src/lib.rs
  - plugin.toml
  - settings.schema.json
---

# 托管契约：清单、命令、视图、提供者、事件

本文件约束插件与 OpenIDEA 宿主之间的边界。改动这一步的东西等于改对外契约，
必须按 7 段式写清签名、错误矩阵、测试点和正反例。

---

## 1. Scope / Trigger

触发条件（任一条即需要本文件的深度）：

- 新增/改名/删除 `contributes.commands`、`contributes.views`、`contributes.endpoints`、`contributes.menus`、`contributes.keymaps`
- 改 `run_command` 的 `match` 分支或返回值语义
- 改 `Views::render` / `Views::handle` 的协议
- 改 `EndpointsProvider::endpoints` 的输出
- 改 `[capabilities]`、`activation`、`engines.openidea`
- 改 `settings.schema.json` 的键

---

## 2. Signatures

### 清单声明（`plugin.toml`，现状）

```toml
id = "apimocktle.openidea"
version = "0.2.0"
engines.openidea = "^1.7"
wasm = "dist/plugin.wasm"
activation = ["on-command:*", "on-view:dashboard", "on-schedule:60"]

[contributes.settings]
title = "ApiMocktle"
schema = "settings.schema.json"

[[contributes.endpoints]]
files = ["**/*.java", "**/*.kt"]
```

命令 id 全集（新增命令必须三处齐备：清单条目、`run_command` 分支、必要时菜单/快捷键）：

| id | 清单标题 | 菜单/键位 | 实现 |
| - | - | - | - |
| `export-project` | ApiMocktle: 导出项目接口 | 无 | `start_export(ctx, None, None)` |
| `export-file` | ApiMocktle: 导出当前文件接口 | `editor.context`(when `editorFocus`)、`projectView.context`、`Alt+Shift+E` | 先 `target_path` 再 `start_export(ctx, Some(path), None)` |
| `export-continue` | ApiMocktle: 继续同步剩余接口 | 通知动作「立即继续」 | `flush_export(&cfg, true)` |
| `open-dashboard` | ApiMocktle: 打开 API 仪表盘 | 无（状态项 tooltip 跳转） | `host::reveal_view("dashboard")` |
| `refresh` | ApiMocktle: 重新扫描接口 | 无 | 清两个缓存 + `refresh_view` + notify |

### Rust 侧注册

```rust
register_plugin!(ApiMocktle, views, endpoints);
```

宏的第一个参数是实现类型，后面是"这个插件扮演哪些角色"。
去掉 `views` 就不再有 `Views::render`/`handle` 调用；去掉 `endpoints` 就不进 Search Everywhere。

### trait 方法

```rust
impl Plugin for ApiMocktle {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error>;
    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error>;
    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation);
}

impl EndpointsProvider for ApiMocktle {
    fn endpoints(&mut self, ctx: &Invocation) -> Result<Vec<Route>, Error>;
}

impl Views for ApiMocktle {
    fn render(&mut self, view: &str, _viewport: Option<Viewport>, _ctx: &Invocation) -> Result<View, Error>;
    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error>;
}
```

`run_command` 返回 `Ok(Some(text))` 表示"有结果文本要说"，返回 `Ok(None)` 表示"静默成功"。
`arg` 是清单菜单/命令调用时宿主透进来的第一个选中路径（Project 视图与编辑器标签页菜单会传）。

---

## 3. Contracts

> 改循环/网络/存储前先读 [budget-and-queue.md](./budget-and-queue.md)：预算链与收手守卫是硬约束。

### 设置键

`settings.schema.json` 的 `properties` 键 = `host::setting_*` 读的键，逐字对应：

| 键 | 类型 | 默认 | 代码读取 | 语义 |
| - | - | - | - | - |
| `serverUrl` | string | `http://localhost:4000` | `setting_string("serverUrl")` | ApiMocktle/YApi 根地址，尾斜杠会被 `Client::trimmed` 去掉 |
| `token` | string | `""` | `setting_string("token")` | 个人令牌，空则所有导出前置校验失败 |
| `includeKotlin` | boolean | `true` | `setting_bool("includeKotlin").unwrap_or(true)` | 是否把 `**/*.kt` 加入候选 |
| `schemaDepth` | number | `3`（最小 1，最大 4） | `setting_number("schemaDepth").map(\|n\| n as u32).unwrap_or(3).clamp(1, 4)` | DTO 递归层数 |

`config()` 是唯一的读取入口，前置校验的失败文案固定：

```rust
if base.is_empty()  { return Err(Error::Invalid("先在设置 › ApiMocktle 里填写服务器地址".into())); }
if token.is_empty() { return Err(Error::Invalid("先在设置 › ApiMocktle 里填写个人令牌".into())); }
```

### 视图

视图 id 是常量 `pub const VIEW: &str = "dashboard"`，与 `[[contributes.views]] id`、`activation` 里的
`on-view:dashboard`、`reveal_view(VIEW)`、`refresh_view(VIEW)` 共享同一个字符串。
`render`/`handle` 收到别的 view id 一律 `Error::NotFound(view)`。

`render` 的返回体：

```rust
View::new(Body::Html(page))
    .button(Button::new("refresh", "重新扫描").icon("refresh"))
    .button(Button::new("export", "导出全部").icon("run"))
```

`handle` 接到的 `ViewEvent` 有三种，全部必须被处理（`_ => Ok(())` 只用于未知 view 之外的兜底）：

- `ViewEvent::Button("refresh")` → 清缓存、重扫、`post(payload)`
- `ViewEvent::Button("export")` → `start_export(ctx, None, None)`，结果走 `report`
- `ViewEvent::Message(text)` → 解析 JSON 后按 `op` 分发，见 `../view/message-protocol.md`

### endpoints 提供器

```rust
Route {
    method: endpoint.method.clone(),        // Option<String>，None = 任意方法
    path: endpoint.path.clone(),
    framework: endpoint.framework.clone(),  // "Spring" | "JAX-RS" | "Feign"
    handler: Some(endpoint.handler()),      // "UserController#create"
    location: Location {
        path: endpoint.file.clone(),
        range: Range::new(
            Position::new(endpoint.line, endpoint.col),
            Position::new(endpoint.line, endpoint.col),
        ),
    },
}
```

位置是**零基行 + UTF-16 列**，两端点相同（零宽位置，落在映射注解的 token 起点）。宿主据此跳转。

### 事件

`activation` 里的 `on-schedule:60` 让宿主每分钟发一次 `Event::Scheduled`。
`handle_events` 只在看到该事件时调 `auto_continue()`，其余事件忽略。
队列为空时 `auto_continue` 还会 `remove_status_item("sync")` 清状态项。

### 能力

| 能力 | 谁在用 | 缺了会怎样 |
| - | - | - |
| `project.read` | `project.root()`、`search_text`、`find_files`、`read_file` | 扫描全失败，仪表盘显示错误文本 |
| `storage` | `host::storage::{get,set,delete}`：`export:queue`、`cats:<project_id>` | 导出无法分批，队列写不进去 |
| `net:localhost` / `net:127.0.0.1` | `host::fetch` 访问本机 mock 服务器 | `fetch` 返回 `Error::Denied` |

注意 `capabilities` 只放行清单里声明过的主机。服务器在局域网或 HTTPS 时必须改清单
（`net:api.example.com` 或 `net.http:192.168.1.20`），改完要重新构建并重新授权。
这是宿主模型，不是本插件的取舍。

---

## 4. Validation & Error Matrix

`run_command` 的错误选择规则（`openidea_plugin::Error` 只有六个变体：
`Denied(String)` / `Stale` / `NotFound(String)` / `Invalid(String)` / `Cancelled` / `Failed(String)`）：

| 条件 | 返回 | 用户看到 |
| - | - | - |
| 未打开项目 | `ctx.project().ok_or("先打开一个项目")?` → 经 `From<&str>` 变成 **`Error::Failed`** | 直接文本 |
| `serverUrl` 空 | `Error::Invalid("先在设置 › ApiMocktle 里填写服务器地址")` | 同上 |
| `token` 空 | `Error::Invalid("先在设置 › ApiMocktle 里填写个人令牌")` | 同上 |
| 一个接口都没扫到 / 过滤后为空 | `Error::Invalid("没有找到可导出的接口")` | 同上 |
| 服务器上没有任何项目 | `Error::Failed("服务器上还没有项目，请先在 ApiMocktle 里创建一个")` | 同上 |
| 多项目且用户取消选择框 | `host::pick(...)?.ok_or(Error::Cancelled)?` | 取消，不弹通知 |
| HTTP 401/403 | `Error::Denied("服务器拒绝了令牌，请在设置里更新个人令牌")` | 同上 |
| 其他非 2xx | `Error::Failed("HTTP {status}：{前 300 字符}")` | 同上 |
| 响应不是 JSON | `Error::Failed("响应不是 JSON：{前 200 字符}")` | 同上 |
| 未知命令 id | `Error::NotFound(format!("command {other}"))` | 宿主日志 |
| 视图 id 不匹配 | `Error::NotFound(view.into())` | 宿主日志 |

`Denied` 的语义边界：宿主 SDK 文档明确"本领未授予是 `Denied`，应优雅降级而不是让命令失败"。
本插件在 `endpoints()` 里就是这么做的——扫描失败只 `host::log(Warn, ...)` 并返回空列表，
让 Search Everywhere 变成空，而不是抛错。

> **Gotcha**：`ok_or("...")?` 与 `Error::Invalid("...")` 不是一回事。
> SDK 为 `&str` / `String` 实现了 `From`，它产出的是 `Error::Failed`（`Display` 原样输出文本）。
> 想表达"用户输入/前置条件不合法"必须显式写 `Error::Invalid`，否则错误分类是错的——
> 宿主与日志会按 `Failed`（运行时失败）而不是 `Invalid`（用法问题）对待。

---

## 5. Good / Base / Bad Cases

**Good：新增一个命令**

清单加条目、`match` 加分支、测试断言发出的 HTTP 请求或视图消息，三处齐备。

**Base：只改命令标题文案**

只动 `plugin.toml` 的 `title`。不需要动 Rust，不需要动 spec 的签名段，但要确认菜单/键位的引用仍指向同一 id。

**Bad：清单加了命令，Rust 没加分支**

宿主会调用 `run_command("new-id", ..)`，走到 `other => Err(Error::NotFound(...))`。
用户点了没反应，日志里一行 not found。这类"半截实现"是本项目最容易出的错。

**Bad：改了设置键名只改一边**

`settings.schema.json` 改成 `server_url`，代码还读 `serverUrl` → 设置页显示新键、代码永远读到空串，
现象是"每次都提示先填服务器地址"，即使设置页里明明填了。

---

## 6. Tests Required

| 用例 | 位置 | 断言点 |
| - | - | - |
| 命令路由与导出 | `src/lib.rs::tests::exports_a_batch_and_queues_the_rest` | `run_command("export-project", ..)` 返回含 `已同步到 ApiMocktle`；`/api/interface/save` 请求数 = 2；body 里 `project_id`/`catid`/`token`/`method`/`path` 正确 |
| 视图消息往返 | `src/lib.rs::tests::the_view_answers_scan_messages` | `render` 出的 HTML 含 `ApiMocktle` 与 `导出全部`，且**不含** `发送请求`；发 `{"op":"scan"}` 后 `host.view_messages()` 最后一条的 `op == "items"`、`count == 2` |
| endpoints 提供器 | `src/lib.rs::tests::endpoints_are_published_for_search` | `routes.len() == 2`；`framework == "Spring"`；`handler == Some("UserController#create")`；`location.path` 是绝对路径 |
| 定时事件 | `src/lib.rs::tests::the_schedule_advances_the_queue_without_a_click` | `handle_events(&[Event::Scheduled])` 后队列键不存在，`save` 请求数 = 2 |
| 能力缺失降级 | 新增 | `MockHost::install().with_capabilities(&[])` 后 `endpoints()` 返回 `Ok(vec![])` 而不是 `Err` |

`MockHost` 的装配样板见 `src/lib.rs` 的 `fn host()`：`add_project` / `set_project_root` / `add_file` /
`set_setting` / `on_search` / `on_fetch`。所有宿主交互都在这一层模拟，不需要真实 IDE。

---

## 7. Wrong vs Correct

### 视图 id 写死字符串

#### Wrong

```rust
host::post_view_message("dashboard", &message.to_string());
host::reveal_view("dashboard");
host::refresh_view("dashboard");
```

三处字面量，改视图 id 时必然漏一处，症状是"消息发出去了但页面没反应"，且无报错。

#### Correct

```rust
pub const VIEW: &str = "dashboard";

fn post(message: Value) {
    let _ = host::post_view_message(VIEW, &message.to_string());
}
```

清单里的 `id = "dashboard"` 与 `activation = ["on-view:dashboard"]` 仍需手改，
所以常量旁边要有一句注释说明它和清单同步——或直接把这条写进改动自查清单。

### 在 provider 里抛错

#### Wrong

```rust
let scanned = self.routes(&project)?;   // 扫描失败 → 整个提供器失败
```

Search Everywhere 会因为一次文本搜索超时而整块报错，用户失去本可用的能力。

#### Correct

```rust
let scanned = match self.routes(&project) {
    Ok(result) => result,
    Err(e) => {
        host::log(LogLevel::Warn, &format!("ApiMocktle: 扫描失败：{e}"));
        return Ok(Vec::new());
    }
};
```

降级 + 日志。这与 SDK 对 `Error::Denied` 的处理约定一致，更多讨论见 [quality-guidelines.md](./quality-guidelines.md)。
