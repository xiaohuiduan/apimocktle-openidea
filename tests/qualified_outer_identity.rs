//! Review round 4 on PR 363: two defects in the qualified-name paths.
//!
//! Both write an empty schema over an existing document, so they are pinned here
//! with both same-named types reachable.

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

/// Two `Outer.java` files in different packages, each with a nested `Request`,
/// both addressed by their fully qualified name.
#[test]
fn two_same_named_outer_classes_in_different_packages() {
    let files = vec![
        (
            "src/com/web/AController.java",
            "package com.web;\n\n@RestController\npublic class AController {\n    @PostMapping(\"/a\")\n    public com.alpha.Outer.Request save(@RequestBody com.alpha.Outer.Request form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/web/BController.java",
            "package com.web;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public com.beta.Outer.Request save(@RequestBody com.beta.Outer.Request form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/alpha/Outer.java",
            "package com.alpha;\n\npublic class Outer {\n    public static class Request {\n        private String alpha;\n    }\n}\n".to_owned(),
        ),
        (
            "src/com/beta/Outer.java",
            "package com.beta;\n\npublic class Outer {\n    public static class Request {\n        private String beta;\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let a = schema(&scanned, "/a", "req_body_other");
    let b = schema(&scanned, "/b", "req_body_other");
    println!("/a -> {a}");
    println!("/b -> {b}");
    assert!(a.contains("alpha"), "/a is com.alpha.Outer.Request: {a}");
    assert!(!a.contains("beta"), "/a must not use the beta one: {a}");
    assert!(b.contains("beta"), "/b is com.beta.Outer.Request: {b}");
    assert!(!b.contains("alpha"), "/b must not use the alpha one: {b}");
}

/// A nested type imported directly — `import com.alpha.AlphaOuter.Request` — and
/// then written bare. The alias has to use the same identity the scan stores.
#[test]
fn a_directly_imported_nested_type() {
    let files = vec![
        (
            "src/com/web/AController.java",
            "package com.web;\n\nimport com.alpha.AlphaOuter.Request;\n\n@RestController\npublic class AController {\n    @PostMapping(\"/a\")\n    public Request save(@RequestBody Request form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/web/BController.java",
            "package com.web;\n\nimport com.beta.BetaOuter.Request;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public Request save(@RequestBody Request form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/alpha/AlphaOuter.java",
            "package com.alpha;\n\npublic class AlphaOuter {\n    public static class Request {\n        private String alpha;\n    }\n}\n".to_owned(),
        ),
        (
            "src/com/beta/BetaOuter.java",
            "package com.beta;\n\npublic class BetaOuter {\n    public static class Request {\n        private String beta;\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let a = schema(&scanned, "/a", "req_body_other");
    let b = schema(&scanned, "/b", "req_body_other");
    println!("/a -> {a}");
    println!("/b -> {b}");
    assert!(a.contains("alpha"), "/a imports com.alpha.AlphaOuter.Request: {a}");
    assert!(!a.contains("beta"), "/a must not use the beta one: {a}");
    assert!(b.contains("beta"), "/b imports com.beta.BetaOuter.Request: {b}");
    assert!(!b.contains("alpha"), "/b must not use the alpha one: {b}");
}
