//! Review: a qualified *member* type must resolve, not become an empty object.
//!
//! `AController.Request` is `com.acme::AController::Request`: the qualifier is the
//! enclosing class, not a package. Splitting at the last dot alone produced
//! `AController::Request` / `com.acme.AController::Request`, which match nothing.

use std::collections::BTreeMap;

use openidea_apimocktle::i18n::Lang;
use openidea_apimocktle::scan::{self, Options};
use openidea_apimocktle::yapi;
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Error, HttpRequest, HttpResponse, Location, Position, Range, TextQuery, TextMatch, TextSearch};

const ROOT: &str = "/proj";

fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

fn scan(files: Vec<(&'static str, String)>) -> scan::ScanResult {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("no network in this test".into()))
    });
    let mut indexed: BTreeMap<String, String> = BTreeMap::new();
    for (rel, text) in &files {
        let path = format!("{ROOT}/{rel}");
        host.add_file(&path, text);
        indexed.insert(path, text.clone());
    }
    host.on_search(move |q: &TextQuery, _l| {
        let d = declaration_name(&q.pattern);
        let mut matches = Vec::new();
        for (path, text) in &indexed {
            if !path.ends_with(".java") {
                continue;
            }
            let hit = match &d {
                Some(name) => text.contains(&format!("class {name} ")),
                None => text.contains("@RestController"),
            };
            if hit {
                matches.push(TextMatch {
                    location: Location {
                        path: path.clone(),
                        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    },
                    line: String::new(),
                });
            }
        }
        Ok(TextSearch { matches, limit_hit: false })
    });
    let project = host.invocation().project().expect("a project");
    scan::scan(&project, &Options::default()).expect("a scan")
}

fn schema(scanned: &scan::ScanResult, path: &str, key: &str) -> String {
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no endpoint for {path}"));
    let doc = yapi::api_doc(endpoint, &scanned.dto, 4, Lang::En);
    doc[key].as_str().unwrap_or_default().to_owned()
}

fn controller_with(body: &str, ret: &str) -> Vec<(&'static str, String)> {
    vec![(
        "src/com/acme/AController.java",
        format!(
            "package com.acme;\n\n@RestController\npublic class AController {{\n    @PostMapping(\"/a\")\n    public {ret} save(@RequestBody {body} form) {{ return null; }}\n\n    public static class Request {{\n        private String alpha;\n    }}\n}}\n"
        ),
    )]
}

#[test]
fn a_relative_member_type_resolves() {
    let scanned = scan(controller_with("AController.Request", "AController.Request"));
    let req = schema(&scanned, "/a", "req_body_other");
    let res = schema(&scanned, "/a", "res_body");
    println!("/a req -> {req}");
    println!("/a res -> {res}");
    assert!(req.contains("alpha"), "AController.Request has alpha: {req}");
    assert!(res.contains("alpha"), "the response too: {res}");
}

#[test]
fn a_fully_qualified_member_type_resolves() {
    let scanned = scan(controller_with(
        "com.acme.AController.Request",
        "com.acme.AController.Request",
    ));
    let req = schema(&scanned, "/a", "req_body_other");
    let res = schema(&scanned, "/a", "res_body");
    println!("/a req (fq) -> {req}");
    println!("/a res (fq) -> {res}");
    assert!(req.contains("alpha"), "the qualified member type has alpha: {req}");
    assert!(res.contains("alpha"), "the response too: {res}");
}

#[test]
fn a_bare_member_type_still_resolves() {
    let scanned = scan(controller_with("Request", "Request"));
    let req = schema(&scanned, "/a", "req_body_other");
    println!("/a req (bare) -> {req}");
    assert!(req.contains("alpha"), "the bare name keeps working: {req}");
}

/// The qualifier itself is an imported type: `import com.alpha.Outer` then
/// `Outer.Request` means `com.alpha::Outer::Request`.
#[test]
fn an_imported_qualifier_resolves() {
    let files = vec![
        (
            "src/com/web/AController.java",
            "package com.web;\n\nimport com.alpha.Outer;\n\n@RestController\npublic class AController {\n    @PostMapping(\"/a\")\n    public Outer.Request save(@RequestBody Outer.Request form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/alpha/Outer.java",
            "package com.alpha;\n\npublic class Outer {\n    public static class Request {\n        private String alpha;\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let req = schema(&scanned, "/a", "req_body_other");
    let res = schema(&scanned, "/a", "res_body");
    println!("/a req (imported) -> {req}");
    println!("/a res (imported) -> {res}");
    assert!(req.contains("alpha"), "Outer.Request is com.alpha.Outer.Request: {req}");
    assert!(res.contains("alpha"), "the response too: {res}");
}