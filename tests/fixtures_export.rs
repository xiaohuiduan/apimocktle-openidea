//! End-to-end coverage driven by `fixtures/spring-demo`: the fixture sources go
//! through the real pipeline (`scan` → `api_doc` → `save_api`) against a mocked
//! ApiMocktle server, so the whole path is exercised without a network.

use std::path::{Path, PathBuf};

use openidea_apimocktle::{scan, yapi};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Error, HttpResponse, Location, Position, Range, TextMatch, TextSearch};
use serde_json::{Value, json};

/// Project root the fixture is mounted under.
const ROOT: &str = "/proj";
/// The category description the plugin sends; only its presence matters here.
const CATEGORY_DESC: &str = "由 OpenIDEA ApiMocktle 插件维护";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/spring-demo")
}

/// Every `.java` / `.kt` under the fixture, as (project path, source text).
fn fixture_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("java" | "kt")) {
                out.push(path);
            }
        }
    }

    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("a readable fixture");
            let rel = path.strip_prefix(&root).expect("a path inside the fixture");
            (format!("{ROOT}/{}", rel.to_string_lossy().replace('\\', "/")), text)
        })
        .collect()
}

fn reply(status: u16, body: Value) -> Result<HttpResponse, Error> {
    Ok(HttpResponse { status, headers: Vec::new(), body: body.to_string().into_bytes() })
}

/// The fixture project plus a YApi server that answers like ApiMocktle.
fn host() -> MockHost {
    let host = MockHost::install().with_capabilities(&[
        "project.read",
        "storage",
        "net:localhost",
        "net:127.0.0.1",
        "clipboard",
    ]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));

    let sources = fixture_sources();
    assert!(sources.len() >= 8, "the fixture keeps its samples: {sources:?}");
    for (path, text) in &sources {
        host.add_file(path, text);
    }
    let pom = std::fs::read_to_string(fixture_root().join("pom.xml")).expect("the fixture pom");
    host.add_file(&format!("{ROOT}/pom.xml"), &pom);

    // Every source answers the annotation search; the parser drops the files
    // that declare no controller, exactly as it does against a real project.
    let all = sources.clone();
    host.on_search(move |_query, _limit| {
        Ok(TextSearch {
            matches: all
                .iter()
                .map(|(path, _)| TextMatch {
                    location: Location {
                        path: path.clone(),
                        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    },
                    line: String::new(),
                })
                .collect(),
            limit_hit: false,
        })
    });

    host.set_setting("serverUrl", "\"http://localhost:4000\"");
    host.set_setting("token", "\"secret-token\"");
    host.set_setting("includeKotlin", "true");
    host.set_setting("schemaDepth", "4");

    host.on_fetch(|request| {
        let url = request.url.as_str();
        if url.contains("/api/project/list") {
            reply(200, json!({ "errcode": 0, "data": [{ "_id": "p1", "name": "Demo" }] }))
        } else if url.contains("/api/interface/getCatMenu") {
            reply(200, json!({ "errcode": 0, "data": [] }))
        } else if url.contains("/api/interface/add_cat") {
            let body = String::from_utf8(request.body.clone().unwrap_or_default()).unwrap_or_default();
            let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let name = body.get("name").and_then(Value::as_str).unwrap_or("Default").to_owned();
            reply(200, json!({ "errcode": 0, "data": { "_id": format!("c_{name}"), "name": name } }))
        } else if url.contains("/api/interface/list_cat") {
            reply(200, json!({ "errcode": 0, "data": { "list": [] } }))
        } else if url.contains("/api/interface/save") {
            reply(200, json!({ "errcode": 0 }))
        } else {
            reply(404, json!({ "errcode": 404, "errmsg": "not found" }))
        }
    });
    host
}

/// The document saved for `key` (`"METHOD /path"`).
fn doc<'a>(docs: &'a [(String, Value)], key: &str) -> &'a Value {
    match docs.iter().find(|(saved, _)| saved == key) {
        Some((_, doc)) => doc,
        None => panic!(
            "no document for {key}; saved: {:?}",
            docs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()
        ),
    }
}

#[test]
fn the_fixture_project_exports_end_to_end() {
    let host = host();
    let project = host.invocation().project().expect("a project");
    let scanned =
        scan::scan(&project, &scan::Options { depth: 4, ..scan::Options::default() }).expect("a scan");

    assert_eq!(scanned.files, 9, "every fixture source is read");
    let labels: Vec<String> = scanned.endpoints.iter().map(|e| e.key()).collect();
    assert_eq!(scanned.endpoints.len(), 10, "{labels:?}");

    let client =
        yapi::Client { base: "http://localhost:4000".into(), token: "secret-token".into(), project_id: "p1".into() };
    let mut docs: Vec<(String, Value)> = Vec::new();
    for endpoint in &scanned.endpoints {
        let cat = client.find_or_create_category(&endpoint.class_name, CATEGORY_DESC).expect("a category");
        let doc = yapi::api_doc(endpoint, &scanned.dto, 4, openidea_apimocktle::i18n::Lang::En);
        client.save_api(&doc, &cat.id, None).expect("a save");
        docs.push((endpoint.key(), doc));
    }
    assert_eq!(docs.len(), 10);
    let saves = host.requests().iter().filter(|r| r.url.contains("/api/interface/save")).count();
    assert_eq!(saves, 10, "one save per endpoint");

    // Feign: `name` is a discovery name, so it must not reach the route.
    assert!(labels.iter().all(|key| !key.contains("passenger-service")), "{labels:?}");
    assert_eq!(doc(&docs, "GET /api/passenger/{id}")["method"], "GET");
    assert_eq!(doc(&docs, "POST /api/passenger/save")["path"], "/api/passenger/save");

    // JAX-RS: the verb and the path live in two annotations.
    let jaxrs = scanned
        .endpoints
        .iter()
        .find(|e| e.path == "/api/jaxrs/orders/{id}")
        .expect("the JAX-RS route");
    assert_eq!(jaxrs.framework, "JAX-RS");

    // Kotlin: both the block body and the expression body are read.
    let kotlin: Vec<&str> = scanned
        .endpoints
        .iter()
        .filter(|e| e.class_name == "KotlinController")
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(kotlin, vec!["/api/kotlin/health", "/api/kotlin/orders/{id}"]);

    // `Result<List<Order>>` keeps the wrapper and substitutes the list.
    let list = doc(&docs, "GET /orders/list");
    let res: Value = serde_json::from_str(list["res_body"].as_str().expect("a schema string")).expect("json");
    assert_eq!(res["type"], "object");
    assert_eq!(res["properties"]["data"]["type"], "array");
    let item = &res["properties"]["data"]["items"]["properties"];
    assert_eq!(item["order_no"]["type"], "string", "@JsonProperty renames the field");
    assert_eq!(item["amount"]["type"], "string", "BigDecimal is described as a string");
    assert_eq!(item["address"]["type"], "object");
    assert_eq!(item["address"]["properties"]["city"]["type"], "string");
    assert_eq!(item["address"]["properties"]["zip_code"]["type"], "string");
    let mut names: Vec<&str> = item.as_object().expect("properties").keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["address", "amount", "order_no"], "@JsonIgnore and constants stay out");

    // Multipart: the parts become `req_body_form`, not an empty JSON body.
    let upload = doc(&docs, "POST /api/upload/file");
    assert_eq!(upload["req_body_type"], "form");
    let form = upload["req_body_form"].as_array().expect("an array of parts");
    assert_eq!(form.len(), 2, "{form:?}");
    let file = form.iter().find(|part| part["name"] == "file").expect("the file part");
    assert_eq!(file["type"], "file");
    assert_eq!(file["required"], 1);
    let metadata = form.iter().find(|part| part["name"] == "metadata").expect("the metadata part");
    assert_eq!(metadata["type"], "text");
    assert_eq!(metadata["required"], 0);

    // Collection-valued parts: the element type decides, not the wrapper name.
    // `List<MultipartFile>` and `List<Part>` are uploads; `String` is not.
    let many = doc(&docs, "POST /api/upload/files");
    assert_eq!(many["req_body_type"], "form");
    let parts = many["req_body_form"].as_array().expect("an array of parts");
    assert_eq!(parts.len(), 3, "{parts:?}");
    for name in ["files", "parts"] {
        let part = parts
            .iter()
            .find(|part| part["name"] == name)
            .unwrap_or_else(|| panic!("the {name} part: {parts:?}"));
        assert_eq!(part["type"], "file", "{name} carries uploads");
    }
    let note = parts.iter().find(|part| part["name"] == "note").expect("the note part");
    assert_eq!(note["type"], "text", "a text part stays text next to file parts");

    // A method without form parts keeps the JSON shape byte for byte.
    let json_save = doc(&docs, "POST /api/passenger/save");
    assert_eq!(json_save["req_body_type"], "json");
    assert_eq!(json_save["req_body_form"], json!([]));
}
