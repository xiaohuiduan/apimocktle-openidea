# ApiMocktle for OpenIDEA

[English](README.md) | **简体中文**

把 IntelliJ 版 [ApiMocktle-java-plugin](https://github.com/xiaohuiduan/ApiMocktle-java-plugin) 的核心能力搬到
[OpenIDEA](https://openidea.sh/zh/plugins) 的插件模型上：从 Java / Kotlin 源码提取 HTTP 接口，浏览、跳转，
并同步到 ApiMocktle。本页是中文版；[英文版在这里](README.md)。

插件是一个 WebAssembly 组件，通过 `plugin.toml` 声明贡献点，代码只经 `wit/` 暴露的宿主接口访问项目和网络。

## ApiMocktle 是什么

[ApiMocktle](https://github.com/xiaohuiduan/ApiMocktle) 是一个本地运行的 API 管理桌面应用
（Tauri + React + Rust + SQLite）：接口文档、请求调试、自动化测试编排、OpenAPI / Swagger 导入导出与
局域网分享都在本机完成。本插件负责其中「从代码生成接口文档」这一段：扫描项目源码，用个人令牌把接口
写进 ApiMocktle 的项目目录。

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
- **路由常量**：`@PostMapping(RouteConstant.ITEM_QUERY)` 这种把路径写成 `static final` 常量的写法，会跨文件
  解析常量值。常量按**声明它的类的全限定名**识别：`com.a.Routes.PATH` 与 `com.b.Routes.PATH` 是两回事，
  即使两个类同名、甚至两个控制器各 import 一个同名 `Routes` 也各取各的；注解里写全限定名时包名参与识别。
  不带限定名时先看当前类，再看 `import static`，只有全项目唯一声明才按名字兜底，歧义时退回 `/` 而不是猜一个。
  支持类级 `@RequestMapping` 前缀；解析不到时退回 `/`，不影响同类的其他接口。
- **接口标题**：优先 `@Operation(summary)`（Swagger），其次方法 Javadoc，最后方法名；方法没有自己的 Javadoc
  时**不会**拿类注释顶上。
- **参数分类**：`@PathVariable` / `@RequestParam` / `@RequestHeader` / `@RequestBody` / `@CookieValue` / `@RequestPart`，
  Java 与 Kotlin 两种参数写法都识别。Form 类参数（`@RequestPart` / `@FormParam` / `@ModelAttribute`）写入
  `req_body_form` 且 `req_body_type` 为 `form`；Cookie 参数只识别、不导出（上游 IntelliJ 版同样不导出）。
- **仪表盘**：底部工具窗口，按控制器分组、搜索、查看路径与参数、跳到源码。
- **导出**：个人令牌 + 目标项目下拉选择，自动创建分类、按 `method + path` 识别已有接口并更新；
  请求/响应体以 JSON Schema 写入：项目源码里定义的包装类型（`Result<T>`、`ApiResponse<T>`…）保留自己的
  字段，并把泛型实参代入 `data` 等字段（`Result<List<UserDto>>` 导出为 `{code, msg, data: [...]}`）；
  解析不到定义的框架包装（`ResponseEntity<T>`、`Optional<T>`）仍自动拆开。`Map<K, V>` 按 K 生成一个
  示例键写进 `properties`（ApiMocktle 与 YApi 一样只渲染 `properties`，`additionalProperties` 会显示为 `{}`）。
  认 `@JsonProperty` 改名、`@JsonIgnore` 排除、record 组件与 Kotlin 构造器属性。
- **依赖 jar 里的类型**：源码里没有的 DTO / 包装类（`ApiEnvelope<T>` 这类放在公共 jar 里的）会找到本地
  Maven 仓库里的 jar，读 `.class` 文件补全字段、泛型参数和父类字段
  （`class ApiEnvelope<T> extends BaseReply` 会导出 `BaseReply` 的 `code`/`message` 加自己的
  `errorNo`/`errorInfo`/`result: T`）。坐标优先用 IDE 已经解析好的依赖图（`build.read` 授权）：
  多模块、BOM、版本冲突都由 IDE 算过，插件不再自己遍历 POM，也不用猜版本；没有这条授权时退回自己读
  POM（含传递依赖），结果通常相同，只是慢一些。jar 按 import 包名匹配 Maven group / artifact 排序，
  命中的通常只读一个 jar；每个项目第一次扫描会记住“类 → jar”，后续扫描直接命中。真正读仓库仍需
  `fs.read` 权限；两条都没授权时只导出泛型实参。
- **Search Everywhere**：扫描到的接口出现在 Endpoints 分组，声明处还有 **Open in HTTP Client** 可用。

不包含：发请求、复制 curl、字段转 JSON、连接测试——这些已按要求移除。

## 安装

### 从已构建的产物

`dist/plugin.wasm` 已随仓库提供。执行 `pwsh script/package.ps1` 会重建组件并打出
`dist/apimocktle-openidea-<version>.zip`（含 `plugin.toml`、`settings.schema.json`、`locales/`、`icons/`、
`README.md` 与 `dist/plugin.wasm`），在 **设置 › 插件 › ⚙ › 从插件包文件安装…** 装上即可。
升级后在插件详情里点 **Reload（重载）** 即可生效，不需要重启整个 IDE。

### 开发期链接

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
pwsh script/package.ps1      # 同时刷新 dist/plugin.wasm

# 需要 openidea-plugin（校验 + 链接）：
cargo install --locked --git https://github.com/l0ng-ai/openidea ide-host --tag v0.1.50 --bin openidea-plugin
openidea-plugin check
openidea-plugin link
```

`ide-host` 依赖 wasmtime 49，编译它需要 rustc ≥ 1.96；本机若是更低版本，`check`/`link` 就跑不了，
改用打好的 zip 走「从插件包文件安装」这条路径。修改代码后重新构建，再在插件详情里点 **Reload（重载）**。

本机没有 MSVC 链接器（PATH 里的 `link.exe` 是 coreutils 的 `link`）时，用 GNU 工具链跑同样的步骤：

```sh
rustup toolchain install 1.96.0-x86_64-pc-windows-gnu --profile minimal
rustup target add wasm32-wasip2 --toolchain 1.96.0-x86_64-pc-windows-gnu
$env:CARGO_TARGET_DIR = "$env:TEMP\apimocktle-target"   # 项目路径含中文时，GNU ld 打不开非 ASCII 的输出路径
cargo +1.96.0-x86_64-pc-windows-gnu test
pwsh script/package.ps1 -Toolchain 1.96.0-x86_64-pc-windows-gnu   # 会沿用 CARGO_TARGET_DIR
```

组件要和宿主对齐：`wasm-tools component wit dist/plugin.wasm` 应列出 `wasi:*` 与
`openidea:plugin/*@1.14.0`，导出 `commands`、`lifecycle`、`event-handler`、`endpoints-provider`、
`view-provider` 五个 interface。

### 本机安装说明（Windows，OpenIDEA 0.1.45）

`%APPDATA%\ai.l0ng.openidea\plugins\apimocktle.openidea\<version>\` 是解包后的插件目录，
`grants.json` 记录 `apimocktle.openidea` 的授权（`project.read`、`storage`、`net:localhost`、`net:127.0.0.1`）。
授权项可在插件详情里逐项撤销，改动前的账本备份为 `grants.json.bak`。

> 手写 `grants.json` 时必须用**无 BOM 的 UTF-8**。带 BOM 时宿主反序列化失败，账本被静默重置为空，
> 插件停在「需要授权」且日志不报错——第一次尝试就栽在这里。

## 真实项目验证

在几个中大型 Spring 多模块工程上跑过（数百个 Java 源文件、二十多个含映射注解的文件），这类工程的名字不写进仓库：

- 提取几十个接口（Spring 与 Feign 混合），多模块路径前缀、参数分类、`Result<List<T>>` 返回类型都正确；
- `@FeignClient(value = "服务名")` 不再被当成路径前缀——这个 bug 就是真实项目暴露后修掉的；
- 带 `@RestController` + `@GetMapping` 的 XXL-Job handler 被正确识别为接口（项目作者有意暴露的触发端点）；
- 路径写成 `static final` 常量、包装类放在依赖 jar 里的工程，也都能正确解析（见上面「功能」最后两段）。

复跑同样的扫描，或量一下不同展开深度下每条接口的 Schema 体积：

```sh
cargo run --example scan_dir -- C:\path\to\project
cargo run --example schema_size -- C:\path\to\project
```

`schema_size` 是上面「默认深度 3」这个结论的来源：它按 1–4 层各算一遍总字节数和最大单条，用来判断再深一层
还值不值。两个 example 都只读源码，不需要工程能编译。

### 大项目与扫描成本

`scan` 的成本不在解析，而在**跨宿主边界的调用次数**：每个 DTO 类型曾要两次全项目文件查找
（`.java` + `.kt`），400 个类型就是 800 次往返，5 秒预算就这么没了。现在整项目只建**一次类型索引**，
按文件名查表；查不到时才退回一次声明搜索，且结果会被记住。

仪表盘和「跳到源码」只显示路由，不需要 DTO 字段，因此走**仅路由档**，不解析 DTO 闭包。

复现与测量用三个工具（都在 `examples/`，不进 wasm 产物）：

```sh
cargo run --release --example gen_demo   -- --preset big        # 生成可调规模的 Spring 工程
cargo run --release --example scan_bench -- "%TEMP%\apimocktle-demo\big"
cargo run --release --example component_api -- dist/plugin.wasm # 读组件的接口版本
```

`big` 档是 6 模块 / 300 控制器 / 1214 接口 / 900 DTO。实测（MockHost，每次往返按 3 ms 估）：

| 档位 | 宿主往返 | 估算耗时 |
| - | - | - |
| 导出（含 DTO） | 716 次 | 2.1 s |
| 仪表盘（仅路由） | 316 次 | 0.9 s |

修复前的同类项目是 1526 次。**注意**：`MockHost` 是内存实现，`read_file` / `find_files` 是哈希查找，
所以绝对毫秒数不能当真，能外推的只有往返次数。

> 用真实工程验证时请只记结论，别把工程名、包名、接口名写进代码或文档——`tests/` 和 `fixtures/` 里一律用
> `com.acme` / `com.example` 这类示例命名。

## 配置

**设置 › ApiMocktle**：

| 项 | 说明 |
| - | - |
| 服务器地址 | ApiMocktle 服务器地址，默认 `http://localhost:4000` |
| 个人令牌 | 在 ApiMocktle 里生成；保存在本机插件设置中 |
| 同时扫描 Kotlin 源码 | 默认开 |
| 请求/响应体展开深度 | DTO 递归层数，1–4，默认 3 |
| 解析依赖 jar 里的类型 | 默认开；从 Maven 本地仓库读依赖 jar 补全源码里没有的 DTO / 包装类。需要 `fs.read` 授权 |
| Maven 本地仓库 | 留空即用 `~/.m2/repository`（本机仓库在那儿时不用填）。换过目录就填绝对路径 |

仓库路径**手填**，没有目录选择器：设置是独立窗口，弹不出系统对话框，插件也没有运行期申请文件权限的办法。

### 界面语言

插件文案跟随 IDE 的界面语言（设置里的 `appearance.locale`），中文界面显示中文、英文界面显示英文。
两种机制分工明确：

- **清单、设置项、视图工具栏按钮**由宿主按包内 `locales/<tag>.json` 翻译。这些位置在
  `plugin.toml` 与 `settings.schema.json` 里以英文源文本书写，中文译文在 `locales/zh-CN.json`
  （键为英文源文本）。没有目录条目的语言显示英文。
- **仪表盘页面、通知、错误提示**由组件自己在生成时读取 `host::locale()` 选择语言
  （`src/i18n.rs`），页面文案表在 `src/page.html` 里。

视图只渲染一次，宿主没有语言切换事件，也不会自己重渲染 HTML 正文。所以页面加载后
**主动向组件询问当前语言**（之后每 5 秒再问一次），组件回 `host::locale()` 的值，页面**原地换文案**——
列表、当前选中的接口、状态行都保留，不用重开视图。切换语言后几秒内就会跟上。
也可以立刻生效：关掉再打开仪表盘，或运行命令 *ApiMocktle: 重新扫描接口*（它调 `host::refresh_view`，
会重建页面）。视图内和工具栏上的「重新扫描」只清缓存并发消息，不会重渲染页面。
插件市场里的「概览」文案（`plugin.toml` 的 `description`）与 `change-notes` 不参与翻译，
始终显示中文：宿主把这两项原样渲染，没有翻译入口。

### 换了仓库目录时，权限怎么配

`fs.read` 是安装时按 `plugin.toml` 一次性授权的，**运行期改不了**，而且匹配是字面量前缀比较——授权
`~/.m2/repository` 不会覆盖 `D:\maven\repo`。所以两件事要一起做：

1. **设置页**填新路径（`插件 › ApiMocktle › Maven 本地仓库`）；
2. **`plugin.toml`** 里把 `optional` 改成新目录：

   ```toml
   [capabilities]
   required = ["project.read", "storage", "net:localhost", "net:127.0.0.1"]
   optional = ["fs.read:~/公司仓库/m2"]        # 默认那条不再用就删掉，两个都留就都写上
   ```

   然后重新打包安装，在 **设置 › 插件 › ApiMocktle › 权限** 里勾上这条新授权。

漏了第 2 步时插件不会静默失败：会弹一条通知、并在日志里写明该把 `fs.read:` 改成什么路径，接口的依赖类型
退化成 `object`，其余功能照常。

分类固定按控制器类名生成（`TicketOrderController`、`PayController`…），不提供统一分类的开关。

## 使用

1. 打开一个 Spring / Kotlin 项目，底部 **ApiMocktle** 工具窗口会扫描并按控制器列出接口
   （也可以在命令面板运行 *ApiMocktle: 打开 API 仪表盘*）。
2. 选中接口查看路径、参数与来源，点 **跳到源码** 定位到注解所在行。
3. 顶栏下拉选择目标项目，**导出全部**（命令面板里是 *ApiMocktle: 导出项目接口*）；或对文件/目录右键
   **ApiMocktle: 导出当前文件接口**（`Alt+Shift+E`）。
4. `Ctrl/Cmd+Shift+A` → 搜索接口，或在 `.http` 文件里直接补全 URL。

导出一次点击就够。发送过程自己开一个后台任务（IDE 的后台任务区显示「ApiMocktle: 同步接口」及其进度），
把这次调用的墙钟从宿主的 5 秒放宽到 60 秒、每发一个接口就续期一次，所以一批的上限是 **50 秒**而不是接口条数：
实测速度下（每个接口约 7.7 毫秒）三千多个接口约 30 秒发完，整个项目一次点完，不需要中途再点「继续」。
只有服务器慢到 50 秒还发不完时，剩下的才进队列，由 `on-schedule:60` 每分钟自动推进一批，状态栏显示
「同步中 · 剩 N」，全部完成时补一条通知。

想立刻推下一批，仍可点通知里的 **立即继续** 或运行命令 *ApiMocktle: 继续同步剩余接口*。队列存在插件存储
里，关窗口不丢，重启 IDE 后会接着走。

队列记住自己是为哪个服务器、哪个令牌建的。导出进行中改了服务器地址或令牌，剩余批次**暂停**而不是改投到
新服务器：状态栏显示「同步已暂停 · 队列属于另一服务器」，点通知或改回原设置都能继续；确实要发到新服务器，
重新点「导出全部」即可。

## 权限与网络

清单里声明的能力：

- `project.read`：读取项目文件、文本搜索、文件枚举。
- `storage`：导出队列、分类 id 缓存、jar 查找结果与扫描缓存。
- `net:localhost`、`net:127.0.0.1`：访问本机 mock 服务器（回环地址允许明文 HTTP）。
- `fs.read:~/.m2/repository`（**optional**）：读项目之外的依赖 jar。安装/升级插件时在权限列表里勾选，
  或在 **设置 › 插件 › ApiMocktle › 权限** 里授予；不授权时插件照常工作，只是 jar 里的类型退化成 `object`。
  仓库换了位置（比如公司 `settings.xml` 里的 `localRepository`）时，把这条 `fs.read:` 改成新目录后重新安装
  并授权——授权路径是静态声明的，运行期无法追加（见上文「换了仓库目录时，权限怎么配」）。
- `build.read`（**optional**）：读 IDE 已解析的 Maven / Gradle 依赖图，用它代替自己遍历 POM。
  不授权时退回自己解析 POM，功能不变。需要项目处于 trusted 状态，否则宿主返回 `denied`，同样是静默退回。

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
- **路由常量只认纯字面量**：`Constants.PREFIX + "/x"` 这种拼接、以及 `String.format(...)`、枚举常量都不解析，
  这类接口的路径会退回 `/`。常量的声明类按全限定名识别，但同名常量分属多个类、又既没写限定名也没 import 时
  无法判断，同样退回 `/`。嵌套在别的类里的常量不支持（源码解析只读顶层类）。
- **依赖 jar 解析的边界**：只走 Maven（读项目 `pom.xml` 的依赖与传递 POM，最多 200 个 POM），Gradle 项目
  目前读不到仓库依赖；jar 要能在本地仓库里找到（`~/.m2/repository` 或设置里的路径），单个 jar 超过 12 MiB
  就跳过，一次扫描最多读 120 个 jar、单个类最多 32 个（超了就当解析不到）；class 里的 `@JsonProperty` 等
  Jackson 注解不读（源码 DTO 才读），Kotlin `@Metadata` 不解析（字段本身能读）；没有源码的接口/抽象类只
  合并父类字段。拿不到 `fs.read` 授权时自动退回旧行为。一次扫描读到的类会按项目记进插件存储，下一次扫描
  直接命中同一个 jar。
- **Map 用示例键渲染**：`Map<K, V>` 导出为 `{ "type": "object", "properties": { "<K 的示例名>": V } }`，
  而不是 `additionalProperties`——后者在 ApiMocktle / YApi 的字段表里是空对象。
- **命令预算：仪表盘 50 秒，导出 50 秒**。宿主对单次插件调用默认限时 5 秒，但 `host::begin_task`
  会把这次调用的墙钟放宽到 60 秒，且每次 `report` 都续期。仪表盘和导出各开一个后台任务、拿到 50 秒预算，
  代价是 IDE 的后台任务区会显示「ApiMocktle: 解析请求响应类型」或「同步接口」及其进度。
  两条路径都没收完就返回已找到的部分并提示结果可能不全，而不是被宿主掐掉。超大项目另外会命中文本搜索上限。
- **状态栏的接口列表来自仪表盘的扫描结果**：宿主给「列出接口」那次调用只有 2 秒的硬限时，而大项目上
  一次全项目搜索就要 3.7 秒，从那里发起扫描必被杀掉、插件被回收（表现为「Plugin ApiMocktle crashed」）。
  所以那次调用只读缓存：仪表盘和导出扫完会把路由写进插件存储，状态栏与 `Ctrl/Cmd+Shift+A` 的接口搜索
  直接读它。**首次打开一个还没扫过的项目时状态栏是空的**，打开一次仪表盘即可填上；文件改动会由 IDE 自己
  让这份缓存失效并重新询问。
- **DTO 解析没跑完时不导出**：路由列表不完整只是显示问题，可以退化成「接口少几条」；DTO 图不完整则是
  另一种性质——按不完整的图生成的 Schema 会缺字段，而导出是按 `method + path` 更新已有接口的，发出去
  就会把服务器上原本正确的文档覆盖成更差的一份，并且还报告「已全部同步」。所以 DTO 闭包没跑完时，导出会
  直接拒绝、一条请求都不发，并说明原因；这份不完整结果也不会写进完整扫描缓存，重试会重新扫描。
  仪表盘只看路由，不受影响。
- **两种「没跑完」分开报**：`ScanResult::dto_stop` 区分 `Deadline`（墙钟用完，值得重试或用更小范围）
  和 `TypeBudget`（一次扫描解析的类型数达到上限）。后者与时间无关，重试会停在同一处，所以文案给的是
  「缩小范围」而不是「重试」。上限是 5000，远高于实测最大的真实项目，只用来兜住环状或生成的模型。
- **「导出当前文件/目录」真的缩小范围**：`Options::only_path` 下推到扫描阶段，只解析选中的控制器、
  只遍历它们用到的 DTO，而不是扫完整个项目再过滤接口列表。收窄的结果不进全量缓存。
- **诊断级别的位置**：接口位置取注解所在行列（0-based 行、UTF-16 列），用于跳转，不做语法高亮。
- **不做 mock 运行时**：没有 JVM agent，只在源码层面对齐。

## 代码结构

| 文件 | 作用 |
| - | - |
| `src/java.rs` | Java/Kotlin 词法分析：标识符、字符串、注释、括号匹配、UTF-16 位置 |
| `src/spring.rs` | 结构化解析：类/方法/字段/参数/注解，生成 `RawEndpoint` |
| `src/model.rs` | 接口模型、泛型拆分、JSON Schema 生成 |
| `src/scan.rs` | 项目扫描：文本搜索 → 解析 → DTO 传递闭包（项目源码 + 依赖 jar） |
| `src/deps.rs` | 依赖 jar 解析：POM 依赖树 → 本地仓库 jar → 类定义，带缓存与读取预算 |
| `src/classfile.rs` | `.class` 文件解析：字段、泛型 `Signature`、父类与类型参数 |
| `src/zip.rs` | jar 的最小 ZIP 读取（中央目录 + stored/deflate 条目） |
| `src/yapi.rs` | YApi 协议客户端与接口文档构造 |
| `src/lib.rs` | 命令、视图、`endpoints` 提供器、分批导出队列 |
| `src/i18n.rs` | 宿主不代翻的文案（页面/通知/错误），按 `host::locale()` 选语言 |
| `src/page.html` | 仪表盘页面（沙箱 iframe，与插件消息通信） |

## 测试

```sh
cargo test                      # 115 个用例：解析、Schema、ZIP/class、POM、路由常量、类型索引、扫描预算、DTO 超时不导出、YApi 编码、队列、i18n，以及端到端链路
node script/check-pages.mjs     # 仪表盘在两种语言下的真实渲染（cargo test 跑不了 JS）
pwsh script/package.ps1         # 重建组件并打包（含 icons/ 与 locales/）
```

端到端用例跑在 SDK 的 `MockHost` 上：`tests/fixtures_export.rs` 把 `fixtures/spring-demo` 下的全部
`.java`/`.kt` 注入成项目文件，走 `scan` → `api_doc` → `save_api` 全链路，断言 Feign 服务名不进路径、
`Result<List<Order>>` 的 `data` 是数组、Kotlin 与 JAX-RS 资源都被识别、multipart 接口带两个 form part；
`tests/unknown_wrapper.rs` 只放 Controller 和 DTO，让 `ApiEnvelope` 像依赖 jar 一样解析不到，断言
`ApiEnvelope<Map<String, List<ItemInfoDTO>>>` 的响应仍是可读的 `Map` + DTO 字段；
`tests/jar_deps.rs` 在内存里搭一个 Maven 仓库（POM 链 + `fixtures/deps/api-envelope.jar`），断言传递依赖里的
`ApiEnvelope` 连父类 `code`/`message` 和泛型实参一起导出、实现了接口的包装类（`JailedEnvelope`）仍然保留
泛型载荷字段，并验证没授权时安静退回；
`tests/route_constants.rs` 覆盖“路径写成常量”的扫描：限定名按声明类解析（两个类各有 `PATH` 也不串）、
两个包各有一个同名 `Routes` 时按各自 `import` 解析、注解里写全限定名时包名参与识别、
不带限定名时当前类优先、`import static` 可解析、同名歧义不猜；
`tests/translations.rs` 按宿主的目录标签匹配规则校验 `locales/zh-CN.json`：中文界面能命中、英文界面保留源文本、
文件名是合法语言标签，且清单与设置里的每条可见文本都有中文条目。

`fixtures/spring-demo` 只是扫描输入，不需要能编译或启动——插件读的是源码文本；`fixtures/deps` 里的
`api-envelope.jar` 是 `BaseReply.java` / `ApiEnvelope.java` / `PageRequest.java` / `GenericBase.java` /
`JailedEnvelope.java` / `PagedEnvelope.java` 用 `javac --release 8` 编译出的测试输入（后两个专门带接口与
泛型父类，用来盯住 class Signature 的解析）。测试与文档里一律用 `com.acme` / `com.example` 这类示例命名，
真实工程的类名不写进仓库。

SDK 版本固定在 `Cargo.toml` 的 tag（当前 `v0.1.50`，插件 API 1.14）；升级 IDE 时同步改 tag 与
`engines.openidea`，两者必须匹配，否则组件与宿主的 WIT 接口对不上。新构建的组件导入表是
`openidea:plugin/*@1.14.0`，并导出 `commands`、`lifecycle`、`view-provider`、`endpoints-provider`、
`event-handler`；用 `cargo run --example component_api -- dist/plugin.wasm` 可以随时核对组件的接口版本。

`dist/plugin.wasm` 随源码一起提交，发版前用 `script/package.ps1` 重建，并用 `cargo run --example component_api -- dist/plugin.wasm` 核对接口版本。

## 发布

流程就是 registry 手册的两条命令，没有额外的构建步骤：

```sh
openidea-plugin build .                                    # 产出 dist/plugin.wasm
openidea-plugin pack . --key ~/.openidea-keys/apimocktle.key --publisher apimocktle
```

`script/release.ps1` 把「打包 → 建 release → 上传附件」串成一条命令，并打印要写进
[l0ng-ai/openidea-plugins](https://github.com/l0ng-ai/openidea-plugins) 的 `version` / `package` / `sha256`：

```sh
git tag v0.3.1 && git push origin v0.3.1
pwsh script/release.ps1            # -DryRun 只构建签名，不碰 release
```

签名密钥只在**本机**：它不入库（`.gitignore` 里的 `*.key`），也不放进 CI secrets。任何人读到它就能以
`apimocktle.*` 的名义发布，所以这一步留在本机，交给一条可重复的命令，而不是交给流水线。
