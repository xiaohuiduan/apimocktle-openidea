# 导出接口响应体字段表为空（0 fields）

## Goal

导出的接口在 ApiMocktle 里"返回响应 › 参数结构"显示 `0 fields`（如
`/api/user-service/inner/passenger/actual/query/ids`），但同区域的"示例"能正常展开出字段。
让字段表能显示响应体结构，同时保持 mock 的 JSON 形状与真实接口一致。

## 背景与已确认事实

以下每条都已在仓库内验证，可直接复现：

- `src/yapi.rs::api_doc` 把 `res_body` 写成 `body_schema(&endpoint.return_type, depth, resolve)` 的
  **JSON 文本**，并声明 `res_body_is_json_schema: true`、`res_body_type: "json"`。
- `src/model.rs::type_schema` 第 4 步把数组类容器（`List`/`Set`/`Collection`/`ArrayList`…）
  渲染成 `{"type":"array","items":…}`；第 6 步的 `WRAPPERS`（21 项）会拆掉 `Result` 等包装。
  两者叠加，`Result<List<PassengerDto>>` 的 `res_body` 顶层是 **array**：
  `{"type":"array","items":{"type":"object","properties":{…8 个字段…}}}`。
- 行为已被测试固定：`src/model.rs::tests::schema_unwraps_result_and_lists` 断言
  `body_schema("Result<List<UserDto>>")` 顶层 `type == "array"`、字段在 `items.properties` 下。
- 用户截图（返回响应区）显示 `参数结构 0 fields` / `暂无字段定义`，而"示例"渲染出
  `[ { createDate, discountType, id, idCard, idType, phone, realName, username, verifyStatus } ]`。
  示例最外层是 `[`…`]`，与"顶层 array"判断一致；示例生成器递归 `items`，字段表只读顶层
  `properties`，于是出现"表空、示例有值"的分裂。
- 上游参考实现从不产生顶层数组：其 YApi 导出快照里 `res_body` 顶层只有
  `{"type":"object",…}`（4 处）与 `{"type":"string",…}`（3 处）。原因在
  `refs/ApiMocktle-java-plugin/.../springmvc/ReturnTypeUnwrapper.kt`：只拆
  `ResponseEntity` / `Mono` / `Optional` / `Flux`（按全限定名精确匹配），
  **不拆 `Result` 这类业务包装，也不拆 `List`**。

## 根因

本插件同时拆"业务包装"和"容器"：`Result<List<T>>` 先拆 `Result` 得 `List<T>`，
再拆成顶层数组。ApiMocktle 的参数结构表只读顶层 `properties`，顶层是 array 就没有可读字段，
于是 `0 fields`；示例值不受影响，因为它走另一条递归路径。

数据没丢，字段全在 `items.properties` 里。

## 目标方案（已选定：C — 保留包装）

`Result<List<PassengerDto>>` 期望导出为：

```json
{"type":"object","properties":{
  "code":{"type":"number"},
  "msg":{"type":"string"},
  "data":{"type":"array","items":{"type":"object","properties":{…8 个字段…}}}
}}
```

即：不再拆业务包装，让它以对象身份携带 `data`；`data` 是数组，mock 形状与真实接口一致。

### 关键技术约束（已验证）

本插件是文本级解析，**没有泛型实参替换能力**：

- `src/spring.rs` 的 `Class` 结构只有 `name` / `anns` / `methods` / `fields` / `doc`，
  **没有类型参数列表**；解析 `class Result<T>` 时 `<T>` 被跳过。
- 因此 `private T data;` 解析出的 `FieldSpec.ty` 是字面量 `"T"`。
  `type_schema("T", …)` 既不是标量也不在容器/包装名单里，`resolve("T")` 必然失败，
  最终得到 `{"type":"object"}`。

所以"保留包装"要真正得到 `data: List<PassengerDto>`，**必须新增泛型参数收集与实参替换**，
否则 `data` 会退化成空对象，比现状更差（现状至少有字段信息）。

### 依赖可行性（已确认）

包装类（`Result` 等）在用户项目源码里有定义，因此 C 的完整实现可行：能解析到
`code` / `msg` / `data` 字段，`data` 经泛型替换得到 `array<PassengerDto>`。
若换成解析不到的情形（依赖 jar），实现会退回现状的拆包行为，见 Acceptance Criteria 的降级项。

## Requirements

- 顶层为数组的响应体，在 ApiMocktle 里能看到字段结构，而不是 `0 fields`。
- 不破坏容器语义：数组不能被伪装成单个对象。
- 单值包装（`ResponseEntity<T>` / `Optional<T>`）的拆解行为保持可用。
- 泛型实参要能替换：`Result<List<X>>` 的 `data` 必须是 `array<X>`，不能是空对象。
- 解析不到包装类定义时**必须降级**到当前行为，不得让 `res_body` 变成空对象。

## Acceptance Criteria

- [ ] `Result<List<X>>` 的 `res_body` 顶层是 object，`properties.data.type == "array"`，
      `properties.data.items.properties` 含 X 的字段（新增单测断言，含至少 3 个字段名）
- [ ] `Result<X>`（单值）的 `res_body` 顶层是 object，`properties.data.type == "object"` 且有 X 的字段
- [ ] 包装类解析不到时，`res_body` 与改动前一致（顶层 array），并有单测固定该降级
- [ ] `ResponseEntity<List<X>>` 行为明确：拆框架包装后仍为 array（或按新规则保留），并有测试固定
- [ ] `cargo test` 全绿（现基线 21 项，只增不减）
- [ ] `cargo run --example scan_dir -- <真实项目>` 不 panic，接口数与框架分布不退化
- [ ] README「包装类型自动拆开」的表述与实际行为一致，不一致则同步修改

## Out of Scope

- 修改 ApiMocktle 服务器/前端的字段表渲染逻辑（不在本仓库）。
- 为 JSON Schema 增加 `$schema` draft 声明（上游有、本项目没有，另议）。
- 请求体 `req_body_other` 的同类问题按同一规则顺带覆盖，不单列范围。

## Open Questions

（无。顶层数组表达方式已定为 C；包装类在项目源码里可解析，泛型替换具备落地前提。）
