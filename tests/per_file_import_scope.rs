//! Review round 3: each source file's own imports must decide which same-named
//! top-level DTO it means.
//!
//! `com.web.AController` imports `com.alpha.Payload`; `com.web.BController` imports
//! `com.beta.Payload`. Both are separate top-level classes, so only the per-file
//! import says which one a `@RequestBody Payload` refers to.

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
fn each_file_resolves_through_its_own_import() {
    let files = [
        (
            "src/com/alpha/Payload.java",
            "package com.alpha;\n\npublic class Payload {\n    private String alpha;\n}\n",
        ),
        (
            "src/com/beta/Payload.java",
            "package com.beta;\n\npublic class Payload {\n    private String beta;\n}\n",
        ),
        (
            "src/com/web/AController.java",
            r#"
package com.web;

import com.alpha.Payload;

@RestController
public class AController {
    @PostMapping("/a")
    public Payload save(@RequestBody Payload form) { return null; }
}
"#,
        ),
        (
            "src/com/web/BController.java",
            r#"
package com.web;

import com.beta.Payload;

@RestController
public class BController {
    @PostMapping("/b")
    public Payload save(@RequestBody Payload form) { return null; }
}
"#,
        ),
    ];
    let scanned = scan(&files);

    let a_schema = body_schema(&scanned, "/a");
    let b_schema = body_schema(&scanned, "/b");
    println!("/a -> {a_schema}");
    println!("/b -> {b_schema}");

    assert!(a_schema.contains("alpha"), "/a imports com.alpha.Payload: {a_schema}");
    assert!(!a_schema.contains("beta"), "/a must not get beta: {a_schema}");
    assert!(b_schema.contains("beta"), "/b imports com.beta.Payload: {b_schema}");
    assert!(!b_schema.contains("alpha"), "/b must not get alpha: {b_schema}");
}
