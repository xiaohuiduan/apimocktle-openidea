---
name: yapi-contract
description: YApi/ApiMocktle 的 5 个端点、响应形状、接口文档字段与分类自愈
paths:
  - src/yapi.rs
---

# YApi / ApiMocktle 协议契约

`src/yapi.rs` 是对外唯一的网络出口。它说 YApi 的一个子集，兼容 ApiMocktle。
改这里等于改与服务器的契约，必须先确认服务器行为，再改代码与测试。

---

## 1. Scope / Trigger

- 新增/修改 `/api/...` 端点、查询参数、请求体字段
- 改 `check()` 的成功判定、HTTP 状态映射、错误文案
- 改 `api_doc()` 输出的字段集合（服务器按字段存库，删字段等于丢数据）
- 改 `api_doc` 的 Schema 生成（与 `model.rs` 联动，见 [scanner-contract.md](./scanner-contract.md)）

---

## 2. Signatures

```rust
pub struct Client { pub base: String, pub token: String, pub project_id: String }

impl Client {
    pub fn list_projects(&self) -> Result<Vec<ProjectInfo>, Error>;
    pub fn list_categories(&self) -> Result<Vec<Category>, Error>;
    pub fn create_category(&self, name: &str, desc: &str) -> Result<Category, Error>;
    pub fn find_or_create_category(&self, name: &str, desc: &str) -> Result<Category, Error>;
    pub fn list_apis(&self, cat_id: &str) -> Result<Vec<ApiBrief>, Error>;
    pub fn save_api(&self, doc: &Value, cat_id: &str, existing_id: Option<&str>) -> Result<(), Error>;
}

pub fn api_doc(endpoint: &Endpoint, dto: &DtoIndex, depth: u32) -> Value;
```

`dto` 是 `scan::DtoIndex`（字段 + 类型参数名），传入后 `api_doc` 用它组装 `ResolvedClass` 交给
`body_schema`。签名换回 `HashMap<String, Vec<FieldSpec>>` 会让泛型实参替换失效。

内部：`fn call(&self, method: &str, path: &str, query: &str, body: Option<Value>) -> Result<Value, Error>`
是所有请求的唯一通道，负责 URL 拼接、令牌校验、超时、状态码与 `errcode` 判定。

---

## 3. Contracts

### 端点清单（5 个，全部字节级确认）

| 用途 | 方法 | 路径 | 查询 | 请求体 |
| - | - | - | - | - |
| 项目列表 | GET | `/api/project/list` | `token=` | 无 |
| 分类菜单 | GET | `/api/interface/getCatMenu` | `project_id=&token=` | 无 |
| 建分类 | POST | `/api/interface/add_cat` | 无 | `{desc, project_id, name, token}` |
| 分类下接口列表 | GET | `/api/interface/list_cat` | `token=&project_id=&catid=&limit=5000` | 无 |
| 保存接口 | POST | `/api/interface/save` | 无 | `doc` + `{token, project_id, catid, id?}` |

所有查询字符串的值都过 `enc()`：**自实现的 RFC 3986 百分号编码**，
保留 `A-Za-z0-9-_.~`，空格编成 `%20`（不是 `+`），其余字节编成 `%XX` 大写。

```rust
assert_eq!(enc("a b/c"), "a%20b%2Fc");
assert_eq!(enc("中文"), "%E4%B8%AD%E6%96%87");
```

不要换成 URL crate：这里只需最小实现，且必须保证 `+` 不被当空格、`/` 被编码。

### 响应形状

```json
// 项目列表  → data[] 里每项：{_id, name, desc}
// 分类菜单  → data[] 里每项：{_id, name}
// 建分类    → data：{_id, name}
// 接口列表  → data.list[] 里每项：{_id, path, method}
// 保存接口  → 只需 errcode
```

`_id` 可能是字符串也可能是数字——解析统一走
`item.get("_id").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))`，
两种形态都要能拿成 `String`。

### 成功判定（`check`）

```rust
let code = payload.get("errcode").and_then(Value::as_i64)
    .or_else(|| payload.get("code").and_then(Value::as_i64));
let message = payload.get("errmsg").and_then(Value::as_str)
    .or_else(|| payload.get("message").and_then(Value::as_str)).unwrap_or("");
let ok = matches!(code, Some(0) | Some(200)) || matches!(message, "成功" | "成功！");
```

四个兼容点，缺一个就会在某种服务器版本上误报失败：
`errcode` 或 `code`；数字 0 或 200；`errmsg` 或 `message`；消息为"成功"或"成功！"。

失败文案：`服务器返回错误：{message || "未知错误"}`。

### `api_doc` 输出字段（服务器按此存库）

```json
{
  "title": "接口摘要（无 doc 时回落成方法名）",
  "path": "/api/users/create",
  "method": "POST",
  "desc": "方法/类的文档注释正文",
  "markdown": "## {summary}\n\n{description}\n\n来源：`{Class#method}`（{framework} {file}）",
  "status": "done",
  "req_body_is_json_schema": true,
  "res_body_is_json_schema": true,
  "req_headers": [{ "name": "X-Token", "value": "", "example": "", "desc": "", "required": true }],
  "req_query":   [{ "name": "verbose", "value": "true", "example": "true", "desc": "", "required": false }],
  "req_params":  [{ "name": "id", "example": "1", "desc": "" }],
  "req_body_form": [],
  "req_body_other": "{...JSON Schema 字符串...}",
  "req_body_type": "json",
  "res_body": "{...JSON Schema 字符串...}",
  "res_body_type": "json",
  "tags": ["UserController"],
  "tag": ["UserController"]
}
```

`save_api` 在此之上追加 `token` / `project_id` / `catid`，有既有接口时再追加 `id`。

要点：
- `method` 取 `endpoint.yapi_method()`，**永不为空**，缺省 `"GET"`（YApi 不接受空 method）。
- `req_body_other` / `res_body` 是**字符串**（JSON 序列化后的 Schema），不是对象。`*_is_json_schema: true` 与之配套。
- `req_params` 只有 `name`/`example`/`desc` 三个键（没有 `value`/`required`），与 `req_query` 不同。
- `req_query` 的 `value` 与 `example` 都取 `example_for(ty)`：数字→`"1"`，浮点→`"1.0"`，布尔→`"true"`，其余空串。
- `req_headers` 的 `value`/`example` 恒空串（不知道值，只有名字与必填性）。
- 无请求体时 `req_body_other` 为 `{"type":"object","properties":{}}` 的字符串；无返回类型时 `res_body` 同形。
- `tags` 与 `tag` 都写，值是控制器简单类名——分类与标签同名，这是刻意的约定（分类固定按控制器类名）。

### 分类策略

分类名 = `endpoint.class_name`（如 `TicketOrderController`）。描述固定：

```rust
const CATEGORY_DESC: &str = "由 OpenIDEA ApiMocktle 插件维护";
```

`find_or_create_category` 先 `list_categories()` 找同名同 id 的项；**列表请求失败也照样尝试创建**
（短暂不可达不该直接放弃）。`create_category` 后 `data._id` 为空 → `Failed("创建分类「{name}」后服务器没有返回 id")`。

### 自愈路径（`resolve_category` / `rebuild_category`）

```
缓存命中(id 非空) ─────────────────────────► 用缓存 id
        │未命中
        ▼
find_or_create_category ──失败/空 id──► None（该项计入 failed）
        │成功
        ▼
    用新 id，写回缓存
```

`save_api` 失败后：

```
rebuild_category(opts) （先从缓存删除，再 create_category）
   ├─ 拿到 fresh 且 fresh != 旧 id ──► save_api(doc, fresh, None) 再试一次（强制新建）
   ├─ fresh == 旧 id ─────────────► 记 failed（第一次的错误文本）
   └─ 重建失败 ───────────────────► 记 failed（第一次的错误文本）
```

**只重试一次**。循环重试会在服务器持续报错时把 5 秒预算烧光。

---

## 4. Validation & Error Matrix

| 条件 | 返回 |
| - | - |
| `base` 空（`trimmed()` 之后） | `Error::Invalid("先在设置里填写 ApiMocktle 服务器地址")` |
| `token` 空（trim 后） | `Error::Invalid("先在设置里填写个人令牌")` |
| HTTP 401 或 403 | `Error::Denied("服务器拒绝了令牌，请在设置里更新个人令牌")` |
| HTTP 非 2xx | `Error::Failed("HTTP {status}：{body 前 300 字符}")` |
| 响应非 JSON | `Error::Failed("响应不是 JSON：{body 前 200 字符}")` |
| `check` 判定失败 | `Error::Failed("服务器返回错误：{msg}")` |
| `add_cat` 未返回 id | `Error::Failed("创建分类「{name}」后服务器没有返回 id")` |

超时固定 `req.timeout_ms = Some(3_000)`——见 [budget-and-queue.md](./budget-and-queue.md) 的预算链。

---

## 5. Good / Base / Bad Cases

**Good：一个带请求体与返回体的接口**

`POST /api/users/create`，`body_type = Some("CreateUser")`，`return_type = "Result<UserDto>"`。
`req_body_other` 是 `CreateUser` 的对象 Schema，`res_body` 是 `UserDto` 的对象 Schema（`Result` 被拆掉）。
写请求的 body 里 `path`/`method`/`catid`/`project_id`/`token` 均就位。

**Base：`@RequestMapping` 没写 method**

`endpoint.method == None`。列表里显示 `ANY`，`api_doc` 的 `method` 写 `"GET"`，
`key()` 也用 `GET` 参与"是否已存在"的比对。

**Bad：把 Schema 当对象塞进去**

`"res_body": { "type": "object" }` 会让服务器把它当**非 Schema** 存，
前端接口文档显示成一段字面量。必须 `body_schema(...)` 返回 `String`（内部 `Value::to_string()`）。

**Bad：给 `list_cat` 省掉 `limit`**

`limit=5000` 是必需的实际参数：默认分页会截断，导致"已存在接口"漏判，
于是每次导出都新建一遍重复接口。

---

## 6. Tests Required

| 用例 | 位置 | 断言点 |
| - | - | - |
| 文档字段齐备 | `src/yapi.rs::tests::document_carries_schemas_and_sources` | `method`/`path` 正确；`req_query[0].example == "true"`；`req_params[0].example == "1"`；`req_body_other` 解析后 `properties.name.description == "名字"`；`res_body.type == "object"` 且 `properties.id.type == "number"`；`markdown` 含 `UserController#create` |
| 百分号编码 | `src/yapi.rs::tests::urls_are_percent_encoded` | `enc("a b/c") == "a%20b%2Fc"`；`enc("中文") == "%E4%B8%AD%E6%96%87"` |
| 真实请求体 | `src/lib.rs::tests::exports_a_batch_and_queues_the_rest` | `save` 请求 body 的 `project_id`/`catid`/`token`/`method`/`path`，且 `req_body_other`/`res_body` 可被 `serde_json::from_str` 解析 |
| 分类自愈 | `src/lib.rs::tests::a_stale_category_id_is_rebuilt_once` | 服务器持续返回 stale id 且 save 拒绝 → 最终 2 个接口都带 `"catid":"fresh"`；返回文本不含 `失败`；`save` 总次数 ≥ 3 |

`MockHost::on_fetch` 的模拟响应就是协议样本，新增端点时照抄它的 `json!` 形状（`errcode` + `data`）。

---

## 7. Wrong vs Correct

### 用 `+` 编空格 / 交给 url crate

#### Wrong

```rust
let query = format!("token={}", token.replace(' ', "+"));
```

YApi 一侧用标准解析，`+` 在查询串里可能被当字面加号，令牌里恰好含空格时就认证失败，且现象随机。

#### Correct

```rust
let query = format!("token={}&project_id={}", enc(&self.token), enc(&self.project_id));
```

`enc` 把空格编成 `%20`，并编码 `/` 等一切非 unreserved 字符。

### `save_api` 失败后无限重试

#### Wrong

```rust
loop {
    match client.save_api(&doc, &cat_id, existing) {
        Ok(()) => break,
        Err(_) => continue,     // 服务器持续报错 → 烧光 5 秒预算
    }
}
```

#### Correct

```rust
match client.save_api(&doc, &cat_id, existing.get(&key).map(String::as_str)) {
    Ok(()) => saved += 1,
    Err(first) => match rebuild_category(&client, &category, &mut categories) {
        Some(fresh) if fresh != cat_id => match client.save_api(&doc, &fresh, None) {
            Ok(()) => saved += 1,
            Err(second) => failed.push(format!("{key}：{second}")),
        },
        _ => failed.push(format!("{key}：{first}")),
    },
}
```

失败进 `failed` 集合，批次继续，用户看到"其中 N 个失败"。
