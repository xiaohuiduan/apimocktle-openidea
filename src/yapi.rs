//! The YApi-compatible client ApiMocktle speaks: project listing, categories,
//! interface listing and interface save, plus the document a saved interface
//! carries.

use openidea_plugin::{Error, HttpRequest, host};
use serde_json::{Value, json};

use crate::i18n;
use crate::model::{Endpoint, ParamSpec, ResolvedClass, body_schema};
use crate::scan::DtoIndex;

const LIST_PROJECTS: &str = "/api/project/list";
const SAVE_API: &str = "/api/interface/save";
const GET_CAT_MENU: &str = "/api/interface/getCatMenu";
const ADD_CAT: &str = "/api/interface/add_cat";
const LIST_CAT: &str = "/api/interface/list_cat";

#[derive(Clone, Debug, Default)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub desc: String,
}

#[derive(Clone, Debug, Default)]
pub struct Category {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct ApiBrief {
    pub id: String,
    pub path: String,
    pub method: String,
}

pub struct Client {
    pub base: String,
    pub token: String,
    pub project_id: String,
}

fn enc(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The base URL as the plugin compares and stores it: no surrounding spaces,
/// no trailing slash, so `http://x/` and `http://x` are the same target. The
/// export queue records this form, which is why it is public.
pub fn normalize_base(base: &str) -> String {
    base.trim().trim_end_matches('/').to_owned()
}

/// YApi answers `{"errcode": 0}` on success and a non-zero code otherwise.
fn check(payload: &Value) -> Result<(), Error> {
    let code = payload.get("errcode").and_then(Value::as_i64).or_else(|| payload.get("code").and_then(Value::as_i64));
    let message = payload
        .get("errmsg")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .unwrap_or("");
    let ok = matches!(code, Some(0) | Some(200)) || matches!(message, "成功" | "成功！");
    if ok { Ok(()) } else { Err(Error::Failed(i18n::server_error(&i18n::redact_token(message)))) }
}

impl Client {
    fn trimmed(&self) -> String {
        normalize_base(&self.base)
    }

    fn call(&self, method: &str, path: &str, query: &str, body: Option<Value>) -> Result<Value, Error> {
        let base = self.trimmed();
        if base.is_empty() {
            return Err(Error::Invalid(i18n::YAPI_SET_SERVER.get(i18n::lang()).into()));
        }
        if self.token.trim().is_empty() {
            return Err(Error::Invalid(i18n::YAPI_SET_TOKEN.get(i18n::lang()).into()));
        }
        let url = if query.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{query}") };
        let mut req = HttpRequest {
            method: method.to_owned(),
            url,
            ..HttpRequest::default()
        };
        if let Some(payload) = body {
            req.headers.push(("Content-Type".into(), "application/json".into()));
            req.body = Some(payload.to_string().into_bytes());
        }
        // A slow server must fail the call, not blow the 5-second host budget.
        req.timeout_ms = Some(3_000);
        // The token rides in the query string, and a transport error quotes the
        // whole URL: redact before the message can reach the UI (`?` alone would
        // surface it in the dashboard's status line and in a notification).
        let response = host::fetch(&req).map_err(|e| match e {
            // These two carry no URL; keep their meaning (and their wording).
            e @ (Error::Denied(_) | Error::Cancelled) => e,
            other => Error::Failed(i18n::redact_token(&other.to_string())),
        })?;
        let text = response.text();
        if response.status == 401 || response.status == 403 {
            return Err(Error::Denied(i18n::YAPI_TOKEN_REJECTED.get(i18n::lang()).into()));
        }
        if !(200..300).contains(&response.status) {
            let shown: String = text.chars().take(300).collect();
            return Err(Error::Failed(i18n::http_status(response.status, &i18n::redact_token(&shown))));
        }
        let payload: Value = serde_json::from_str(&text).map_err(|_| {
            let shown: String = text.chars().take(200).collect();
            Error::Failed(i18n::not_json(&shown))
        })?;
        check(&payload)?;
        Ok(payload)
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectInfo>, Error> {
        let payload = self.call("GET", LIST_PROJECTS, &format!("token={}", enc(&self.token)), None)?;
        let items = payload.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|item| {
                Some(ProjectInfo {
                    id: item.get("_id").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))?,
                    name: item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    desc: item.get("desc").and_then(Value::as_str).unwrap_or_default().to_owned(),
                })
            })
            .collect())
    }

    pub fn list_categories(&self) -> Result<Vec<Category>, Error> {
        let query = format!("project_id={}&token={}", enc(&self.project_id), enc(&self.token));
        let payload = self.call("GET", GET_CAT_MENU, &query, None)?;
        let items = payload.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|item| {
                Some(Category {
                    id: item.get("_id").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))?,
                    name: item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                })
            })
            .collect())
    }

    pub fn create_category(&self, name: &str, desc: &str) -> Result<Category, Error> {
        let body = json!({
            "desc": desc,
            "project_id": self.project_id,
            "name": name,
            "token": self.token,
        });
        let payload = self.call("POST", ADD_CAT, "", Some(body))?;
        let data = payload.get("data").cloned().unwrap_or(Value::Null);
        let id = data
            .get("_id")
            .map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))
            .unwrap_or_default();
        if id.is_empty() {
            return Err(Error::Failed(i18n::category_without_id(name)));
        }
        Ok(Category { id, name: data.get("name").and_then(Value::as_str).unwrap_or(name).to_owned() })
    }

    /// The id of `name`, creating the category when it does not exist yet.
    pub fn find_or_create_category(&self, name: &str, desc: &str) -> Result<Category, Error> {
        // A server that is briefly unreachable still gets a creation attempt.
        if let Ok(categories) = self.list_categories()
            && let Some(found) = categories.into_iter().find(|c| c.name == name && !c.id.is_empty())
        {
            return Ok(found);
        }
        self.create_category(name, desc)
    }

    pub fn list_apis(&self, cat_id: &str) -> Result<Vec<ApiBrief>, Error> {
        let query = format!(
            "token={}&project_id={}&catid={}&limit=5000",
            enc(&self.token),
            enc(&self.project_id),
            enc(cat_id)
        );
        let payload = self.call("GET", LIST_CAT, &query, None)?;
        let items = payload
            .get("data")
            .and_then(|d| d.get("list"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|item| {
                Some(ApiBrief {
                    id: item.get("_id").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))?,
                    path: item.get("path").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    method: item.get("method").and_then(Value::as_str).unwrap_or_default().to_owned(),
                })
            })
            .collect())
    }

    pub fn save_api(&self, doc: &Value, cat_id: &str, existing_id: Option<&str>) -> Result<(), Error> {
        let mut body = doc.clone();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("token".into(), Value::String(self.token.clone()));
            obj.insert("project_id".into(), Value::String(self.project_id.clone()));
            obj.insert("catid".into(), Value::String(cat_id.to_owned()));
            if let Some(id) = existing_id {
                obj.insert("id".into(), Value::String(id.to_owned()));
            }
        }
        self.call("POST", SAVE_API, "", Some(body)).map(|_| ())
    }
}

fn kv(name: &str, value: &str, desc: &str, required: bool) -> Value {
    json!({ "name": name, "value": value, "example": value, "desc": desc, "required": required })
}

fn header_kv(name: &str, desc: &str, required: bool) -> Value {
    json!({ "name": name, "value": "", "example": "", "desc": desc, "required": required })
}

fn path_params(params: &[ParamSpec]) -> Value {
    Value::Array(
        params
            .iter()
            .map(|p| json!({ "name": p.name, "example": example_for(&p.ty), "desc": p.description }))
            .collect(),
    )
}

fn example_for(ty: &str) -> String {
    let simple = crate::model::simple_name(ty);
    match simple.as_str() {
        "int" | "Integer" | "long" | "Long" | "short" | "Short" | "byte" | "Byte" => "1".into(),
        "double" | "Double" | "float" | "Float" | "BigDecimal" => "1.0".into(),
        "boolean" | "Boolean" => "true".into(),
        _ => String::new(),
    }
}

/// `YapiFormParam.type`: an upload part is `file`, everything else `text`.
/// The parameter type decides this on its own; `consumes` is not parsed, since
/// a second source of truth would fight the parameter classification.
///
/// A collection names its element, not the field: `List<MultipartFile>` looks
/// like `List` from the outside, so the collection has to be unwrapped before
/// deciding. Only collections are unwrapped — a `Map<String, MultipartFile>` is
/// still one text field, not an upload.
fn form_type(ty: &str) -> &'static str {
    let (base, args) = crate::model::split_type(ty);
    let base = base.rsplit('.').next().unwrap_or(&base);
    if crate::model::is_array_like(base) {
        return args.first().map_or("text", |element| form_type(element));
    }
    if base == "MultipartFile" || base.ends_with("Part") { "file" } else { "text" }
}

/// The `req_body_form` array, shaped like the upstream `YapiFormParam`.
/// `required` is the integer 0 or 1 YApi expects, not a JSON boolean.
fn form_params(params: &[ParamSpec]) -> Value {
    Value::Array(
        params
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "example": example_for(&p.ty),
                    "type": form_type(&p.ty),
                    "required": if p.required { 1 } else { 0 },
                    "desc": p.description,
                })
            })
            .collect(),
    )
}

/// The YApi interface document for one endpoint.
///
/// `lang` picks the language of the `markdown` field, the only prose in the
/// document. It is a parameter rather than a `host::locale()` call so building a
/// document does not depend on a mounted host — the document is also built by
/// tests and by the endpoint export path with no view attached.
pub fn api_doc(endpoint: &Endpoint, dto: &DtoIndex, depth: u32, lang: i18n::Lang) -> Value {
    let resolve = |name: &str, _d: u32| {
        let fields = dto.fields.get(name)?;
        Some(ResolvedClass { params: dto.params.get(name).cloned().unwrap_or_default(), fields: fields.clone() })
    };
    let request_type = endpoint.body_type.clone();
    let req_body_other = request_type
        .as_ref()
        .map(|ty| body_schema(ty, depth, &resolve))
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }).to_string());
    // A form interface is described by `req_body_form`; YApi ignores
    // `req_body_other` in that mode, so the empty object stays as it was.
    let form = form_params(&endpoint.form_params);
    let req_body_type = if endpoint.form_params.is_empty() { "json" } else { "form" };
    let res_body = if endpoint.return_type.trim().is_empty() {
        json!({ "type": "object", "properties": {} }).to_string()
    } else {
        body_schema(&endpoint.return_type, depth, &resolve)
    };

    json!({
        "title": endpoint.summary,
        "path": endpoint.path,
        "method": endpoint.yapi_method(),
        "desc": endpoint.description,
        "markdown": i18n::api_markdown(
            lang,
            &endpoint.summary,
            &endpoint.description,
            &endpoint.handler(),
            &endpoint.framework,
            &endpoint.file
        ),
        "status": "done",
        "req_body_is_json_schema": true,
        "res_body_is_json_schema": true,
        "req_headers": Value::Array(endpoint.header_params.iter().map(|p| header_kv(&p.name, &p.description, p.required)).collect()),
        "req_query": Value::Array(endpoint.query_params.iter().map(|p| kv(&p.name, &example_for(&p.ty), &p.description, p.required)).collect()),
        "req_params": path_params(&endpoint.path_params),
        "req_body_form": form,
        "req_body_other": req_body_other,
        "req_body_type": req_body_type,
        "res_body": res_body,
        "res_body_type": "json",
        "tags": [endpoint.class_name.clone()],
        "tag": [endpoint.class_name.clone()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Endpoint, FieldSpec, ParamSpec};

    fn endpoint() -> Endpoint {
        Endpoint {
            method: Some("POST".into()),
            path: "/api/users".into(),
            class_name: "UserController".into(),
            method_name: "create".into(),
            summary: "创建用户".into(),
            description: "创建一个用户".into(),
            body_type: Some("CreateUser".into()),
            return_type: "Result<UserDto>".into(),
            query_params: vec![ParamSpec { name: "verbose".into(), ty: "boolean".into(), required: false, description: String::new() }],
            path_params: vec![ParamSpec { name: "id".into(), ty: "long".into(), required: true, description: String::new() }],
            ..Endpoint::default()
        }
    }

    #[test]
    fn document_carries_schemas_and_sources() {
        let mut dto = DtoIndex::default();
        dto.fields.insert(
            "CreateUser".to_owned(),
            vec![FieldSpec { name: "name".into(), ty: "String".into(), description: "名字".into(), children: vec![] }],
        );
        dto.fields.insert(
            "UserDto".to_owned(),
            vec![FieldSpec { name: "id".into(), ty: "long".into(), description: String::new(), children: vec![] }],
        );
        let doc = api_doc(&endpoint(), &dto, 2, i18n::Lang::Zh);
        assert_eq!(doc["method"], "POST");
        assert_eq!(doc["path"], "/api/users");
        assert_eq!(doc["req_query"][0]["name"], "verbose");
        assert_eq!(doc["req_query"][0]["example"], "true");
        assert_eq!(doc["req_params"][0]["example"], "1");
        let req: Value = serde_json::from_str(doc["req_body_other"].as_str().unwrap()).unwrap();
        assert_eq!(req["properties"]["name"]["description"], "名字");
        let res: Value = serde_json::from_str(doc["res_body"].as_str().unwrap()).unwrap();
        assert_eq!(res["type"], "object");
        assert_eq!(res["properties"]["id"]["type"], "number");
        assert!(doc["markdown"].as_str().unwrap().contains("UserController#create"));
    }

    #[test]
    fn urls_are_percent_encoded() {
        assert_eq!(enc("a b/c"), "a%20b%2Fc");
        assert_eq!(enc("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn form_parts_become_req_body_form_entries() {
        let mut endpoint = endpoint();
        // A multipart method carries its payload in the parts, not in a JSON body.
        endpoint.body_type = None;
        endpoint.form_params = vec![
            ParamSpec { name: "file".into(), ty: "MultipartFile".into(), required: true, description: "附件".into() },
            ParamSpec { name: "metadata".into(), ty: "String".into(), required: false, description: String::new() },
        ];
        let doc = api_doc(&endpoint, &DtoIndex::default(), 2, i18n::Lang::Zh);

        assert_eq!(doc["req_body_type"], "form");
        let form = doc["req_body_form"].as_array().expect("an array of parts");
        assert_eq!(form.len(), 2);
        assert_eq!(form[0]["name"], "file");
        assert_eq!(form[0]["type"], "file");
        // YApi stores `required` as 0 or 1, never as a JSON boolean.
        assert_eq!(form[0]["required"], 1);
        assert_eq!(form[0]["desc"], "附件");
        assert_eq!(form[1]["name"], "metadata");
        assert_eq!(form[1]["type"], "text");
        assert_eq!(form[1]["required"], 0);
        // The JSON-Schema slot keeps its old shape under `req_body_type = form`.
        let other: Value = serde_json::from_str(doc["req_body_other"].as_str().expect("a string")).unwrap();
        assert_eq!(other["type"], "object");
    }

    #[test]
    fn a_file_collection_is_typed_by_its_element() {
        // Spring binds a list of files as an upload; the outer name of a
        // collection says nothing about what it carries.
        assert_eq!(form_type("MultipartFile"), "file");
        assert_eq!(form_type("org.springframework.web.multipart.MultipartFile"), "file");
        assert_eq!(form_type("MultipartFile[]"), "file");
        assert_eq!(form_type("jakarta.servlet.http.Part"), "file");

        assert_eq!(form_type("List<MultipartFile>"), "file");
        assert_eq!(form_type("Set<MultipartFile>"), "file");
        assert_eq!(form_type("Collection<Part>"), "file");
        assert_eq!(form_type("List<jakarta.servlet.http.Part>"), "file");

        // Text parts keep their type, including collections of them.
        assert_eq!(form_type("String"), "text");
        assert_eq!(form_type("List<String>"), "text");
        assert_eq!(form_type("int"), "text");

        // Only collections are unwrapped: a Map is one text field, not an upload.
        assert_eq!(form_type("Map<String, MultipartFile>"), "text");
        // A bare collection carries no element type to inspect.
        assert_eq!(form_type("List"), "text");
    }

    #[test]
    fn a_json_body_keeps_req_body_type_json() {
        let doc = api_doc(&endpoint(), &DtoIndex::default(), 2, i18n::Lang::Zh);
        assert_eq!(doc["req_body_type"], "json");
        assert_eq!(doc["req_body_form"], json!([]));
    }
}
