# ApiMocktle for OpenIDEA

把 IntelliJ 版 [ApiMocktle-java-plugin](https://github.com/xiaohuiduan/ApiMocktle-java-plugin) 的核心能力搬到
[OpenIDEA](https://openidea.sh/zh/plugins) 的插件模型上：从 Java / Kotlin 源码提取 HTTP 接口，浏览、跳转、复制 curl，
并同步到 ApiMocktle / YApi 兼容的 mock 服务器。

插件是一个 WebAssembly 组件，通过 `plugin.toml` 声明贡献点，代码只经 `wit/` 暴露的宿主接口访问项目和网络。

## 和 IntelliJ 版的关系

OpenIDEA 插件运行在 WASM 沙箱里，没有 Java PSI、没有 Swing、没有 JVM agent。所以这不是移植，而是按 OpenIDEA
的能力重新实现的一版；语义保留，实现路径不同。

| IntelliJ 版 | 本插件 |
| - | - |
| Java PSI + 类型解析 | 自写词法分析 + 结构解析（`src/java.rs`、`src/spring.rs`） |
| ToolWindow（Swing / JBList） | `[[contributes.views]]` + 沙箱 HTML 视图（`postMessage` 通信） |
| AnAction / 右键菜单 | `[[contributes.commands]]` + `[[contributes.menus]]` |
| PersistentStateComponent | `host::storage` + `[[contributes.settings]]` |
| Apache HttpClient | `host::fetch` |
| GotoDeclaration | `host::open_location` |
| Goto Symbol 全局搜索 | `[[contributes.endpoints]]`：接口直接进 **Search Everywhere**，并在 `.http` 文件里参与 URL 补全 |

原插件的 `mock-agent`（JVM 运行时拦截）在 OpenIDEA 插件模型里没有对应物，本插件不包含这部分。

## 功能

只有两件事：**看接口** 和 **导出到 ApiMocktle**。

- **接口扫描**：Spring MVC（`@RestController`、`@RequestMapping`、`@GetMapping` 等）、JAX-RS（`@Path` + `@GET`）、
  Feign（`@FeignClient`）；Java 与 Kotlin（含 `data class`、`fun ... : T` 返回类型）。
- **参数分类**：`@PathVariable` / `@RequestParam` / `@RequestHeader` / `@RequestBody` / `@CookieValue` / `@RequestPart`，
  Java 与 Kotlin 两种参数写法都识别。Form 类参数（`@RequestPart` / `@FormParam` / `@ModelAttribute`）写入
  `req_body_form` 且 `req_body_type` 为 `form`；Cookie 参数只识别、不导出（上游 IntelliJ 版同样不导出）。
- **仪表盘**：底部工具窗口，按控制器分组、搜索、查看路径与参数、跳到源码。
- **导出**：个人令牌 + 目标项目下拉选择，自动创建分类、按 `method + path` 识别已有接口并更新；
  请求/响应体以 JSON Schema 写入：项目源码里定义的包装类型（`Result<T>`、`ApiResponse<T>`…）保留自己的
  字段，并把泛型实参代入 `data` 等字段（`Result<List<UserDto>>` 导出为 `{code, msg, data: [...]}`）；
  解析不到定义的框架包装（`ResponseEntity<T>`、`Optional<T>`）仍自动拆开。认 `@JsonProperty` 改名、
  `@JsonIgnore` 排除、record 组件与 Kotlin 构造器属性。
- **Search Everywhere**：扫描到的接口出现在 Endpoints 分组，声明处还有 **Open in HTTP Client** 可用。

不包含：发请求、复制 curl、字段转 JSON、连接测试——这些已按要求移除。

## 安装

### 从已构建的产物

`dist/plugin.wasm` 已随仓库提供。执行 `pwsh script/package.ps1` 会重建组件并打出
`dist/apimocktle-openidea-<version>.zip`（含 `plugin.toml`、`settings.schema.json`、`dist/plugin.wasm`），
在 **设置 › 插件** 的齿轮菜单里选 **从插件包文件安装…** 装上即可。

### 开发期链接

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
pwsh script/package.ps1      # 同时刷新 dist/plugin.wasm

# 需要 openidea-plugin（校验 + 链接）：
cargo install --locked --git https://github.com/l0ng-ai/openidea ide-host --tag v0.1.44 --bin openidea-plugin
openidea-plugin check
openidea-plugin link
```

`ide-host` 依赖 wasmtime 49，编译它需要 rustc ≥ 1.96；本机若是更低版本，`check`/`link` 就跑不了，
改用打好的 zip 走「从插件包文件安装」这条路径。修改代码后重新构建，再在插件详情里点 **从磁盘重新加载**。

### 本机已链接（Windows，OpenIDEA 0.1.44）

`%APPDATA%\ai.l0ng.openidea\plugins\links.json` 已指向本目录，`grants.json` 已记录
`apimocktle.openidea` 的授权（`project.read`、`storage`、`net:localhost`、`net:127.0.0.1`、`clipboard`）。
宿主只在启动和 `openidea-plugin link` 时读取链接清单，所以**重启 OpenIDEA** 后插件才会被加载。授权项可在插件
详情里逐项撤销，改动前的账本备份为 `grants.json.bak`。

> 手写 `grants.json` 时必须用**无 BOM 的 UTF-8**。带 BOM 时宿主反序列化失败，账本被静默重置为空，
> 插件停在「需要授权」且日志不报错——第一次尝试就栽在这里。

## 真实项目验证

在 `opengoofy/index12306`（414 个 Java 源文件、24 个含映射注解的文件）上跑：

- 提取 57 个接口（Spring 47、Feign 10），多模块路径前缀、参数分类、`Result<List<T>>` 返回类型都正确；
- `@FeignClient(value = "服务名")` 不再被当成路径前缀——这个 bug 就是真实项目暴露后修掉的；
- 带 `@RestController` + `@GetMapping` 的 XXL-Job handler 被正确识别为接口（项目作者有意暴露的触发端点）。

复跑同样的扫描：

```sh
cargo run --example scan_dir -- C:\path\to\project
```

## 配置

**设置 › ApiMocktle**：

| 项 | 说明 |
| - | - |
| 服务器地址 | ApiMocktle / YApi 根地址，默认 `http://localhost:4000` |
| 个人令牌 | 在 ApiMocktle 里生成；保存在本机插件设置中 |
| 同时扫描 Kotlin 源码 | 默认开 |
| 请求/响应体展开深度 | DTO 递归层数，1–4，默认 3 |

分类固定按控制器类名生成（`TicketOrderController`、`PayController`…），不提供统一分类的开关。

## 使用

1. 打开一个 Spring / Kotlin 项目，底部 **ApiMocktle** 工具窗口会扫描并按控制器列出接口。
2. 选中接口查看路径、参数与来源，点 **跳到源码** 定位到注解所在行。
3. 顶栏下拉选择目标项目，**导出全部**；或对文件/目录右键 **ApiMocktle: 导出当前文件接口**（`Alt+Shift+E`）。
4. `Ctrl/Cmd+Shift+A` → 搜索接口，或在 `.http` 文件里直接补全 URL。

导出一次点击就够。宿主对单次插件调用的墙钟预算是 5 秒，而 `host::fetch` 是同步的，所以批次有上限；
不过本机没有网络延迟，一个接口一次往返只有几毫秒，56 个接口不到一秒就写完。真正会触发分批的是慢服务器：
循环在 4.2 秒处收手，剩下的进队列，插件靠 `on-schedule:60` 每分钟自动推进一批，状态栏显示
「同步中 · 剩 N」，全部完成时补一条通知。

想立刻推下一批，仍可点通知里的 **立即继续** 或运行命令 *ApiMocktle: 继续同步剩余接口*。队列存在插件存储
里，关窗口不丢，重启 IDE 后会接着走。

队列记住自己是为哪个服务器、哪个令牌建的。导出进行中改了服务器地址或令牌，剩余批次**暂停**而不是改投到
新服务器：状态栏显示「同步已暂停 · 队列属于另一服务器」，点通知或改回原设置都能继续；确实要发到新服务器，
重新点「导出全部」即可。

## 权限与网络

清单里声明的能力：

- `project.read`：读取项目文件、文本搜索、文件枚举。
- `storage`：导出队列、分类 id 缓存与扫描缓存。
- `net:localhost`、`net:127.0.0.1`：访问本机 mock 服务器（回环地址允许明文 HTTP）。

宿主只放行清单里声明过的主机。服务器在局域网或走 HTTPS 时，需要改 `plugin.toml`：

```toml
[capabilities]
required = ["project.read", "storage", "net:api.example.com"]   # HTTPS
# 或明文 HTTP：加 "net.http:192.168.1.20"
```

改完重新构建、重新授权。这不是插件的取舍，是宿主的能力模型。

## 已知限制

- **没有类型系统**：DTO 字段来自文本级解析，跨文件继承、复杂泛型推导、接口多态都不完整；`List<User>` 这类常见形状可靠，
  `Result<? extends Base<T>>` 这类会退化成 `object`。
- **5 秒命令预算**：扫描和导出都按这个上限设计（分批发送），超大项目会命中文本搜索上限，仪表盘会提示结果可能不全。
- **诊断级别的位置**：接口位置取注解所在行列（0-based 行、UTF-16 列），用于跳转，不做语法高亮。
- **不做 mock 运行时**：没有 JVM agent，只在源码层面对齐。

## 代码结构

| 文件 | 作用 |
| - | - |
| `src/java.rs` | Java/Kotlin 词法分析：标识符、字符串、注释、括号匹配、UTF-16 位置 |
| `src/spring.rs` | 结构化解析：类/方法/字段/参数/注解，生成 `RawEndpoint` |
| `src/model.rs` | 接口模型、泛型拆分、JSON Schema 生成 |
| `src/scan.rs` | 项目扫描：文本搜索 → 解析 → DTO 传递闭包 |
| `src/yapi.rs` | YApi 协议客户端与接口文档构造 |
| `src/lib.rs` | 命令、视图、`endpoints` 提供器、分批导出队列 |
| `src/page.html` | 仪表盘页面（沙箱 iframe，与插件消息通信） |

## 测试

```sh
cargo test                      # 49 个用例：解析、Schema、YApi 编码、队列目标绑定，以及扫描→文档→保存整链路
pwsh script/package.ps1         # 重建组件并打包（含 icons/）
```

端到端用例跑在 SDK 的 `MockHost` 上：`tests/fixtures_export.rs` 把 `fixtures/spring-demo` 下的全部
`.java`/`.kt` 注入成项目文件，走 `scan` → `api_doc` → `save_api` 全链路，断言 Feign 服务名不进路径、
`Result<List<Order>>` 的 `data` 是数组、Kotlin 与 JAX-RS 资源都被识别、multipart 接口带两个 form part；
`src/lib.rs` 的用例覆盖换服务器暂停、未换服务器继续、旧格式队列丢弃与分类缓存按服务器隔离。

`fixtures/spring-demo` 只是扫描输入，不需要能编译或启动——插件读的是源码文本。

SDK 版本固定在 `Cargo.toml` 的 tag（当前 `v0.1.44`，插件 API 1.7）；升级 IDE 时同步改 tag 与
`engines.openidea`，两者必须匹配，否则组件与宿主的 WIT 接口对不上。`dist/plugin.wasm` 的导入表已确认是
`openidea:plugin/*@1.7.0`，并导出 `commands`、`lifecycle`、`view-provider`、`endpoints-provider`。
