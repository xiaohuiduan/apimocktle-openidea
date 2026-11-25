//! Review round 5 on PR 363: a member path deeper than one class.
//!
//! `com.beta.BetaOuter.Middle.Request` is declared in `BetaOuter.java` and stored
//! as `com.beta::BetaOuter::Middle::Request`. Both the file lookup and the import
//! alias assumed a single class level, so the beta document was overwritten.

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

struct Names<'a> {
    a_import: Option<&'a str>,
    b_import: Option<&'a str>,
    a_ty: &'a str,
    b_ty: &'a str,
    alpha_file: &'static str,
    beta_file: &'static str,
    alpha_outer: &'a str,
    beta_outer: &'a str,
}

fn two_level_project(n: Names<'_>) -> Vec<(&'static str, String)> {
    let imports = |i: Option<&str>| i.map(|v| format!("import {v};\n\n")).unwrap_or_default();
    let (a_ty, b_ty) = (n.a_ty, n.b_ty);
    let (alpha_outer, beta_outer) = (n.alpha_outer, n.beta_outer);
    vec![
        (
            "src/com/web/AController.java",
            format!(
                "package com.web;\n\n{}@RestController\npublic class AController {{\n    @PostMapping(\"/a\")\n    public {a_ty} save(@RequestBody {a_ty} form) {{ return null; }}\n}}\n",
                imports(n.a_import)
            ),
        ),
        (
            "src/com/web/BController.java",
            format!(
                "package com.web;\n\n{}@RestController\npublic class BController {{\n    @PostMapping(\"/b\")\n    public {b_ty} save(@RequestBody {b_ty} form) {{ return null; }}\n}}\n",
                imports(n.b_import)
            ),
        ),
        (
            n.alpha_file,
            format!(
                "package com.alpha;\n\npublic class {alpha_outer} {{\n    public static class Middle {{\n        public static class Request {{\n            private String alpha;\n        }}\n    }}\n}}\n"
            ),
        ),
        (
            n.beta_file,
            format!(
                "package com.beta;\n\npublic class {beta_outer} {{\n    public static class Middle {{\n        public static class Request {{\n            private String beta;\n        }}\n    }}\n}}\n"
            ),
        ),
    ]
}

fn check(scanned: &scan::ScanResult, context: &str) {
    let a = schema(scanned, "/a", "req_body_other");
    let b = schema(scanned, "/b", "req_body_other");
    let a_res = schema(scanned, "/a", "res_body");
    let b_res = schema(scanned, "/b", "res_body");
    println!("{context}: /a={a} /b={b}");
    assert!(a.contains("alpha"), "{context}: /a must be the alpha type: {a}");
    assert!(!a.contains("beta"), "{context}: /a must not use beta: {a}");
    assert!(b.contains("beta"), "{context}: /b must be the beta type: {b}");
    assert!(!b.contains("alpha"), "{context}: /b must not use alpha: {b}");
    assert!(a_res.contains("alpha"), "{context}: /a response: {a_res}");
    assert!(b_res.contains("beta"), "{context}: /b response: {b_res}");
}

/// Direct imports of a two-level nested type.
#[test]
fn directly_imported_two_level_members() {
    let files = two_level_project(Names {
        a_import: Some("com.alpha.AlphaOuter.Middle.Request"),
        b_import: Some("com.beta.BetaOuter.Middle.Request"),
        a_ty: "Request",
        b_ty: "Request",
        alpha_file: "src/com/alpha/AlphaOuter.java",
        beta_file: "src/com/beta/BetaOuter.java",
        alpha_outer: "AlphaOuter",
        beta_outer: "BetaOuter",
    });
    let scanned = scan(files);
    check(&scanned, "import");
}

/// The same types written fully qualified, with distinct outer file names.
#[test]
fn fully_qualified_two_level_members() {
    let files = two_level_project(Names {
        a_import: None,
        b_import: None,
        a_ty: "com.alpha.AlphaOuter.Middle.Request",
        b_ty: "com.beta.BetaOuter.Middle.Request",
        alpha_file: "src/com/alpha/AlphaOuter.java",
        beta_file: "src/com/beta/BetaOuter.java",
        alpha_outer: "AlphaOuter",
        beta_outer: "BetaOuter",
    });
    let scanned = scan(files);
    check(&scanned, "qualified");
}

/// Both outer classes named `Outer`, which is what the review also exercised.
#[test]
fn fully_qualified_two_level_members_in_same_named_files() {
    let files = two_level_project(Names {
        a_import: None,
        b_import: None,
        a_ty: "com.alpha.Outer.Middle.Request",
        b_ty: "com.beta.Outer.Middle.Request",
        alpha_file: "src/com/alpha/Outer.java",
        beta_file: "src/com/beta/Outer.java",
        alpha_outer: "Outer",
        beta_outer: "Outer",
    });
    let scanned = scan(files);
    check(&scanned, "qualified-same-name");
}
