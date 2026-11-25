//! Review round 5: a single-type import outranks a same-package top-level type.
//!
//! JLS 6.4.1 orders simple type names: member types of the enclosing class, then
//! single-type imports, then types in the same package. `AController` imports
//! `com.alpha.Payload` while `com/web/Payload.java` also exists, so `Payload` in A
//! means the imported one — B, with no import, means the same-package one.

use std::collections::BTreeMap;

use openidea_apimocktle::i18n::Lang;
use openidea_apimocktle::scan::{self, Options};
use openidea_apimocktle::yapi;
use openidea_plugin::mock::MockHost;
use openidea_plugin::{
    Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch,
};

const ROOT: &str = "/proj";

fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn scan(files: &[(&str, &str)]) -> scan::ScanResult {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("no network in this test".into()))
    });
    let mut indexed: BTreeMap<String, String> = BTreeMap::new();
    for (rel, text) in files {
        let path = format!("{ROOT}/{rel}");
        host.add_file(&path, text);
        indexed.insert(path, (*text).to_owned());
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
        Ok(TextSearch {
            matches,
            limit_hit: false,
        })
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

fn import_beats_same_package_project() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "src/com/web/AController.java",
            "package com.web;\n\nimport com.alpha.Payload;\n\n@RestController\npublic class AController {\n    @PostMapping(\"/a\")\n    public Payload save(@RequestBody Payload form) { return null; }\n}\n",
        ),
        (
            "src/com/web/BController.java",
            "package com.web;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public Payload save(@RequestBody Payload form) { return null; }\n}\n",
        ),
        (
            "src/com/alpha/Payload.java",
            "package com.alpha;\n\npublic class Payload {\n    private String alpha;\n}\n",
        ),
        (
            "src/com/web/Payload.java",
            "package com.web;\n\npublic class Payload {\n    private String beta;\n}\n",
        ),
    ]
}

#[test]
fn an_imported_type_beats_a_same_package_type() {
    let scanned = scan(&import_beats_same_package_project());
    let a_req = schema(&scanned, "/a", "req_body_other");
    let b_req = schema(&scanned, "/b", "req_body_other");
    println!("/a req -> {a_req}");
    println!("/b req -> {b_req}");

    // JLS 6.4.1: A's single-type import wins over com.web's own Payload.
    assert!(
        a_req.contains("alpha"),
        "AController imports com.alpha.Payload: {a_req}"
    );
    assert!(
        !a_req.contains("beta"),
        "/a must not use the same-package Payload: {a_req}"
    );
    // B has no import, so it means the same-package type.
    assert!(
        b_req.contains("beta"),
        "BController means com.web.Payload: {b_req}"
    );
    assert!(
        !b_req.contains("alpha"),
        "/b must not borrow A's import: {b_req}"
    );
}

#[test]
fn the_response_schema_follows_the_same_order() {
    let scanned = scan(&import_beats_same_package_project());
    let a_res = schema(&scanned, "/a", "res_body");
    println!("/a res -> {a_res}");
    assert!(
        a_res.contains("alpha") && !a_res.contains("beta"),
        "/a res: {a_res}"
    );
}

#[test]
fn a_member_type_still_beats_an_import() {
    // JLS puts member types before single-type imports, so this ordering must not
    // regress: a nested `Payload` in the controller wins over an imported one.
    let files = [
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\nimport com.alpha.Payload;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/c\")\n    public Payload save(@RequestBody Payload form) { return null; }\n\n    public static class Payload {\n        private String nested;\n    }\n}\n",
        ),
        (
            "src/com/alpha/Payload.java",
            "package com.alpha;\n\npublic class Payload {\n    private String alpha;\n}\n",
        ),
    ];
    let scanned = scan(&files);
    let req = schema(&scanned, "/c", "req_body_other");
    println!("/c req -> {req}");
    assert!(
        req.contains("nested"),
        "the member type outranks the import: {req}"
    );
}
