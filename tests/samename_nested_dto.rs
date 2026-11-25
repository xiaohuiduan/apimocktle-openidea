//! Regression: two controllers declaring their own same-named nested DTO.
//!
//! Review finding on 0.3.2 — the DTO graph keyed types by simple name, so
//! `AController.Request` and `BController.Request` collapsed into one entry. The
//! endpoint whose declaration lost was built from the other's fields, and because
//! export updates by `method + path`, `POST /b` overwrote the server's correct
//! `beta` document with A's `alpha` while reporting success.
//!
//! Resolution is now scoped: a name is looked up from the endpoint's own
//! declaration outward, so each endpoint sees its own type.

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
        Ok(TextSearch { matches, limit_hit: false })
    });
    let project = host.invocation().project().expect("a project");
    scan::scan(&project, &Options::default()).expect("a scan")
}

fn request_schema(scanned: &scan::ScanResult, path: &str) -> String {
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no endpoint for {path}"));
    let doc = yapi::api_doc(endpoint, &scanned.dto, 2, Lang::En);
    doc["req_body_other"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn each_controller_resolves_its_own_same_named_nested_dto() {
    let a = r#"
package com.acme;

@RestController
public class AController {
    @PostMapping("/a")
    public Request save(@RequestBody Request form) { return null; }

    public static class Request {
        private String alpha;
    }
}
"#;
    let b = r#"
package com.acme;

@RestController
public class BController {
    @PostMapping("/b")
    public Request save(@RequestBody Request form) { return null; }

    public static class Request {
        private String beta;
    }
}
"#;
    let scanned = scan(&[("src/AController.java", a), ("src/BController.java", b)]);

    // The bug sent A's `alpha` for /b, overwriting the server's `beta`.
    let scheme_a = request_schema(&scanned, "/a");
    let scheme_b = request_schema(&scanned, "/b");
    assert!(scheme_a.contains("alpha"), "/a uses AController.Request: {scheme_a}");
    assert!(!scheme_a.contains("beta"), "/a must not borrow B's field: {scheme_a}");
    assert!(scheme_b.contains("beta"), "/b uses BController.Request: {scheme_b}");
    assert!(!scheme_b.contains("alpha"), "/b must not borrow A's field: {scheme_b}");
}

#[test]
fn a_nested_dto_still_resolves_when_only_one_type_has_that_name() {
    // The common case must keep working: scoping is a refinement, not a new
    // requirement on the project.
    let src = r#"
package com.acme;

@RestController
public class Ctrl {
    @PostMapping("/save")
    public Request save(@RequestBody Request form) { return null; }

    public static class Request {
        private String name;
        private Detail detail;
    }

    public static class Detail {
        private String message;
    }
}
"#;
    let scanned = scan(&[("src/Ctrl.java", src)]);
    let schema = request_schema(&scanned, "/save");
    assert!(schema.contains("\"name\""), "the field is there: {schema}");
    // The nested field resolves through its own scope as well.
    assert!(schema.contains("\"message\""), "the referenced inner type expanded: {schema}");
}

#[test]
fn a_same_named_dto_in_another_package_does_not_leak_across() {
    let a = r#"
package com.alpha;

@RestController
public class AlphaCtrl {
    @PostMapping("/alpha")
    public Payload get(@RequestBody Payload body) { return null; }

    public static class Payload {
        private String fromAlpha;
    }
}
"#;
    let b = r#"
package com.beta;

@RestController
public class BetaCtrl {
    @PostMapping("/beta")
    public Payload get(@RequestBody Payload body) { return null; }

    public static class Payload {
        private String fromBeta;
    }
}
"#;
    let scanned = scan(&[("src/com/alpha/AlphaCtrl.java", a), ("src/com/beta/BetaCtrl.java", b)]);
    let alpha = request_schema(&scanned, "/alpha");
    let beta = request_schema(&scanned, "/beta");
    assert!(alpha.contains("fromAlpha") && !alpha.contains("fromBeta"), "{alpha}");
    assert!(beta.contains("fromBeta") && !beta.contains("fromAlpha"), "{beta}");
}
