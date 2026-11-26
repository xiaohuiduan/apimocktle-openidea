//! The scan's own package derivation for a dotted reference.
//!
//! `com.beta.BetaOuter.Middle.Request` names package `com.beta` plus a class path.
//! `rsplit_once('.')` would take `com.beta.BetaOuter.Middle` as the package, which
//! is wrong; the candidate still resolved in the simpler tests because another step
//! answered first. These cases make the scan's fallback the deciding one.

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

/// The qualified type is only mentioned through a chain of `Holder` fields, so the
/// scan's own fallback — not the schema's exact lookup — has to find the file.
#[test]
fn a_deep_qualified_type_reached_through_fields() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/h\")\n    public Object save(@RequestBody Holder form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/web/Holder.java",
            "package com.web;\n\npublic class Holder {\n    private com.beta.BetaOuter.Middle.Request request;\n}\n".to_owned(),
        ),
        (
            "src/com/alpha/BetaOuter.java",
            "package com.alpha;\n\npublic class BetaOuter {\n    public static class Middle {\n        public static class Request {\n            private String alpha;\n        }\n    }\n}\n".to_owned(),
        ),
        (
            "src/com/beta/BetaOuter.java",
            "package com.beta;\n\npublic class BetaOuter {\n    public static class Middle {\n        public static class Request {\n            private String beta;\n        }\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let s = schema(&scanned, "/h", "req_body_other");
    println!("holder-deep -> {s}");
    assert!(s.contains("beta"), "com.beta.BetaOuter.Middle.Request has beta: {s}");
    assert!(!s.contains("alpha"), "must not use the alpha package: {s}");
}

/// The same, but the two outer classes live in same-named files.
#[test]
fn a_deep_qualified_type_in_same_named_files() {
    let files = vec![
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/h\")\n    public Object save(@RequestBody Holder form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/web/Holder.java",
            "package com.web;\n\npublic class Holder {\n    private com.beta.Outer.Middle.Request request;\n}\n".to_owned(),
        ),
        (
            "src/com/alpha/Outer.java",
            "package com.alpha;\n\npublic class Outer {\n    public static class Middle {\n        public static class Request {\n            private String alpha;\n        }\n    }\n}\n".to_owned(),
        ),
        (
            "src/com/beta/Outer.java",
            "package com.beta;\n\npublic class Outer {\n    public static class Middle {\n        public static class Request {\n            private String beta;\n        }\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let s = schema(&scanned, "/h", "req_body_other");
    println!("holder-deep-same-name -> {s}");
    assert!(s.contains("beta"), "com.beta.Outer.Middle.Request has beta: {s}");
    assert!(!s.contains("alpha"), "must not use the alpha package: {s}");
}
