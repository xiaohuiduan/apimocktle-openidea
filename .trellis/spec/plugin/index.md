---
name: rust-component-overview
description: Rust 组件层总览：仓库形态、文件地图、开工检查、设计决策、质量门
---

# Rust 组件层开发指南（plugin）

> 本目录约束 `src/*.rs` 与 `plugin.toml`：一个 `cdylib` crate 编译成单文件 WASM 组件，
> 由 OpenIDEA 宿主加载。文档叙述用中文，代码标识符、字段名、运行时消息一律保留原文。

---

## 这个仓库的真实形态

在改任何东西之前先记住三条事实，它们决定了这里所有的设计：

1. **只有一个 crate**。`Cargo.toml` 的 `name = "openidea-apimocktle"`，`crate-type = ["cdylib", "rlib"]`，
   目标 `wasm32-wasip2`。没有 workspace 成员，`[workspace] exclude = ["refs"]` 就是为了把上游检出挡在外面。
2. **`refs/` 不是本项目的代码**。`refs/ApiMocktle-java-plugin`（IntelliJ 插件）与 `refs/openidea`（宿主 SDK）
   是只读参考检出，已被根 `.gitignore` 的 `/refs` 忽略，也被 Cargo workspace 排除。
   改它们不会改变本插件的行为，本 spec 也不覆盖它们。查 SDK 契约时读 `refs/openidea/sdk/rust/openidea-plugin/src/`
   与 `refs/openidea/wit/`，但绝不把改动写回去。
3. **没有类型系统、没有 JVM、没有 PSI**。宿主只给文本搜索、读文件、同步 fetch、视图消息。
   接口是从源码 token 里"猜"出来的，猜不到就退化成 `object`（见 [scanner-contract.md](./scanner-contract.md)）。

---

## 文件地图

| 文件 | 职责 | 改动时必读的 spec |
| - | - | - |
| `src/lib.rs` | 插件本体：命令、视图、endpoints 提供器、导出队列、定时续传 | [hosting-contract.md](./hosting-contract.md)、[budget-and-queue.md](./budget-and-queue.md) |
| `src/yapi.rs` | YApi 协议客户端 + 接口文档构造 | [yapi-contract.md](./yapi-contract.md) |
| `src/scan.rs` | 项目扫描：搜索 → 解析 → DTO 传递闭包 → 仪表盘 JSON | [scanner-contract.md](./scanner-contract.md) |
| `src/spring.rs` | 结构化解析：类/方法/字段/参数/注解 → `RawEndpoint` | [scanner-contract.md](./scanner-contract.md) |
| `src/model.rs` | `Endpoint` / `FieldSpec` / `ParamSpec` 与 JSON Schema 生成 | [scanner-contract.md](./scanner-contract.md) |
| `src/java.rs` | 词法分析、括号匹配、UTF-16 行列索引 | [scanner-contract.md](./scanner-contract.md) |
| `src/page.html` | 沙箱 iframe 页面（仪表盘） | `../view/message-protocol.md` |
| `plugin.toml` | 清单：贡献点、能力、激活事件、引擎版本 | [hosting-contract.md](./hosting-contract.md) |
| `settings.schema.json` | 设置项，键名与 `host::setting_*` 读取的键必须一致 | [hosting-contract.md](./hosting-contract.md) |
| `script/package.ps1` | 构建组件并打出可安装 zip | [build-and-release.md](./build-and-release.md) |

---

## 开工前检查（Pre-Development Checklist）

每次动 Rust 代码前逐条确认，这些是历史上真实踩过的坑：

- [ ] 我要改的行为，是否已经在 `plugin.toml` 里有对应的贡献点？命令 id 必须同时存在于清单与
      `run_command` 的 `match` 分支，缺一边就是死代码或静默失败。
- [ ] 我新增/改名的设置键，是否同时改了 `settings.schema.json` 的 `properties`？宿主按 schema 渲染，
      代码按字符串读，两边不同步就得到"设置显示为空且永远读不到"。
- [ ] 我新增的 `host::` 调用，其能力是否在 `plugin.toml` 的 `[capabilities] required` 里？
      未声明的能力会返回 `Error::Denied`，而宿主不会替代码兜底。
- [ ] 这段代码会不会在 5 秒命令预算里跑超？循环里是否已有 `BUDGET` / `timeout_ms` 收手点？
- [ ] 改动是否触碰 `openidea-plugin` 的 trait 签名（`Plugin` / `Views` / `EndpointsProvider`）？
      若 SDK 升了 tag，`Cargo.toml` 的 `tag` 与 `plugin.toml` 的 `engines.openidea` 必须一起动。
- [ ] 新增字符串会出现在用户面前吗？会的话必须与现有文案风格一致（中文、无口号、不解释原理）。

---

## 设计决策（为什么这么做）

### 决策一：只保留"看接口"和"导出到 ApiMocktle"

v0.2.0 明确移除了发请求、复制 curl、字段转 JSON、连接测试。`src/lib.rs` 的测试里有断言守着这条线：

```rust
assert!(!page.contains("发送请求"), "the dashboard only browses and exports");
```

要加回这类功能，先改 PRD 与这条断言，不要绕过它。

### 决策二：解析用自写词法分析，不引第三方 Java 解析器

WASM 组件体积预算紧张（`opt-level = "s"` + `lto` + `strip`，成品约 476 KiB），
且宿主不提供任何语言服务。`src/java.rs` 的 `lex` 只做四件事：切 token、识别字符串/注释、
记录文档注释区间、维护字节区间。所有语义（哪个注解是路由、哪个参数是 body）都在 `spring.rs` 里按 token 形态判定。

### 决策三：缓存分两档，别合并

`ApiMocktle` 持有两个 map：

```rust
cache: HashMap<String, Rc<ScanResult>>,   // 完整扫描（含 DTO）
routes: HashMap<String, Rc<ScanResult>>,  // 仅路由，给 endpoints provider
```

`routes` 存在的唯一原因是 endpoints provider 的 2000ms 预算跑不动 DTO 解析。
`scan()` 会先查 `cache` 再看是否需要真扫；`routes()` 同样先查 `cache`，命中就直接复用完整结果。
**不要为了"省一个 map"把它们合并**，那会让 Search Everywhere 在大型项目上超时。

### 决策四：视图不重绘

`render` 只在宿主要求时返回一次 HTML，页面自己持有状态并通过消息要数据。
因此 `handle` 里任何"数据变了"都要走 `post(...)` 推消息，而不是 `host::refresh_view(...)`。
唯一的例外是 `refresh` 命令：它显式调 `host::refresh_view(VIEW)`，代价是用户看到一次整页重建。

---

## 质量门（Quality Check）

改完代码，按顺序跑。任一不通过就不算完成：

```sh
cargo test                                        # 现在 31 个用例
cargo build --release --target wasm32-wasip2      # 组件必须能出
cargo clippy --all-targets -- -D warnings         # 可选，但别引入新的警告类型
```

已知现状（不是让你顺手改掉，而是别被它误导）：

- `cargo test` 会打印 1 个 `unused_mut` 警告，位置是 **`src/lib.rs:334` 的生产代码**：
  `flush_export` 里 `let mut summary = if left == 0 { … }` 之后没有再对 `summary` 赋值。
  它不阻塞任何门；要清就单独一个提交清掉（删掉那个 `mut` 即可）。
- `openidea-plugin check` / `link` 需要 rustc ≥ 1.96（`ide-host` 依赖 wasmtime 49）。本机是 1.94，
  **跑不了**。别声称验证过它们，改用 `pwsh script/package.ps1` 加「从插件包文件安装」这条路。
- 本目录不是 git 仓库（没有 `.git`），所以 `script/package.ps1` 写进 `dist/plugin.wasm.sources` 的
  commit 是空串。这是预期，不是 bug。
- `python ./.trellis/scripts/get_context.py` 的默认输出会多出
  `GIT STATUS (refs_ApiMocktle-java-plugin: ...)` 之类段落。那是 Trellis 的 polyrepo 兜底探测
  （根不是 git 仓库时扫子目录里的 `.git`），扫到的是 `refs/` 下的上游检出。
  纯展示噪音，不影响 spec、任务或构建；不要据此判断"本项目配置了多仓"。

### 断言点（新增行为必须落在这些位置）

| 行为 | 断言落点 |
| - | - |
| 命令路由 | `src/lib.rs` 测试 `exports_a_batch_and_queues_the_rest` 或新分支测试 |
| 视图消息往返 | `the_view_answers_scan_messages`（`host.view_messages()` 取回 payload） |
| 定时续传 | `the_schedule_advances_the_queue_without_a_click`（`handle_events(&[Event::Scheduled])`） |
| 分类自愈 | `a_stale_category_id_is_rebuilt_once`（old id 被拒 → 重建 → 成功） |
| 路由扫描轻量 | `the_route_scan_skips_dto_resolution`（`light.dto.is_empty()`） |
| 解析规则 | `src/spring.rs` / `src/model.rs` 的 `#[cfg(test)] mod tests` |

测试全部跑在 SDK 的 `MockHost` 上（`openidea_plugin::mock`），不需要真实 IDE、不需要网络。

---

## 常见错误（都是真事）

**把 `@FeignClient(value = "...")` 当路径前缀。** `value` 是服务名，只有 `path` 属性才是前缀。
`spring.rs::class_prefix` 里已经按这个规则分流；真实项目 `opengoofy/index12306` 就是靠这条修好的。

**以为 Cookie / Form 参数会进 YApi 文档。** 它们被识别成 `ParamKind::Cookie` / `ParamKind::Form`，
但 `scan::pick` 只挑 Path / Query / Header 三种塞进 `Endpoint`，其余直接丢弃；
`req_body_form` 恒为 `[]`。这是当前刻意的简化，别"顺手补上"——补上就是一次未评审的契约变更。

**在宿主边界 `unwrap()`。** `Cargo.toml` 设了 `panic = "abort"`，panic 在组件里是 trap，
宿主侧表现为插件莫名其妙不可用。所有可能失败的路径返回 `Error`，见 [quality-guidelines.md](./quality-guidelines.md)。

**改深度默认值只改一处。** 三处默认值不一致且都有意义：`scan::Options::default().depth = 2`（库默认）、
`lib.rs::scan_options()` 把缺失设置兜底成 `3` 并 `clamp(1, 4)`、`settings.schema.json` 的 `default: 3`。
改设置语义必须同时想清楚这三者。
