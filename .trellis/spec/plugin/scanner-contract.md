---
name: scanner-contract
description: 项目扫描与结构解析：候选文件、框架判定、路径拼接、参数分类、DTO 闭包、位置坐标
paths:
  - src/scan.rs
  - src/spring.rs
  - src/java.rs
---

# 扫描与解析契约

`src/java.rs`（词法）→ `src/spring.rs`（结构）→ `src/scan.rs`（项目级）是一条单向流水线。
任何一环改判定规则，都会同时改变仪表盘、Search Everywhere 和导出文档三处输出。
Schema 生成单独成篇，见 [schema-contract.md](./schema-contract.md)。

---

## 1. Scope / Trigger

- 改 `MARKERS`、`FILE_LIMIT`、`MATCH_LIMIT`、`TYPE_BUDGET`
- 改 `spring::parse` / `spring::endpoints` 的判定（注解名、参数 kind、路径拼接、摘要来源）
- 改 `scan::to_json` 的字段（页面和测试都吃它）
- 支持新的框架或注解

---

## 2. Signatures

```rust
// java.rs
pub struct Tok { pub kind: Kind, pub start: usize, pub end: usize }
pub struct Lexed { pub toks: Vec<Tok>, pub docs: Vec<(usize, usize)> }
pub fn lex(src: &str) -> Lexed;
pub struct LineIndex<'a>;
impl<'a> LineIndex<'a> { pub fn new(src: &'a str) -> Self; pub fn of(&mut self, byte: usize) -> (u32, u32); }

// spring.rs
pub fn parse(src: &str) -> FileModel;
pub fn endpoints(model: &FileModel, src: &str) -> Vec<RawEndpoint>;
pub fn summary_of(text: &str) -> String;
pub enum ParamKind { Path, Query, Header, Body, Cookie, Form }

// scan.rs
pub struct Options { pub include_kotlin: bool, pub depth: u32, pub resolve_dto: bool, pub file_limit: u32 }
pub struct DtoIndex { pub fields: HashMap<String, Vec<FieldSpec>>, pub params: HashMap<String, Vec<String>> }
pub struct ScanResult { pub endpoints: Vec<Endpoint>, pub dto: DtoIndex, pub files: usize, pub truncated: bool }
pub fn scan(project: &Project, options: &Options) -> Result<ScanResult, Error>;
pub fn to_json(result: &ScanResult) -> Value;
```

`DtoIndex` 把"类名 → 字段"与"类名 → 类型参数名"绑在一起：泛型实参替换需要两者同时可见，
拆成两个平行 HashMap 会让每个调用点多一个参数。字段与 Schema 规则见
[schema-contract.md](./schema-contract.md)。

---

## 3. Contracts

### 候选文件（两段式，带上限）

```rust
const MARKERS: &str = "@(RestController|Controller|RequestMapping|GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|FeignClient|Path)";
const FILE_LIMIT: u32 = 5_000;   // 参与解析的候选文件上限
const MATCH_LIMIT: u32 = 40_000; // 文本搜索返回的匹配上限
const TYPE_BUDGET: usize = 400;  // 一次扫描解析的 DTO 类型上限
```

1. `project.search_text(TextQuery { pattern: MARKERS, regex: true, include }, MATCH_LIMIT)`，
   `include` 为 `["**/*.java"]`，`include_kotlin` 为真时追加 `["**/*.kt"]`
2. 按 `location.path` 去重收集，达到 `file_limit` 即停；然后 `paths.sort()` 保证顺序稳定
3. 逐个 `read_file` → `spring::parse` → `spring::endpoints`

`truncated = found.limit_hit || paths.len() >= file_limit`，为真时页面提示"已达搜索上限，结果可能不全"。

### `Options` 的三套默认值（别当成笔误）

| 来源 | include_kotlin | depth | resolve_dto | file_limit |
| - | - | - | - | - |
| `Options::default()`（库默认，测试与示例用） | true | **2** | true | 5000 |
| `lib::scan_options()`（命令/仪表盘） | 读设置，缺省 true | 读设置，缺省 **3**，`clamp(1,4)` | true | 5000 |
| `lib::route_options()`（endpoints provider） | 同 scan_options | 同 scan_options | **false** | **400** |

`route_options` 的唯一目的是在宿主 2000ms 预算内只出路由，不做 DTO 解析。

### 框架判定（顺序即优先级）

```rust
@FeignClient                      → "Feign"
@RestController | @Controller     → "Spring"
（类上没有上述注解时）
  任一方法带 verb 注解 + 类上 @Path → "JAX-RS"
  任一方法带 verb 注解              → "Spring"
  否则                             → None（整个类跳过）
```

`@FeignClient` 必须最先判：Feign 接口常同时带 `@RequestMapping`，让 Spring 先命中就会标错框架，
并把服务名当路径前缀。

### 路径拼接

`class_prefix(class.anns) × 方法侧所有 path 注解` 的组合 → `join_path(prefix, sub)`。

- `@FeignClient`：**只取 `path` 属性**做前缀；`value` 是服务名，忽略；无 `path` 则空前缀
- `@RequestMapping` / `@Path`：`value` 优先，其次 `path`，都没有则空串（不加前缀）
- 多值（`@RequestMapping({"/a","/b"})`）产生多个组合，索引级笛卡尔积
- 方法侧：遍历全部 path 注解做同样处理；单个空值注解（`@PostMapping` 无参）被跳过
- JAX-RS 的 `@GET` + `@Path("/x")` 分属两个注解，靠"遍历全部 path 注解"自然合并
- `join_path` 归一化：两边 trim `/`、强制以 `/` 开头、反复折叠 `//` 直到消失、全空 → `"/"`

HTTP 方法（`verb_of`）：`XxxMapping` 的后缀（仅 `GET/POST/PUT/DELETE/PATCH/HEAD/OPTIONS` 白名单）
或注解名本身是 verb（JAX-RS `@GET`）；`@RequestMapping` 读 `method` 属性的最后一个标识符并大写，
无 `method` 属性时得到 `Some(None)`（= 任意方法）。

### 参数分类

| 注解（任一） | `ParamKind` | 进 `Endpoint`？ | 去向 |
| - | - | - | - |
| `@PathVariable` / `@PathParam` | `Path` | 是 | `path_params` |
| `@RequestParam` / `@QueryParam` | `Query` | 是 | `query_params` |
| `@RequestHeader` / `@HeaderParam` | `Header` | 是 | `header_params` |
| `@RequestBody` | `Body` | 是 | 只写 `body_type` |
| `@CookieValue` / `@CookieParam` | `Cookie` | **否** | 丢弃 |
| `@RequestPart` / `@FormParam` / `@ModelAttribute` | `Form` | **否** | 丢弃 |
| 无注解 | `Query` | 是 | `query_params`，`required = true` |

名字取注解 `value`，其次 `name`，都没有则形参名。`required` 取注解 `required`，缺省 `true`。

> **Warning**：Cookie 与 Form 参数被识别后**丢弃**（`scan::pick` 只挑 Path/Query/Header）。
> `api_doc` 的 `req_body_form` 恒为 `[]`。这是刻意的简化，不是遗漏；要补就一次改到位
> （`Endpoint` 模型 + `api_doc` + 页面 + PRD）。

### 位置与去重

- `raw.at` = 方法第一个注解的 token 下标；取 `lexed.toks[at].start`（字节）→ `LineIndex::of(byte)`
  → **零基行 + UTF-16 列**。与宿主 IPC 约定一致（`ide-core::text`：0-based 行、UTF-16 列）
- `Endpoint.line` / `col` 零基；`to_json` 输出 `"line": e.line + 1`（给人看）；
  `host::open_location(path, line, col)` 收零基，直接传原值
- 排序：`path` 升序，其次 `method` 升序
- 去重：`dedup_by(|a,b| a.key() == b.key() && a.class_name == b.class_name)`，
  `key()` = `"{METHOD} {path}"`（缺省 `GET`）——同控制器内同方法同路径只留一条

### DTO 传递闭包

起点：每个 endpoint 的 `body_type` 与 `return_type`。`push_names` 递归拆泛型，
只收**首字母大写**的简单类名（小写标量被排除）。

循环：`queue.pop()` → `tried` 去重 → 先查同批 `classes`，未命中再 `load_class` →
对每个字段的 `ty` 继续 `push_names` → 产出 `FieldSpec`。`TYPE_BUDGET` 递减，耗尽即停（防环形或超大模型）。

`load_class` 两段策略：先 `find_files("**/{name}.java", 3)`（含 `.kt`），
候选为空再 `search_text("(class|interface|record)\s+{name}\b", 8)`——有些项目类名与文件名不一致。

字段过滤（`keep_field`，命中任一即丢）：`serialVersionUID`、`this$` 开头、`ty` 为空、
全 `[A-Z0-9_]` 常量（长度 > 1，如 `MAX_SIZE`）、注解含 `JsonIgnore`/`JsonBackReference`/`JsonIgnoreProperties`。

字段 JSON 名（`wire_name`）：`@JsonProperty` / `@SerializedName` / `@JSONField` 的 `value` 或 `name`
中第一个非空串，否则用字段名。字段描述 = 文档注释里第一个非空且不以 `@` 开头的行。

### 仪表盘 JSON（`to_json`）

```json
{
  "count": 2, "files": 1, "truncated": false,
  "items": [{ "method": "POST", "path": "/api/users/create", "summary": "创建用户。",
              "handler": "UserController#create", "class": "UserController",
              "framework": "Spring", "file": "/proj/src/UserController.java",
              "line": 7, "params": ["q"], "body": "CreateUser" }]
}
```

`method` 缺省 `"ANY"`；`line` 是 1-based；`params` 只含 query 参数名；`body` 无请求体时为 `null`。
字段名是页面契约，改一个字母就断（见 `../view/message-protocol.md`）。

---

## 4. Validation & Error Matrix

| 条件 | 行为 |
| - | - |
| `search_text` 失败 | `scan` 返回 `Err`，向上抛 |
| 单文件 `read_file` 失败 | `continue`，不计入 `files`，不中断 |
| 类判定不出框架 | 整个类跳过 |
| 方法无 verb 注解 | 跳过该方法（类级 `@RequestMapping` 不算 verb） |
| 类型解析不到 | 该字段/类型退化成 `{"type":"object"}`，不报错 |
| `TYPE_BUDGET` 耗尽 | 停止解析剩余类型，已解析照常输出（**不**置 `truncated`） |

---

## 5. Good / Base / Bad Cases

**Good：多模块 Spring 项目**

`@RestController` + 类级 `@RequestMapping("/api/users")` + 方法级 `@GetMapping("/list")`
→ `/api/users/list`，`framework = "Spring"`，`handler = "UserController#list"`，
query 参数进 `params`，返回类型进 `return_type` 供 Schema 拆包。

**Base：`@RequestMapping` 无 method**

`method == None`。列表显示 `ANY`，导出时 `yapi_method()` 回落 `GET`，`key()` 同样用 `GET` 判定"是否已存在"。

**Bad：用 byte 偏移当列号**

含中文的行 byte 偏移大于 UTF-16 列，跳转位置偏右。`LineIndex::of` 用 `ch.len_utf16()` 累加才对。

---

## 6. Tests Required

| 用例 | 位置 | 断言点 |
| - | - | - |
| 注释里的注解不算 | `java.rs::tests::skips_comments_and_reads_strings` | 注释内的 `@GetMapping`/`@PostMapping` 不出现在标识符序列；`docs.len() == 1` |
| 括号与位置 | `java.rs::tests::brackets_and_positions` | 见该测试 |
| 文本块与转义 | `java.rs::tests::text_blocks_and_escapes` | 见该测试 |
| Spring 路由 | `spring.rs::tests::reads_spring_routes` | 路径、method、`framework == "Spring"` |
| JAX-RS | `spring.rs::tests::reads_jax_rs` | `framework == "JAX-RS"`，`@GET`+`@Path` 合并成一个路由 |
| Kotlin 控制器 | `spring.rs::tests::reads_kotlin_controllers` | `fun x(): T` 的返回类型被取到 |
| Kotlin data class | `spring.rs::tests::kotlin_data_class_and_properties` | 构造器属性进字段 |
| record | `spring.rs::tests::reads_record_components` | 组件成为字段 |
| 字段过滤 | `spring.rs::tests::reads_fields_and_skips_locals` | 方法内局部变量不算字段 |
| 端到端 | `lib.rs::tests::scans_the_project_into_endpoints` | `files == 1`、2 个接口、路径与 method、`summary == "列表。"`、`dto` 含 `UserDto`/`CreateUser`、`CreateUser[1].ty == "int"` |
| 轻量路由扫描 | `lib.rs::tests::the_route_scan_skips_dto_resolution` | `route_options()` 的 `dto.is_empty()`；`resolve_dto: true` 时非空 |

改解析规则后必须跑真实项目冒烟（唯一能发现"规则看起来对但实际错"的手段）：

```sh
cargo run --example scan_dir -- C:\path\to\project
```

---

## 7. Wrong vs Correct

### 框架判定按"有 Mapping 注解就行"

#### Wrong

```rust
if class.anns.iter().any(|a| a.name.ends_with("Mapping")) { return Some("Spring"); }
```

带 `@RequestMapping` 的 Feign 客户端被判成 Spring，随后 `class_prefix` 把服务名当路径。

#### Correct

```rust
if has(&["FeignClient"]) { return Some("Feign"); }
if has(&["RestController", "Controller"]) { return Some("Spring"); }
if class.methods.iter().any(|m| m.anns.iter().any(|a| verb_of(a).is_some())) {
    if has(&["Path"]) { return Some("JAX-RS"); }
    return Some("Spring");
}
None
```

### 解析失败就整体报错

#### Wrong

```rust
let fields = load_class(project, options, &name, &sources)
    .ok_or_else(|| Error::Failed(format!("无法解析 {name}")))?;
```

一个陌生 DTO 让整个项目扫描失败，用户看到"扫描失败"而不是"这个类型退化成 object"。

#### Correct

```rust
let Some(fields) = fields else { continue };
```

解析是尽力而为：解析不到的类型在 Schema 里自然变成 `{"type":"object"}`。
