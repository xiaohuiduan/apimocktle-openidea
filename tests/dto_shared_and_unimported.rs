//! A probe for a case the review rounds have not covered: one DTO file reached
//! from two endpoints in different packages, where only one of them imports it.
//!
//! The imported one must resolve through its import; the other has no import and
//! no local type, so its `Detail` is genuinely undecidable and must not silently
//! borrow the other endpoint's.

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

fn schema(scanned: &scan::ScanResult, path: &str) -> String {
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no endpoint for {path}"));
    let doc = yapi::api_doc(endpoint, &scanned.dto, 4, Lang::En);
    doc["req_body_other"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn a_shared_dto_resolves_the_same_from_both_endpoints() {
    // One DTO file, imported by two controllers in different packages. Both must
    // agree on what its `Detail` is, because the DTO's own file says so.
    let files = [
        (
            "src/com/web/AController.java",
            "package com.web;\n\nimport com.shared.Shared;\n\n@RestController\npublic class AController {\n    @PostMapping(\"/a\")\n    public Shared save(@RequestBody Shared form) { return null; }\n}\n",
        ),
        (
            "src/com/web/BController.java",
            "package com.web;\n\nimport com.shared.Shared;\n\n@RestController\npublic class BController {\n    @PostMapping(\"/b\")\n    public Shared save(@RequestBody Shared form) { return null; }\n}\n",
        ),
        (
            "src/com/shared/Shared.java",
            "package com.shared;\n\nimport com.detail.Detail;\n\npublic class Shared {\n    private Detail detail;\n}\n",
        ),
        (
            "src/com/detail/Detail.java",
            "package com.detail;\n\npublic class Detail {\n    private String value;\n}\n",
        ),
    ];
    let scanned = scan(&files);
    let a = schema(&scanned, "/a");
    let b = schema(&scanned, "/b");
    println!("/a -> {a}");
    println!("/b -> {b}");
    assert_eq!(a, b, "the same DTO reached twice resolves the same way");
    assert!(a.contains("\"value\""), "the shared DTO's field resolves: {a}");
}

#[test]
fn an_unimported_same_named_detail_is_not_borrowed_from_another_package() {
    // `com.web.Ctrl` names `Detail` with no import and no `Detail` in its package,
    // while two other packages declare one. Nothing decides which, so whichever is
    // picked, the point is that the schema is not built from a type the file never
    // referenced through an import — and the scan records the ambiguity.
    let files = [
        (
            "src/com/one/Detail.java",
            "package com.one;\n\npublic class Detail {\n    private String one;\n}\n",
        ),
        (
            "src/com/two/Detail.java",
            "package com.two;\n\npublic class Detail {\n    private String two;\n}\n",
        ),
        (
            "src/com/web/Holder.java",
            "package com.web;\n\nimport com.one.Detail;\n\npublic class Holder {\n    private Detail detail;\n}\n",
        ),
        (
            "src/com/web/Ctrl.java",
            "package com.web;\n\n@RestController\npublic class Ctrl {\n    @PostMapping(\"/h\")\n    public Holder save(@RequestBody Holder form) { return null; }\n}\n",
        ),
    ];
    let scanned = scan(&files);
    let h = schema(&scanned, "/h");
    println!("/h -> {h}");
    assert!(h.contains("\"one\""), "Holder imports com.one.Detail: {h}");
    assert!(!h.contains("\"two\""), "must not borrow com.two.Detail: {h}");
}
