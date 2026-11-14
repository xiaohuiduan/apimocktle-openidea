# 执行计划：响应体保留包装 + 泛型实参替换

依赖 [design.md](./design.md)。按顺序做，每步都留可编译、可测的状态。

## 步骤

1. **`src/spring.rs`：收集类型参数**
   - `Class` 增加 `pub type_params: Vec<String>`，`..Class::default()` 调用点自动兼容。
   - 在 `class` / `interface` / `record` 关键字与类体之间扫描 `<`，用 `matching(.., '<', '>')`
     取配对区间，按深度 0 的 `,` 切分，每段取首个标识符（Kotlin 上界 `: Any` 之后的丢弃）。
   - 构造器字段分支（line ~242）与类体分支（line ~259）两处都要带上 `type_params`。
   - 加单测：`class Result<T>`、`class Pair<K, V>`、`data class Page<T : Any>(val list: List<T>)`。

2. **`src/model.rs`：替换与第 6 步改写**
   - 新增 `ResolvedClass { params, fields }`，`Resolver` 返回类型改为 `Option<ResolvedClass>`。
   - 新增 `substitute(fields, params, args) -> Vec<FieldSpec>`：逐 token 边界替换，递归 children。
   - 第 6 步：先尝试 `resolve(&base)`，有非空字段则 `substitute` + `object_schema`；否则按原逻辑拆包。
   - 加单测：替换正例、`Test`/`Time` 负例、`args` 短于 `params` 的缺位保持、空字段类退回拆包。

3. **`src/scan.rs`：索引带 params**
   - 新增 `DtoIndex { fields, params }`；`ScanResult.dto` 类型改为 `DtoIndex`。
   - `classes` 缓存与 `load_class` 返回 `(Vec<String>, Vec<Field>)`，`resolve_dtos` 两份都写。
   - `resolve_dtos` 的替换由 `model.rs` 负责，这里只提供 params，不做类型变换。

4. **跟随签名（编译驱动）**
   - `src/yapi.rs::api_doc(endpoint, dto: &DtoIndex, depth)`，内部 resolve 闭包返回 `ResolvedClass`。
   - `src/lib.rs::start_export` / `export_single` 传 `&scanned.dto`。
   - `examples/schema_size.rs` / `examples/scan_dir.rs` 构造 `DtoIndex`。

5. **文档同步**
   - README「包装类型自动拆开（`Result<T>` / `ResponseEntity<T>` / `Optional<T>` …）」改为
     与新行为一致的表述；`plugin.toml` 的 `change-notes` 补一条。

## 验证命令（按序）

```sh
cargo test                                        # 基线 21 项，只增不减
cargo build --release --target wasm32-wasip2      # 组件仍可出
cargo run --example scan_dir -- <真实项目>         # 接口数与框架分布不退化
cargo run --example schema_size -- <真实项目>      # 1..4 层体积曲线不异常膨胀
pwsh script/package.ps1                           # 打包通过
```

## 必须新增的测试（断言点）

| 用例 | 断言 |
| - | - |
| 保留包装 + 数组实参 | `body_schema("Result<List<UserDto>>", 2, r)` → 顶层 `object`；`properties.data.type == "array"`；`properties.data.items.properties` 含 `name`/`age` |
| 保留包装 + 单值实参 | `body_schema("Result<UserDto>", 2, r)` → `properties.data.type == "object"` 且有字段 |
| 降级 | resolve 对 `Result` 返回 `None` → 顶层 `array`（与旧行为一致） |
| 类型参数多参数 | 自定义 `Pair<K,V>` → 两个字段分别是 K/V 的实参类型 |
| 替换边界 | 字段类型 `Test`、`Time`、`TaskDto` 在 params 含 `T` 时**不被**替换 |
| 端到端 | `src/lib.rs` 的 MockHost 用例中，`res_body` 解析后 `properties.data.type == "array"` |

## 风险文件与回滚点

- `src/model.rs` —— 公开签名变更，波及 `scan.rs` / `yapi.rs` / `examples`。回滚点：步骤 2 完成前。
- `src/spring.rs::parse` —— 类型参数提取插在类头与类体之间，最容易影响既有的
  record/构造器字段解析；改完必须跑 `spring.rs` 全部既有测试。
- `ScanResult.dto` 类型变化 —— 编译器会指路，但要确认 `lib.rs` 的日志行
  （`result.dto.len()`）改成 `result.dto.fields.len()` 后语义不变。

## task 启动前检查

- [ ] `prd.md` 的 Open Questions 已清空（两条均已回答：选 C；包装类在源码里）
- [ ] `design.md`、`implement.md` 已评审
- [ ] `implement.jsonl` / `check.jsonl` 填入真实条目：
      `.trellis/spec/plugin/schema-contract.md`、`.trellis/spec/plugin/scanner-contract.md`、
      `.trellis/spec/plugin/yapi-contract.md`
