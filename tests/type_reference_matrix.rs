//! Every spelling Java allows for referring to a type, in one place.
//!
//! Three review rounds in a row introduced a fix for one spelling and broke
//! another, so they are all pinned here rather than one at a time.

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

fn req(scanned: &scan::ScanResult, path: &str) -> String {
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no endpoint for {path}"));
    let doc = yapi::api_doc(endpoint, &scanned.dto, 4, Lang::En);
    doc["req_body_other"].as_str().unwrap_or_default().to_owned()
}

fn ctrl(body: &str) -> Vec<(&'static str, String)> {
    vec![(
        "src/com/web/Ctrl.java",
        format!(
            "package com.web;\n\n@RestController\npublic class Ctrl {{\n    @PostMapping(\"/t\")\n    public Object save(@RequestBody {body} form) {{ return null; }}\n}}\n"
        ),
    )]
}

fn outer() -> (&'static str, String) {
    (
        "src/com/alpha/Outer.java",
        "package com.alpha;\n\npublic class Outer {\n    public static class Request {\n        private String alpha;\n    }\n}\n".to_owned(),
    )
}

/// A plain same-package top-level class.
#[test]
fn a_same_package_type() {
    let mut files = ctrl("Payload");
    files.push((
        "src/com/web/Payload.java",
        "package com.web;\n\npublic class Payload {\n    private String web;\n}\n".to_owned(),
    ));
    let scanned = scan(files);
    let s = req(&scanned, "/t");
    println!("same-package -> {s}");
    assert!(s.contains("web"), "{s}");
}

/// `import com.alpha.Outer.Request` then a bare `Request` — importing the member
/// type itself, which is idiomatic Java.
#[test]
fn a_statically_imported_member_type() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\nimport com.alpha.Outer.Request;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/t\")\n    public Object save(@RequestBody Request form) { return null; }\n}\n".to_owned(),
        ),
        outer(),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/t");
    println!("static-import -> {s}");
    assert!(s.contains("alpha"), "the imported member type is com.alpha.Outer.Request: {s}");
}

/// `Outer.Middle.Inner` — two levels of nesting, written relative.
#[test]
fn a_two_level_member_path() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\nimport com.alpha.Outer;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/t\")\n    public Object save(@RequestBody Outer.Middle.Inner form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/alpha/Outer.java",
            "package com.alpha;\n\npublic class Outer {\n    public static class Middle {\n        public static class Inner {\n            private String alpha;\n        }\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/t");
    println!("two-level -> {s}");
    assert!(s.contains("alpha"), "Outer.Middle.Inner is com.alpha.Outer.Middle.Inner: {s}");
}

/// A fully qualified member type used as a generic argument.
#[test]
fn a_qualified_member_inside_a_generic() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/t\")\n    public Object save(@RequestBody java.util.List<com.alpha.Outer.Request> form) { return null; }\n}\n".to_owned(),
        ),
        outer(),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/t");
    println!("generic-member -> {s}");
    assert!(s.contains("alpha"), "the argument is com.alpha.Outer.Request: {s}");
}

/// A wildcard import alongside a relative member path: the qualifier still comes
/// from the wildcard package.
#[test]
fn a_wildcard_imported_qualifier() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\nimport com.alpha.*;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/t\")\n    public Object save(@RequestBody Outer.Request form) { return null; }\n}\n".to_owned(),
        ),
        outer(),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/t");
    println!("wildcard-qualifier -> {s}");
    assert!(s.contains("alpha"), "Outer comes from com.alpha: {s}");
}

/// A single-type import shadows a same-package class for a plain name, and the
/// imported member type is the one that wins.
#[test]
fn an_imported_member_beats_a_same_package_class() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\nimport com.alpha.Outer.Request;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/t\")\n    public Object save(@RequestBody Request form) { return null; }\n}\n".to_owned(),
        ),
        outer(),
        (
            "src/com/web/Request.java",
            "package com.web;\n\npublic class Request {\n    private String web;\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/t");
    println!("import-beats-package -> {s}");
    assert!(s.contains("alpha"), "the single-type import wins (JLS 6.4.1): {s}");
    assert!(!s.contains("web"), "must not use com.web.Request: {s}");
}
