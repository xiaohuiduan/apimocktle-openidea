//! Java type references whose meaning the compiler fixes by syntax alone.
//!
//! Each case is valid Java with an unambiguous meaning, so a wrong schema is a
//! genuine defect rather than an undecidable project.

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

fn pkg(name: &str, field: &str) -> String {
    format!("package {name};\n\npublic class Payload {{\n    private String {field};\n}}\n")
}

/// `com.alpha.Payload` on a field is that type, whatever the package holds.
#[test]
fn a_fully_qualified_field_type_ignores_scope() {
    let files = vec![
        (
            "src/com/web/Holder.java",
            "package com.web;\n\npublic class Holder {\n    private com.alpha.Payload payload;\n}\n".to_owned(),
        ),
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/h\")\n    public Holder save(@RequestBody Holder form) { return null; }\n}\n".to_owned(),
        ),
        ("src/com/alpha/Payload.java", pkg("com.alpha", "alpha")),
        ("src/com/web/Payload.java", pkg("com.web", "beta")),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/h");
    println!("fq-field -> {s}");
    assert!(s.contains("alpha"), "com.alpha.Payload is fully qualified: {s}");
    assert!(!s.contains("beta"), "must not use the same-package one: {s}");
}

/// The same, written on the `@RequestBody` parameter.
#[test]
fn a_fully_qualified_body_type_ignores_scope() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/a\")\n    public Object save(@RequestBody com.alpha.Payload form) { return null; }\n}\n".to_owned(),
        ),
        ("src/com/alpha/Payload.java", pkg("com.alpha", "alpha")),
        ("src/com/web/Payload.java", pkg("com.web", "beta")),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/a");
    println!("fq-body -> {s}");
    assert!(s.contains("alpha"), "com.alpha.Payload is fully qualified: {s}");
    assert!(!s.contains("beta"), "must not use the same-package one: {s}");
}

/// JLS 6.4.1: a type-import-on-demand does NOT outrank a same-package type.
#[test]
fn a_wildcard_import_loses_to_a_same_package_type() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\nimport com.alpha.*;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/w\")\n    public Payload save(@RequestBody Payload form) { return null; }\n}\n".to_owned(),
        ),
        ("src/com/alpha/Payload.java", pkg("com.alpha", "alpha")),
        ("src/com/web/Payload.java", pkg("com.web", "beta")),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/w");
    println!("wildcard -> {s}");
    assert!(s.contains("beta"), "the same-package type wins over a wildcard: {s}");
}

/// An imported type used as a generic argument keeps the import's meaning.
#[test]
fn an_imported_type_inside_a_generic_wrapper_resolves() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\nimport com.alpha.Payload;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/g\")\n    public java.util.List<Payload> save(@RequestBody java.util.List<Payload> form) { return null; }\n}\n".to_owned(),
        ),
        ("src/com/alpha/Payload.java", pkg("com.alpha", "alpha")),
        ("src/com/web/Payload.java", pkg("com.web", "beta")),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/g");
    println!("generic -> {s}");
    assert!(s.contains("alpha"), "the import applies inside the generic: {s}");
    assert!(!s.contains("beta"), "must not use the same-package one: {s}");
}

/// A fully qualified type inside a generic argument.
#[test]
fn a_fully_qualified_type_inside_a_generic_resolves() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/gf\")\n    public Object save(@RequestBody java.util.List<com.alpha.Payload> form) { return null; }\n}\n".to_owned(),
        ),
        ("src/com/alpha/Payload.java", pkg("com.alpha", "alpha")),
        ("src/com/web/Payload.java", pkg("com.web", "beta")),
    ];
    let scanned = scan(files);
    let s = req(&scanned, "/gf");
    println!("generic-fq -> {s}");
    assert!(s.contains("alpha"), "the qualified argument is that type: {s}");
    assert!(!s.contains("beta"), "must not use the same-package one: {s}");
}
