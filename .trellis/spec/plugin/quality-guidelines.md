---
name: quality-guidelines
description: panic 禁令、日志与文案约定、依赖纪律、Rust 风格与检查手段
---

# 质量准则与禁止模式

本项目的质量信号很集中：**生产代码里一次 panic 都没有**，且所有宿主交互都走统一入口。
守住这两点，剩下的问题都是局部的。

---

## 1. Scope / Trigger

- 任何 `src/*.rs` 的改动
- 新增依赖
- 新增错误路径、日志、用户可见文案
- 新增测试

---

## 2. Signatures

```rust
// 错误统一构造方式
Err(Error::Invalid("...".into()))   // 用法/前置条件问题
Err(Error::Failed("...".into()))    // 运行时失败
Err(Error::Denied("...".into()))    // 能力未授予
Err(Error::Cancelled)               // 用户取消
Err(Error::NotFound("...".into()))  // id 不存在

// 日志唯一入口
host::log(LogLevel::Info | LogLevel::Warn, &message);
```

---

## 3. Contracts

### panic 禁令（硬约束）

`Cargo.toml` 设了 `panic = "abort"`。组件内 panic = WASM trap，宿主侧表现为
"插件突然不可用 / 命令毫无反应"，**没有调用栈、没有错误文本**。

现状核查（`src/*.rs` 全部 25 处 `unwrap`/`expect`/`panic!` 都在 `#[cfg(test)]` 模块里，
生产代码 0 处）。改代码时保持这个性质：

- 宿主调用返回 `Result` 一律用 `?` 或显式 `match` 传播
- `Option` 用 `ok_or(...)`/`ok_or_else(...)`/`?`/`let ... else` 转换，不 `unwrap`
- 已知不可能失败的分支（例如 `serde_json::to_string` 对 `HashMap<String,String>`）也用 `if let Ok(..)`，
  失败就静默降级（见 `save_categories`）——因为"不可能"的判断依赖假设，而假设会过期

### 日志

前缀统一 `ApiMocktle: `，中文，说事实不说态度。级別选择：

| 级別 | 用在哪 | 现有例子 |
| - | - | - |
| `Info` | 每次扫描/导出/续传的结果摘要 | `ApiMocktle: {} 个控制器文件 → {} 个接口，{} 个 DTO 类型` |
| `Info` | 激活时一行版本与配置 | `ApiMocktle {ver} 已激活（serverUrl={base}，API 1.7）` |
| `Warn` | 降级、可恢复失败 | `ApiMocktle: 扫描失败：{e}`、`ApiMocktle: 自动续传失败：{e}` |

不要 `println!` / `eprintln!`：组件没有 stdout，输出会丢。
（`examples/*.rs` 里可以用 `println!`，它们跑在本机而不是 WASM 里。）

### 用户可见文案

- 中文，陈述句，不解释原理、不道歉、不鼓励。
- 长度受通知面板限制：汇总文本控制在三行内，多出来的信息进 `host::log`。
- 失败要给出**下一步动作**（`请在设置里更新个人令牌`），而不是只报错。
- 已有文案是基线，改文案要同步改断言（测试断言依赖 `已同步到 ApiMocktle`、`UserController#create` 等子串）。

### 依赖纪律

现有依赖只有两个：`openidea-plugin`（git + tag）与 `serde_json = "1"`。

新增依赖前必须回答：能不能用 `std` 加 50 行内自己写完？
`yapi.rs` 的 `enc` 就是拒绝引入 URL crate 的结果（它需要精确控制 `+` 与 `/` 的编码行为）。
组件体积对启动耗时敏感，`opt-level = "s"` + `lto` + `strip` 才压到约 476 KiB。

`openidea-plugin` 的 `tag` 与 `plugin.toml` 的 `engines.openidea` 是**成对**的：
`v0.1.44` ↔ `^1.7`。升级 IDE 必须同时改这两个，否则 WIT 接口对不上，组件加载失败。

### Rust 风格（跟现有代码一致，不要各写一套）

- `edition = "2024"`，可以用 let-chains：`if let Some(x) = a && let Ok(y) = b { ... }`
- 提前返回优于嵌套：`let Some(x) = ... else { return ... };`
- `Option::is_none_or` / `is_some_and` 优于手写 `map_or`
- 模块内私有函数不标 `pub`；`lib.rs` 只 `pub mod` 五个模块 + `pub const VIEW`
- 注释解释**为什么**（尤其那些"看起来多余"的守卫），不复述代码。现有注释是范例：
  `// The clock guard never fires before a few interfaces went out, so an unexpected clock cannot shrink a batch to one.`

---

## 4. Validation & Error Matrix

| 违反 | 后果 | 检查手段 |
| - | - | - |
| 生产代码 `unwrap()`/`expect()` | 组件 trap，用户侧零信息 | `Select-String -Path src/*.rs -Pattern 'unwrap\(\)\|expect\(\|panic!'` 后核对行号是否都在测试模块 |
| `println!` | 输出丢失 | 同上，只应在 `examples/` |
| 新依赖未评估 | 体积膨胀、启动变慢 | 看 `dist/plugin.wasm` 前后大小 |
| SDK tag 与 `engines.openidea` 不同步 | 组件加载失败 | 对比 `Cargo.toml` 的 `tag` 与 `plugin.toml` 的 `engines.openidea` |
| 设置键只改一边 | 设置永远读不到（无报错） | 对比 `settings.schema.json` 与 `setting_*` 调用 |
| 新命令只改清单 | 点了没反应，日志 `command ... not found` | 对比清单 id 集合与 `run_command` 的 `match` 分支 |

---

## 5. Good / Base / Bad Cases

**Good：可选数据的读写静默降级**

```rust
fn save_categories(project_id: &str, map: &HashMap<String, String>) {
    if let Ok(raw) = serde_json::to_string(map) {
        let _ = host::storage::set(&format!("{CATS_KEY}{project_id}"), &raw);
    }
}
```

缓存写失败不该让导出失败——下一批会重新查服务器，功能仍然正确，只是慢一点。

**Base：必须失败的地方就失败**

`config()` 缺令牌、队列 JSON 损坏、服务器拒绝令牌：这三类直接 `Err`，因为继续下去只会产生更糟的结果。

**Bad：为了少写一行而 `unwrap`**

```rust
let project = ctx.project().unwrap();   // 用户没打开项目 → 组件 trap
```

正确写法是 `let project = ctx.project().ok_or("先打开一个项目")?;`。

---

## 6. Tests Required

| 项 | 要求 |
| - | - |
| 覆盖新分支 | 新增 `run_command` 分支必须有测试，用 `MockHost` 触发 |
| 断言用户可见文本 | 断言返回的汇总文本/通知内容，而不是只断言行数 |
| 断言协议字段 | 涉及 YApi 时断言请求 body 的具体字段（`project_id`/`catid`/`token`/Path Schema） |
| 测试不联网 | 全部走 `MockHost::on_fetch`，禁止真实 HTTP |
| 命名 | 句子式小写英文，描述行为：`a_stale_category_id_is_rebuilt_once`、`the_schedule_advances_the_queue_without_a_click` |

统一入口：`cd` 到仓库根，`cargo test`。当前 31 个用例，全绿是基线。

---

## 7. Wrong vs Correct

### 用 `?` 直接吞掉宿主错误语义

#### Wrong

```rust
host::post_view_message(VIEW, &message.to_string())?;   // 视图不可用就让整个命令失败
```

#### Correct

```rust
fn post(message: Value) {
    let _ = host::post_view_message(VIEW, &message.to_string());
}
```

推消息是尽力而为：窗口没打开时消息没人收，但命令本身该继续完成。

### 把"降级"写成"忽略"

#### Wrong

```rust
let scanned = self.routes(&project).unwrap_or_default();  // 扫描失败静默变成"零接口"
```

用户会以为项目里没有接口，而不是"扫描出错了"。

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

降级 + 一行 Warn 日志。零接口与"扫描失败"在日志里必须可区分。
