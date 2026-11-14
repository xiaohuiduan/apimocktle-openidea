---
name: schema-contract
description: 类型拆分与 JSON Schema 生成规则（包装类型拆包、容器、标量映射、深度）
paths:
  - src/model.rs
---

# 类型与 JSON Schema 契约

`src/model.rs` 决定 YApi 里请求体/响应体长什么样。它只做两件事：拆类型字符串，把类型翻译成 JSON Schema。
输入是文本级的类名（来自 `spring.rs`），所以这里的一切都必须对"不完整的信息"容忍。

---

## 1. Scope / Trigger

- 改 `split_type` / `simple_name` / `type_schema` / `body_schema`
- 改标量映射表、`WRAPPERS` 列表、容器判定集合
- 改 `depth` 的语义或递归衰减方式

---

## 2. Signatures

```rust
pub fn split_type(ty: &str) -> (String, Vec<String>);
pub fn simple_name(ty: &str) -> String;
pub struct ResolvedClass { pub params: Vec<String>, pub fields: Vec<FieldSpec> }
pub type Resolver<'a> = dyn Fn(&str, u32) -> Option<ResolvedClass> + 'a;
pub fn type_schema(ty: &str, depth: u32, resolve: &Resolver<'_>) -> Value;
pub fn body_schema(ty: &str, depth: u32, resolve: &Resolver<'_>) -> String;  // 返回 JSON 文本，不是 Value
```

`Resolver` 返回 `ResolvedClass` 而不只是字段，是因为泛型实参替换必须同时看到
**类声明的参数名**（`params`）和字段列表（`fields`）。返回 `None` 的含义是"这个类型我不知道"——
不是错误，调用方必须把它当普通对象处理。

---

## 3. Contracts

### `split_type`：一层泛型拆解

```rust
split_type("Result<List<User>>")  == ("Result", ["List<User>"])
split_type("Map<String, List<Long>>") == ("Map", ["String", "List<Long>"])
split_type("int")                 == ("int", [])
split_type("User[]")              == ("User", [])      // 尾部 [] 被剥掉
simple_name("com.acme.UserDto")   == "UserDto"
```

实现要点，改的时候别破坏：

- 先剥 `? extends ` 前缀（通配符上界），再按第一个 `<` 切基名
- 只认"以 `<` 开始且以 `>` 结尾"的组合；`Result<` 这种残缺串原样返回、无参数
- 切参数时用深度计数器，只在**深度 0** 的 `,` 处分段（否则 `Map<String, List<Long>>` 会被切错）
- 尾段非空也要收（`List<>` 得到空段被丢弃，`List<A,>` 得到 `["A"]`）

### `type_schema`：判定顺序即语义

严格按此顺序，重排会改变输出：

1. 空字符串 → `{"type":"string"}`
2. 以 `[]` 结尾 → `{"type":"array","items": type_schema(剥掉[]的, depth, resolve)}`
3. object-like（`Map` `HashMap` `LinkedHashMap` `TreeMap` `JSONObject` `JsonObject`）→
   `{"type":"object","additionalProperties": 第二个类型参数或 {"type":"string"}}`
4. array-like（`List` `Set` `Collection` `Iterable` `ArrayList` `LinkedList` `Queue` `Deque` `Stream`）→
   `{"type":"array","items": 第一个类型参数或 {"type":"string"}}`
5. 标量映射命中 → 直接返回标量 Schema
6. 基名在 `WRAPPERS` 里时：
   - `depth > 0` 且 `resolve(基名)` 命中非空字段 → **保留包装**：`substitute(字段, params, args)` 后
     `object_schema` 展开
   - 否则 → **拆包**：递归第一个类型参数（`args.first()`）
7. `depth == 0` → `{"type":"object"}`
8. `resolve(基名)` 命中且字段非空 → `substitute` 后 `object_schema(fields, depth, resolve)`；否则 `{"type":"object"}`

### 包装：能展开就保留，否则拆

第 6 步的取舍决定导出文档的形状。规则是"**包装类能在项目源码里解析到字段就保留它**"，
而不是维护"业务包装 vs 框架包装"名单：

- `Result<T>` 在项目源码里 → 保留 → `res_body` 顶层是 object，字段表能读出 `code`/`msg`/`data`
- `ResponseEntity<T>` / `Optional<T>` 在依赖里 → 解析不到 → 拆包 → 行为与旧版完全一致

为什么靠"能否解析"而不是名单：分类名单永远不全（`R`、`Res`、`CommonResult`、公司自定义名），
而"能否解析"是客观事实，用户换命名习惯时行为仍然正确。

> **Gotcha**：不保留包装时 `Result<List<T>>` 会变成顶层数组 `{"type":"array","items":…}`。
> 服务器端的参数结构表只读顶层 `properties`，此时显示 `0 fields`（示例仍能生成，因为走另一条递归路径）。
> 这就是 v0.2.x 之前"响应体字段表为空"的根因。

> **Gotcha**：`depth > 0` 守卫不能删。`object_schema` 内部用 `saturating_sub(1)` 递减，
> depth 归零后不再变小；没有守卫时自引用包装（`class Result<T> { Result<T> next; }`）
> 会无限重入 `object_schema`，在 WASM 里就是栈溢出 trap。

### 泛型实参替换（`substitute`）

```rust
fn substitute(fields: &[FieldSpec], params: &[String], args: &[String]) -> Vec<FieldSpec>;
fn substitute_type(ty: &str, params: &[String], args: &[String]) -> String;
```

- 按标识符字符（`[A-Za-z0-9_$]`）聚词，**整词**匹配参数名：`T` 替换，`Test`/`Time`/`TaskDto` 不动
- 标点逐字符原样拷回：`Map<String, T>` → `Map<String, List<X>>`
- `args` 短于 `params` 时，缺位保持参数名原样（不猜）
- 递归处理 `children`
- 无实参时是恒等变换——这是"非泛型类的输出逐字节不变"的依据

参数名的来源在 `spring.rs`：`class Result<T, E>` → `["T","E"]`，
`class ApiResult<out T : Any>` / `class Wrapped<@Ann T>` / Java `T extends Comparable<T>` 都只取 `T`。
取词规则是"以 `:` 或 `extends` 为界取它前面的标识符；无绑定时取段内最后一个标识符"，
**不能取段内第一个**——否则 variance 修饰符或注解会被当成参数名，替换失效，`data` 退化成空对象。

标量表：

| Schema 类型 | Java/Kotlin 类型（原样匹配简单名） |
| - | - |
| string | `String` `CharSequence` `StringBuilder` `char` `Character` `UUID` `LocalDate` `LocalDateTime` `Date` `Instant` `BigDecimal` `BigInteger` `Object` |
| number | `byte` `short` `int` `long` `float` `double` `Byte` `Short` `Integer` `Long` `Float` `Double` |
| boolean | `boolean` `Boolean` `AtomicBoolean` |
| 不命中（返回 `None`） | `void` `Void`，以及一切其它名字 |

`WRAPPERS` 全文（21 项，顺序无关）：

```
Result R Res Resp Response ResponseEntity ApiResponse ApiResult CommonResult BaseResponse
Optional List Set Collection Iterable ArrayList LinkedList Page PageResult PageInfo IPage
```

> **Gotcha**：`Result` 只出现在 `WRAPPERS`，不在容器集合里。
> 所以 `Result<List<UserDto>>` 必须先在**第 6 步**拆成 `List<UserDto>`，
> 再在递归时由**第 4 步**变成 array。两个集合分工不同，合并会得到错误结果。

> **Gotcha**：`void` 不是"没有 Schema"。它落到第 7/8 步，最终是 `{"type":"object"}`。
> 若产品上希望 `void` 返回空 Schema 或 null，必须在前置分支显式处理并补测试。

### `object_schema`：逐字段递归

```rust
let schema = if field.children.is_empty() {
    type_schema(&field.ty, depth.saturating_sub(1), resolve)
} else {
    object_schema(&field.children, depth.saturating_sub(1), resolve)
};
if !field.description.is_empty() { obj.insert("description", ...); }
```

- 每下一层 `depth` 减 1，`saturating_sub` 保证不会下溢
- `children` 非空时优先用已解析的子字段，不再按 `ty` 字符串重新解析
- 字段描述写进**该字段自己的 Schema**，不写进父对象的 description

### `body_schema`

只是 `type_schema(..).to_string()`。**返回 String** 是硬要求：YApi 的 `req_body_other`/`res_body`
是字符串字段，配 `*_is_json_schema: true`。传 `Value` 会让服务器把 Schema 当普通字面量存。

---

## 4. Validation & Error Matrix

| 输入 | 输出 |
| - | - |
| `""` | `{"type":"string"}` |
| `"User[]"` | `{"type":"array","items": <User 的解析结果>}` |
| `"Map<String, Long>"` | `{"type":"object","additionalProperties":{"type":"number"}}` |
| `"Map"`（无参数） | `{"type":"object","additionalProperties":{"type":"string"}}` |
| `"List"`（无参数） | `{"type":"array","items":{"type":"string"}}` |
| `"Result<UserDto>"` | `<UserDto 的对象 Schema>` |
| `"UserDto"` + 深度 0 | `{"type":"object"}` |
| `"UserDto"` + resolve 返回 `None` | `{"type":"object"}` |
| `"void"` | `{"type":"object"}` |
| `"? extends Base<T>"` | 按 `Base<T>` 处理 |

没有任何输入会返回 `Err`：Schema 生成是纯函数，永不失败。

---

## 5. Good / Base / Bad Cases

**Good：`Result<List<UserDto>>`**

`Result` → 拆包 → `List<UserDto>` → array → `items` 递归 `UserDto` → object，字段带类型与描述。

**Base：`Map<String, JsonNode>`**

`{"type":"object","additionalProperties":{"type":"object"}}`（`JsonNode` 解析不到，退化成对象）。

**Bad：把 `List` 也放进 `WRAPPERS` 的判定里**

`List` 同时是容器与包装。若让第 6 步先命中，`List<UserDto>` 会被拆成 `UserDto`（丢掉数组语义）。
容器判定（第 4 步）必须在包装拆解（第 6 步）之前。

---

## 6. Tests Required

| 用例 | 位置 | 断言点 |
| - | - | - |
| 泛型拆分 | `src/model.rs::tests::splits_generics` | `split_type("Result<List<User>>").0 == "Result"`、`.1 == ["List<User>"]`；`split_type("Map<String, List<Long>>").1 == ["String","List<Long>"]`；`split_type("int").1.len() == 0`；`simple_name("com.acme.UserDto") == "UserDto"` |
| 降级：解析不到 | `src/model.rs::tests::an_unresolvable_wrapper_is_unwrapped` | `Result<List<UserDto>>` 与 `Optional<UserDto>` 解析不到时顶层 `array`；空字段包装 `ResolvedClass::default()` 同样拆包 |
| 保留 + 数组实参 | `keeps_a_resolved_wrapper_and_substitutes_a_list_argument` | 顶层 `object`；`properties.data.type == "array"`；`items.properties` 含 `name`/`age`/`active` |
| 保留 + 单值实参 | `keeps_a_resolved_wrapper_and_substitutes_a_single_argument` | `properties.data.type == "object"` 且有 `UserDto` 字段 |
| 自定义泛型类 | `a_custom_generic_class_substitutes_its_argument` | 非 `WRAPPERS` 的 `Envelope<UserDto>` → `properties.value.properties.name.type == "string"` |
| 深度耗尽 | `a_spent_depth_budget_unwraps_the_wrapper` | `depth == 0` 时自引用包装仍走拆包（顶层 array），证明守卫必要 |
| 替换边界 | `substitute_only_replaces_whole_identifiers` | `Test`/`Time`/`TaskDto` 不动；`T`→`List<UserDto>`；`Map<String,T>` 标点保留；`children` 递归 |
| 缺位实参 | `missing_arguments_leave_parameters_alone` | `params=[K,V]`、`args=[String]` 时 `K` 替换、`V` 原样 |
| Map 与 `[]` | `src/model.rs::tests::schema_handles_maps_and_arrays` | `Map<String,Long> → additionalProperties.type == "number"`；`String[] → items.type == "string"` |
| 落到文档 | `src/yapi.rs::tests::document_carries_schemas_and_sources` | `req_body_other` 解析后字段描述为"名字"；`res_body.properties.id.type == "number"` |

新增映射时在 `model.rs` 测试模块里加用例，并确认 `cargo run --example schema_size -- <项目>`
的深度曲线没有异常膨胀（该示例就是为观察 1–4 层的 Schema 体积写的）。

---

## 7. Wrong vs Correct

### 返回 `Value` 而不是 String

#### Wrong

```rust
pub fn body_schema(...) -> Value { type_schema(...) }
```

写进 YApi 文档时被序列化成嵌套对象，服务器当普通字段存，接口详情页显示成一坨 JSON 字面量。

#### Correct

```rust
pub fn body_schema(ty: &str, depth: u32, resolve: &Resolver<'_>) -> String {
    type_schema(ty, depth, resolve).to_string()
}
```

### 让容器判定晚于包装拆解

#### Wrong

```rust
if let Some(first) = args.first() && WRAPPERS.contains(&base.as_str()) {
    return type_schema(first, depth, resolve);   // List<UserDto> 丢掉数组
}
if is_array_like(&base) { /* 永远走不到 */ }
```

#### Correct

```rust
if is_array_like(&base) { return json!({"type":"array","items": ...}); }   // 先容器
if let Some(first) = args.first() && WRAPPERS.contains(&base.as_str()) {
    return type_schema(first, depth, resolve);                            // 后拆包
}
```
