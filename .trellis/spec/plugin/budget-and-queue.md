---
name: budget-and-queue
description: 宿主预算链、分批导出、队列状态机、状态项与通知（改 src/lib.rs 的循环/网络/存储前必读）
---

# 预算、分批与导出队列

本项目所有"看起来过度设计"的循环逻辑，都来自同一个约束：**宿主对单次插件调用有墙钟预算，
超了就杀**。本文件把这个约束、它的实现和它的状态机写清楚，避免有人"优化"掉这些守卫。

---

## 1. Scope / Trigger

- 改 `BATCH` / `BUDGET` / `req.timeout_ms` / `route_options().file_limit` / `FILE_LIMIT` / `MATCH_LIMIT` / `TYPE_BUDGET`
- 改 `flush_export` 的分批条件、队列结构、状态项或通知
- 改 `auto_continue` 或 `on-schedule:60`
- 新增任何在命令里循环发网络请求的功能

---

## 2. Signatures

```rust
const BATCH: usize = 200;                            // 单次调用最多开始多少个接口
const BUDGET: Duration = Duration::from_millis(4_200); // 单次调用内"开始新接口"的截止时间
const QUEUE_KEY: &str = "export:queue";              // 队列存储键
const CATS_KEY: &str = "cats:";                      // 分类 id 缓存键前缀，后接 project_id

fn start_export(&mut self, ctx: &Invocation, only_path: Option<String>, target: Option<String>)
    -> Result<String, Error>;
fn flush_export(&mut self, cfg: &Config, notify: bool) -> Result<String, Error>;
fn auto_continue(&mut self) -> Result<(), Error>;
```

`Config` 只有 `base` 与 `token` 两个字段，由 `config()` 统一做前置校验。

### 预算全景（四个数字，别搞混）

| 位置 | 值 | 谁定的 | 作用 |
| - | - | - | - |
| 命令总预算 | 约 5000ms | 宿主 | 超时即杀，无法 catch |
| `BUDGET` | 4200ms | 本项目 | 主动收手，留 800ms 余量给最后一次往返和收尾 |
| `HttpRequest.timeout_ms` | 3000ms | 本项目 | 慢服务器必须**失败**，而不是拖垮命令 |
| endpoints provider 预算 | 约 2000ms | 宿主 | 所以 `route_options()` 关掉 DTO 解析、`file_limit = 400` |

三个数构成一条预算链：`fetch(3000) < BUDGET(4200) < command(5000)`。
任何一环被调高，都会让整条链失去余量。

---

## 3. Contracts

### 队列结构（`host::storage` 键 `export:queue`）

```json
{
  "project_id": "p1",
  "items": [
    { "cat": "UserController", "doc": { "...": "yapi::api_doc 的完整输出" } }
  ]
}
```

- `items` 在**每批之后被重写为剩余项**；剩余为空时整个键被 `delete`。
- `doc` 在入队时就算好（含 JSON Schema 字符串），`flush_export` 不再重算，这是为了不重复解析 DTO。
- 队列存在插件存储里，**关窗口不丢，重启 IDE 后接着走**。

### 分类 id 缓存（键 `cats:<project_id>`）

```json
{ "UserController": "c1", "PayController": "c2" }
```

用 `HashMap<String, String>`（控制器名 → 分类 id）的 JSON。读写函数：

```rust
fn load_categories(project_id: &str) -> HashMap<String, String>   // 读失败 → 空 map
fn save_categories(project_id: &str, map: &HashMap<String, String>) // 写失败 → 静默忽略
```

存在的原因：每次分类查找都是一次往返，5 秒预算装不下几十次。缓存跨批持久化。

### 单批执行流程（`flush_export`）

1. 无队列键 → 直接返回 `Ok("没有待导出的接口")`（不算错误）。
2. 队列 JSON 解析失败 → `Err(Error::Failed("导出队列已损坏：{e}"))`。
3. `items` 为空 → 删键，返回同上。
4. 遍历 items，每项：
   - 收手判断：`if sent >= BATCH || (sent >= 5 && started.elapsed() > BUDGET)` → 推入 `rest`，`continue`
   - 解析分类：`resolve_category`（缓存命中 → 直接用；未命中 → `find_or_create_category`）。
     失败则记 `failed.push(format!("分类「{category}」不可用"))` 并跳过这一项
   - 该分类的现有接口清单：每个 `cat_id` 每批只拉一次（`known: HashMap<cat_id, HashMap<"METHOD path", id>>`）
   - `save_api(&doc, &cat_id, existing.get(&key))`；失败则尝试一次 `rebuild_category` 自愈
5. 写回缓存与队列，发通知/状态项，返回汇总文本。

### 收手条件的两个门槛（关键）

```rust
if sent >= BATCH || (sent >= 5 && started.elapsed() > BUDGET) {
```

`sent >= 5` 这个下界**不能删**。它的作用是：时钟异常（`Instant` 跳变、宿主虚拟时钟不前进）时
不会把一批缩成 1 个接口。没有它，一个坏时钟会让导出变成"每批 1 个、每分钟 1 次"。

### 状态项与通知（`notify` 参数决定形态）

| 场景 | `notify` | 行为 |
| - | - | - |
| 用户点命令/按钮触发 | `true`，剩 0 | `host::notify(Info, "ApiMocktle", summary)` |
| 用户点命令/按钮触发 | `true`，剩 >0 | `host::notify_with_actions(Info, "ApiMocktle", summary, &[NotificationAction::new("立即继续", "export-continue")])` |
| 定时续传 | `false`，剩 >0 | `host::set_status_item("sync", StatusItem { text: "同步中 · 剩 {left}", tooltip: ..., command: Some("open-dashboard") })` |
| 定时续传 | `false`，剩 0 | 不动状态项（`auto_continue` 收尾：`remove_status_item("sync")` + `notify("后台续传完成，接口已全部同步。")`） |

状态项 id 固定为 `"sync"`。`auto_continue` **一进来先检查队列键**：不存在就 `remove_status_item("sync")`
并返回，所以闲置时不会每分钟重扫状态。

### 汇总文案契约

剩 0：`全部 {queued} 个接口已同步到 ApiMocktle。{失败说明}`
剩 >0：`已同步 {saved} 个，还剩 {left} 个没发完。{失败说明}\nOpenIDEA 对单次插件调用限时约 5 秒，剩下的接口每分钟会自动续传，不用管也会走完；想马上推下一批就点「立即继续」。`
失败说明：`\n其中 {n} 个失败：{前 3 条，用；连接}`（超过 3 条只显示前 3 条，不省略计数）
测试断言依赖 `已同步到 ApiMocktle` 这个子串，改文案要同步改测试。

---

## 4. Validation & Error Matrix

| 条件 | 行为 |
| - | - |
| `serverUrl`/`token` 空 | `Error::Invalid(...)`，命令失败，页面 toast 走 `report` |
| 队列 JSON 损坏 | `Error::Failed("导出队列已损坏：{e}")`，队列**不被删除**（保留现场供排查） |
| 队列 `items` 为空 | 删键 + `Ok("没有待导出的接口")` |
| 单接口分类不可用 | 计入 `failed`，继续下一个；不中断整批 |
| `save_api` 第一次失败 | 尝试 `rebuild_category`；成功且新 id 不同则再 save 一次（`existing_id = None`，强制新建） |
| `rebuild_category` 也失败 | 计入 `failed`，继续下一个 |
| 定时续传时 `config()` 失败 | `Ok(())` 静默返回——用户在设置页改到一半不该被通知轰炸 |
| 定时续传 `flush_export` 报错 | `host::log(Warn, ...)`，不通知 |
| 存储读写失败 | `load_categories`/`save_categories` 静默降级；队列读写失败则向上抛 |

---

## 5. Good / Base / Bad Cases

**Good：慢服务器下的完整走完**

本地服务器 5ms 往返 → 56 个接口一批发完，`started.elapsed()` 远小于 4.2s。
慢服务器每次 300ms → 约 14 个接口后触发 `BUDGET` 收手 → 剩余入队 → 状态栏「同步中 · 剩 N」→
宿主每分钟唤醒一次，每批推进一批 → 队列清空时收一条通知。

**Base：只有一个项目**

`projects.len() == 1` 时跳过 `host::pick`，直接用 `projects[0].id`。用户少一次点击。

**Bad：把 `notify` 用来区分用户触发与定时触发之外的语义**

`notify=false` 的语义是"这是后台推进，用状态项而不是通知"，不是"别打扰用户"的通用开关。
再加第三种来源（比如定时器加事件触发）时要重新设计这个布尔量，别硬塞。

**Bad：在批内并发发请求**

`host::fetch` 是同步的，WASM 组件里没有线程池。用 `std::thread` 会直接编译失败或运行时不通。
分批串行 + 定时续传是唯一可行的路径。

---

## 6. Tests Required

| 用例 | 位置 | 断言点 |
| - | - | - |
| 分批 + 队列写回 | `src/lib.rs::tests::exports_a_batch_and_queues_the_rest` | 返回文本含 `已同步到 ApiMocktle`；`save` 请求 2 次；请求 body 的 `project_id == "p1"`、`catid == "c1"`、`token == "secret-token"` |
| 定时续传 | `the_schedule_advances_the_queue_without_a_click` | 预置 2 项队列 → `handle_events(&[Event::Scheduled])` → 队列键被删；`save` 请求 2 次 |
| 队列空时不打扰 | 新增（建议） | 无队列键时 `handle_events(&[Event::Scheduled])` 后 `host.status_items()` 不含 `"sync"`，且无 `fetch` 请求 |
| 损坏队列 | 新增（建议） | 写入非法 JSON 后 `run_command("export-continue", ..)` 返回 `Failed`，且键仍在 |

新增分批相关测试时，用 `MockHost::on_fetch` 注入延迟/失败，不要引入真实网络。

---

## 7. Wrong vs Correct

### 收手判断写成只看时间

#### Wrong

```rust
if sent >= BATCH || started.elapsed() > BUDGET {
    rest.push(item);
    continue;
}
```

时钟不前进或跳变时，第一批就可能只剩 1 个接口，用户看到"每批 1 个"，续传要几小时。

#### Correct

```rust
// The clock guard never fires before a few interfaces went out, so an
// unexpected clock cannot shrink a batch to one.
if sent >= BATCH || (sent >= 5 && started.elapsed() > BUDGET) {
    rest.push(item);
    continue;
}
```

### 每批重算 doc

#### Wrong

```rust
// 队列里只存 endpoint 元信息，flush 时重新 api_doc(...)
let doc = yapi::api_doc(endpoint, &dto, depth);
```

`flush_export` 拿不到 `ScanResult`（队列跨进程/跨会话存活），要么重扫（吃掉预算），
要么无法构造 Schema，两类错误都会发生。

#### Correct

入队时算好整份 doc，存进 `items[].doc`；`flush_export` 只做网络往返。
代价是存储里多存一份 JSON Schema 文本，收益是续传时零解析。
