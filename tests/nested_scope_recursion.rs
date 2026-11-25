//! Review round 2: recursion must resolve a field against the scope of the DTO
//! that declares it, not the scope of the endpoint.
//!
//! `AController.Request.detail` is `AController.Request.Detail`, and
//! `BController.Request.detail` is `BController.Request.Detail`. Both are spelled
//! `Detail` in the source. Resolving from the endpoint's scope only works for the
//! first level; inside `Request` the lookup has to continue from `Request`.

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

fn body_schema(scanned: &scan::ScanResult, path: &str) -> String {
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no endpoint for {path}"));
    let doc = yapi::api_doc(endpoint, &scanned.dto, 3, Lang::En);
    doc["req_body_other"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn a_nested_field_resolves_against_its_own_dto_scope() {
    let a = r#"
package com.acme;

@RestController
public class AController {
    @PostMapping("/a")
    public Request save(@RequestBody Request form) { return null; }

    public static class Request {
        private Detail detail;

        public static class Detail {
            private String alpha;
        }
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
        private Detail detail;

        public static class Detail {
            private String beta;
        }
    }
}
"#;
    let scanned = scan(&[("src/AController.java", a), ("src/BController.java", b)]);

    let a_schema = body_schema(&scanned, "/a");
    let b_schema = body_schema(&scanned, "/b");
    println!("/a -> {a_schema}");
    println!("/b -> {b_schema}");

    assert!(
        a_schema.contains("alpha"),
        "/a must use AController.Request.Detail.alpha: {a_schema}"
    );
    assert!(
        !a_schema.contains("beta"),
        "/a must not borrow BController's Detail: {a_schema}"
    );
    assert!(
        b_schema.contains("beta"),
        "/b must use BController.Request.Detail.beta: {b_schema}"
    );
    assert!(
        !b_schema.contains("alpha"),
        "/b must not borrow AController's Detail: {b_schema}"
    );
}

#[test]
fn a_third_level_of_nesting_still_resolves() {
    let src = r#"
package com.acme;

@RestController
public class Ctrl {
    @PostMapping("/x")
    public Request save(@RequestBody Request form) { return null; }

    public static class Request {
        private Middle middle;

        public static class Middle {
            private Deep deep;

            public static class Deep {
                private String value;
            }
        }
    }
}
"#;
    let scanned = scan(&[("src/Ctrl.java", src)]);
    let schema = body_schema(&scanned, "/x");
    println!("/x -> {schema}");
    assert!(schema.contains("\"value\""), "three levels deep: {schema}");
}
