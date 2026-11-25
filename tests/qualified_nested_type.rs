//! A fully qualified reference to a *nested* type, which is what a real project
//! writes as `MidjourneyApi.Notify` or `com.acme.Outer.Inner`.
//!
//! The scan keys such a type `com.acme::Outer::Inner`, so the dotted form has to
//! be mapped carefully: the package keeps its dots and only the boundary between
//! the package and the class becomes `::`.

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

/// `com.acme.Outer.Inner` written on the body, with a same-named `Inner` reachable
/// through another controller — the shape a real project has.
#[test]
fn a_qualified_nested_type_resolves_to_its_own_outer() {
    let files = vec![
        (
            "src/com/acme/Ctrl.java",
            "package com.acme;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/a\")\n    public Object save(@RequestBody com.acme.Outer.Inner form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/acme/OtherCtrl.java",
            "package com.acme;\n\n@RestController\npublic class OtherCtrl {\n    @PostMapping(\"/b\")\n    public com.acme.Other.Inner save(@RequestBody com.acme.Other.Inner form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/acme/Outer.java",
            "package com.acme;\n\npublic class Outer {\n    public static class Inner {\n        private String fromOuter;\n    }\n}\n".to_owned(),
        ),
        (
            "src/com/acme/Other.java",
            "package com.acme;\n\npublic class Other {\n    public static class Inner {\n        private String fromOther;\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let a = req(&scanned, "/a");
    let b = req(&scanned, "/b");
    println!("/a -> {a}");
    println!("/b -> {b}");
    assert!(a.contains("fromOuter"), "com.acme.Outer.Inner is the field's type: {a}");
    assert!(!a.contains("fromOther"), "must not use the other Outer: {a}");
    assert!(b.contains("fromOther"), "/b means com.acme.Other.Inner: {b}");
}

/// A field typed by a nested class of the *same* file, written fully qualified.
#[test]
fn a_qualified_nested_type_in_a_field_resolves() {
    let files = vec![
        (
            "src/com/acme/Ctrl.java",
            "package com.acme;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/h\")\n    public Holder save(@RequestBody Holder form) { return null; }\n}\n".to_owned(),
        ),
        (
            "src/com/acme/Holder.java",
            "package com.acme;\n\npublic class Holder {\n    private com.acme.Outer.Inner inner;\n}\n".to_owned(),
        ),
        (
            "src/com/acme/Outer.java",
            "package com.acme;\n\npublic class Outer {\n    public static class Inner {\n        private String fromOuter;\n    }\n}\n".to_owned(),
        ),
        (
            "src/com/acme/Other.java",
            "package com.acme;\n\npublic class Other {\n    public static class Inner {\n        private String fromOther;\n    }\n}\n".to_owned(),
        ),
    ];
    let scanned = scan(files);
    let h = req(&scanned, "/h");
    println!("/h -> {h}");
    assert!(h.contains("fromOuter"), "the qualified field is Outer.Inner: {h}");
    assert!(!h.contains("fromOther"), "must not use Other.Inner: {h}");
}
