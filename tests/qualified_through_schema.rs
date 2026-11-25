//! Review round 7: a fully qualified type must survive the schema walk.
//!
//! The scan kept `com.alpha.Payload` in its queue, but the schema walk reduced it
//! to `Payload` before asking the resolver, so the endpoint's own package won.
//! These cases make both `Payload`s reachable, which is what exposes it.

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

fn payload_pkg(name: &str, field: &str) -> String {
    format!("package {name};\n\npublic class Payload {{\n    private String {field};\n}}\n")
}

/// Two controllers, both `Payload`s reachable: A writes the qualified name, B the
/// simple one.
fn two_payload_project(a_body: &str, a_ret: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "src/com/web/AController.java",
            format!(
                "package com.web;\n\n@RestController\npublic class AController {{\n    @PostMapping(\"/a\")\n    public {a_ret} save(@RequestBody {a_body} form) {{ return null; }}\n}}\n"
            ),
        ),
        (
            "src/com/web/BController.java",
            "package com.web;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public Payload save(@RequestBody Payload form) { return null; }\n}\n".to_owned(),
        ),
        ("src/com/alpha/Payload.java", payload_pkg("com.alpha", "alpha")),
        ("src/com/web/Payload.java", payload_pkg("com.web", "beta")),
    ]
}

#[test]
fn a_fully_qualified_body_type_survives_the_schema_walk() {
    // The review's four-file case: without the fix, /a came out as `beta` because
    // the schema reduced `com.alpha.Payload` to `Payload`.
    // Both the parameter and the return type are fully qualified, as in the review.
    let scanned = scan(two_payload_project("com.alpha.Payload", "com.alpha.Payload"));
    let a_req = schema(&scanned, "/a", "req_body_other");
    let a_res = schema(&scanned, "/a", "res_body");
    let b_req = schema(&scanned, "/b", "req_body_other");
    println!("/a req -> {a_req}");
    println!("/a res -> {a_res}");
    println!("/b req -> {b_req}");

    assert!(a_req.contains("alpha"), "/a request is com.alpha.Payload: {a_req}");
    assert!(!a_req.contains("beta"), "/a must not use com.web.Payload: {a_req}");
    assert!(a_res.contains("alpha"), "/a response is com.alpha.Payload: {a_res}");
    assert!(!a_res.contains("beta"), "/a response must not use com.web.Payload: {a_res}");
    assert!(b_req.contains("beta"), "/b means com.web.Payload: {b_req}");
}

#[test]
fn a_fully_qualified_field_type_survives_the_schema_walk() {
    // The same, reached through a Holder field rather than the body itself.
    let mut files = vec![
        (
            "src/com/web/AController.java",
            "package com.web;\n\n@RestController\npublic class AController {\n    @PostMapping(\"/a\")\n    public Holder save(@RequestBody Holder form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/web/Holder.java",
            "package com.web;\n\npublic class Holder {\n    private com.alpha.Payload payload;\n}\n".to_owned(),
        ),
        (
            "src/com/web/BController.java",
            "package com.web;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public Payload save(@RequestBody Payload form) { return null; }\n}\n".to_owned(),
        ),
        ("src/com/alpha/Payload.java", payload_pkg("com.alpha", "alpha")),
        ("src/com/web/Payload.java", payload_pkg("com.web", "beta")),
    ];
    files.rotate_left(0);
    let scanned = scan(files);
    let a_req = schema(&scanned, "/a", "req_body_other");
    println!("/a (holder) -> {a_req}");
    assert!(a_req.contains("alpha"), "the qualified field is com.alpha.Payload: {a_req}");
    assert!(!a_req.contains("beta"), "must not use com.web.Payload: {a_req}");
}

#[test]
fn a_fully_qualified_type_inside_a_generic_survives_the_schema_walk() {
    let mut files = two_payload_project(
        "java.util.List<com.alpha.Payload>",
        "java.util.List<Payload>",
    );
    files.truncate(4);
    // Replace B so only A's list is interesting, but keep both Payloads reachable
    // by leaving the same-package class in place.
    files[1] = (
        "src/com/web/BController.java",
        "package com.web;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public Payload save(@RequestBody Payload form) { return null; }\n}\n".to_owned(),
    );
    let scanned = scan(files);
    let a_req = schema(&scanned, "/a", "req_body_other");
    println!("/a (generic) -> {a_req}");
    assert!(a_req.contains("alpha"), "the qualified argument is com.alpha.Payload: {a_req}");
    assert!(!a_req.contains("beta"), "must not use com.web.Payload: {a_req}");
}
