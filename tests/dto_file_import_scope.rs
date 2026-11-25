//! Review round 4: a DTO that lives in its own file has its own imports, and its
//! fields must resolve against those rather than against the controller's.

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

fn six_file_project() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "src/com/web/AController.java",
            "package com.web;\n\nimport com.alpha.AlphaRequest;\n\n@RestController\npublic class AController {\n    @PostMapping(\"/a\")\n    public AlphaRequest save(@RequestBody AlphaRequest form) { return null; }\n}\n",
        ),
        (
            "src/com/web/BController.java",
            "package com.web;\n\nimport com.beta.BetaRequest;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public BetaRequest save(@RequestBody BetaRequest form) { return null; }\n}\n",
        ),
        (
            "src/com/alpha/AlphaRequest.java",
            "package com.alpha;\n\nimport com.detailalpha.Detail;\n\npublic class AlphaRequest {\n    private Detail detail;\n}\n",
        ),
        (
            "src/com/beta/BetaRequest.java",
            "package com.beta;\n\nimport com.detailbeta.Detail;\n\npublic class BetaRequest {\n    private Detail detail;\n}\n",
        ),
        (
            "src/com/detailalpha/Detail.java",
            "package com.detailalpha;\n\npublic class Detail {\n    private String alpha;\n}\n",
        ),
        (
            "src/com/detailbeta/Detail.java",
            "package com.detailbeta;\n\npublic class Detail {\n    private String beta;\n}\n",
        ),
    ]
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

#[test]
fn a_dto_in_its_own_file_resolves_through_that_file_import() {
    let scanned = scan(&six_file_project());
    let a_req = schema(&scanned, "/a", "req_body_other");
    let b_req = schema(&scanned, "/b", "req_body_other");
    println!("/a req -> {a_req}");
    println!("/b req -> {b_req}");

    assert!(a_req.contains("alpha"), "/a uses com.detailalpha.Detail: {a_req}");
    assert!(!a_req.contains("beta"), "/a must not get beta: {a_req}");
    assert!(b_req.contains("beta"), "/b uses com.detailbeta.Detail: {b_req}");
    assert!(!b_req.contains("alpha"), "/b must not get alpha: {b_req}");
}

#[test]
fn the_response_schema_follows_the_same_import() {
    let scanned = scan(&six_file_project());
    let a_res = schema(&scanned, "/a", "res_body");
    let b_res = schema(&scanned, "/b", "res_body");
    println!("/a res -> {a_res}");
    println!("/b res -> {b_res}");
    assert!(a_res.contains("alpha") && !a_res.contains("beta"), "/a res: {a_res}");
    assert!(b_res.contains("beta") && !b_res.contains("alpha"), "/b res: {b_res}");
}

#[test]
fn one_controller_with_its_own_nested_detail_still_works() {
    let src = r#"
package com.acme;

@RestController
public class Ctrl {
    @PostMapping("/x")
    public Request save(@RequestBody Request form) { return null; }

    public static class Request {
        private Detail detail;

        public static class Detail {
            private String value;
        }
    }
}
"#;
    let scanned = scan(&[("src/Ctrl.java", src)]);
    let req = schema(&scanned, "/x", "req_body_other");
    assert!(req.contains("\"value\""), "nested resolution still works: {req}");
}
