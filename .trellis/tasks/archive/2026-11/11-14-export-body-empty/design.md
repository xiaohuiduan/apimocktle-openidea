# 设计：保留响应包装类型并正确替换泛型实参

## 1. 问题与目标形状

现状：`Result<List<PassengerDto>>` 被连业务包装（`Result`）带容器（`List`）一起拆掉，
`res_body` 顶层成了 `{"type":"array","items":…}`。ApiMocktle 的参数结构表只读顶层
`properties`，于是 `0 fields`。数据没丢，只是形状不对。

目标：

```json
{"type":"object","properties":{
  "code":{"type":"number"},
  "msg":{"type":"string"},
  "data":{"type":"array","items":{"type":"object","properties":{…}}}
}}
```

## 2. 核心障碍

文本级解析没有泛型实参替换。已核实：

- `src/spring.rs::Class` 只有 `name` / `anns` / `methods` / `fields` / `doc`，无类型参数；
  解析 `class Result<T>` 时 `<T>` 被跳过。
- `parse_field` 用 `type_before(...)` 取类型文本，`private T data;` 得到 `ty = "T"`。
- `type_schema("T", …)` 命中不了标量/容器/包装，`resolve("T")` 必然失败 → `{"type":"object"}`。

所以只"不拆 Result"是不够的：不补泛型替换，`data` 会退化成空对象，比现状更差。

## 3. 设计

### 3.1 三个数据结构的变更

```rust
// spring.rs —— 记录类的类型参数名，供替换用
pub struct Class {
    pub name: String,
    pub type_params: Vec<String>,   // 新增：`class Result<T, E>` → ["T", "E"]
    pub anns: Vec<Ann>,
    pub methods: Vec<Method>,
    pub fields: Vec<Field>,
    pub doc: String,
}

// model.rs —— 解析结果的统一载体，替换需要 params 与 fields 同时可见
pub struct ResolvedClass {
    pub params: Vec<String>,
    pub fields: Vec<FieldSpec>,
}
pub type Resolver<'a> = dyn Fn(&str, u32) -> Option<ResolvedClass> + 'a;

// scan.rs —— 索引打包，避免给每个调用点加第二个 map 参数
pub struct DtoIndex {
    pub fields: HashMap<String, Vec<FieldSpec>>,
    pub params: HashMap<String, Vec<String>>,
}
pub struct ScanResult {
    pub endpoints: Vec<Endpoint>,
    pub dto: DtoIndex,              // 原为 HashMap<String, Vec<FieldSpec>>
    pub files: usize,
    pub truncated: bool,
}
```

### 3.2 泛型实参替换

新增纯函数（`model.rs`）：

```rust
/// 把字段类型里的类型参数名替换成实参。`T` → `List<X>`，`Map<String,T>` → `Map<String,List<X>>`。
fn substitute(fields: &[FieldSpec], params: &[String], args: &[String]) -> Vec<FieldSpec>;
```

实现要点：

- 按**标识符边界**匹配，只替换完整的类型名 token：`T` 命中，`Test`、`Time`、`TaskDto` 不命中。
- 替换是逐 token 拼回，`List<T>` 里的 `T` 会被替换，`,`、`<`、`>` 原样保留。
- `args` 比 `params` 短时，缺位的参数保持原样（不猜）。
- 递归处理 `children`。
- 单字母泛型名（`T`/`E`/`R`/`K`/`V`）与多字母（`ID`、`DTO`）一视同仁，只认 `params` 里列出的名字。

### 3.3 `type_schema` 的判定顺序调整

第 6 步（WRAPPERS 拆包）改为"能展开就保留，否则拆"：

```rust
// 原第 6 步
if let Some(first) = args.first() && WRAPPERS.contains(&base.as_str()) {
    return type_schema(first, depth, resolve);
}
```

改为：

```rust
if WRAPPERS.contains(&base.as_str()) {
    // 包装类在项目源码里能解析到字段时保留它，让 res_body 顶层是 object，
    // 字段表才有 properties 可读；data 通过实参替换得到真实类型。
    if let Some(rc) = resolve(&base, depth).filter(|rc| !rc.fields.is_empty()) {
        let mapped = substitute(&rc.fields, &rc.params, &args);
        return object_schema(&mapped, depth, resolve);
    }
    // 解析不到（框架类型在依赖里）就退回拆包，保持现有行为。
    if let Some(first) = args.first() {
        return type_schema(first, depth, resolve);
    }
}
```

**为什么不按"业务包装 vs 框架包装"分类**：分类需要维护名单，且名单永远不全
（`R`、`Res`、`ApiResult`、公司自定义名称…）。"能否解析到"是客观事实，
天然把 `ResponseEntity`/`Optional`（依赖里，解析不到 → 拆）与 `Result`（源码里 → 保留）分开，
且用户项目换命名习惯时行为仍正确。

第 4 步（array-like 容器）不动：`List<T>` 永远渲染成 array，这是容器的本质。

第 7/8 步保持：`resolve` 失败仍是 `{"type":"object"}`。

### 3.4 `spring.rs` 的类型参数提取

在解析到 `class`/`interface`/`record` 关键字后、类体之前，若遇到 `<`：

- 用现有的 `matching(src, lexed, k, '<', '>')` 找到配对；
- 按顶层 `,` 切分（与 `split_type` 同样的深度计数），每段取第一个标识符作为类型参数名；
- 空段丢弃；整体为空则不记录。
- Kotlin 的 `class Result<T : Any>` 只取 `T`（`:` 之后的部分丢弃）。

`parse_body` 与构造器字段路径都复用同一个 `Class`，字段逻辑不变。

### 3.5 `scan.rs` 的索引构建

`resolve_dtos` 在 `dto.insert(name, specs)` 的同时写入 `params.insert(name, class.type_params)`。
`load_class` 的返回值从 `Vec<FieldSpec>` 改为 `(Vec<String>, Vec<FieldSpec>)`，
缓存分支（`classes.get(&name)`）同样要带上 `type_params`，所以 `classes` 的 value 从
`Vec<spring::Field>` 变为 `Arc<(Vec<String>, Vec<spring::Field>)>` 或等价结构。

### 3.6 调用点

- `src/yapi.rs::api_doc(endpoint, dto: &DtoIndex, depth)`：resolve 闭包返回 `ResolvedClass`。
- `src/lib.rs::start_export` 的 `json!` 构造不变（仍调 `api_doc`）。
- `examples/schema_size.rs`、`examples/scan_dir.rs` 跟随类型变更调整。

## 4. 兼容与迁移

**用户可见行为变化**（必须写进 change-notes）：

- `Result<UserDto>`（单值）也不再拆开：响应形状从 `UserDto` 变成 `{code,msg,data:{…}}`。
- `ResponseEntity<T>`、`Optional<T>` 不变（依赖里的类型，解析不到 → 仍拆）。
- 已导出接口的 `res_body` 会在下次同步时被覆盖 `res_body` 字段（`save_api` 带 `id` 走更新）。
  这是期望结果，但用户会看到文档变化。

**降级保证**：包装类解析不到 → 与改动前完全一致（顶层 array）。这条有专门单测固定。

## 5. 风险与回滚

| 风险 | 处理 |
| - | - |
| 泛型替换把 `Test` 里的 `T` 改坏 | 逐 token 边界匹配 + 专门的负例测试（`Test` / `Time` / `TaskDto` 不被替换） |
| 类名与类型参数同名（`class T`） | 替换只作用于声明过的参数名，且先查 `params` 是否包含该名字 |
| 包装类字段为空（`class Result<T> {}`） | `filter(|rc| !rc.fields.is_empty())` 使其退回拆包 |
| `ScanResult.dto` 类型变更波及面 | 编译期强制暴露，逐个改；`DtoIndex` 只增字段不改语义 |
| 真实项目上行为漂移 | `cargo run --example scan_dir` 对比接口数与框架分布，`cargo run --example schema_size` 看 Schema 体积曲线 |

回滚：改动集中在 `spring.rs` / `model.rs` / `scan.rs` / `yapi.rs` 的纯函数与结构体，
无存储格式变化（`export:queue` 里存的是已算好的 doc），回滚即还原代码。

## 6. 落地顺序

1. `spring.rs`：`Class.type_params` + 提取 + 单测（`class Result<T>` / 多参数 / Kotlin 上界）
2. `model.rs`：`ResolvedClass`、`substitute`、第 6 步改写 + 单测（替换、边界、降级）
3. `scan.rs`：`DtoIndex`、`resolve_dtos` / `load_class` 带 params
4. `yapi.rs` + `lib.rs` + `examples`：跟随签名
5. README「包装类型自动拆开」表述同步
