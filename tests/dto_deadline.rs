//! A DTO closure that stops on its deadline must not be exported as if it were
//! the whole document.
//!
//! The scan has one wall-clock budget shared by the route pass and the DTO
//! pass. When the route pass is fast and the DTO pass is slow — many types, or
//! a few expensive host calls — the closure can give up *after* the routes are
//! complete. That used to be invisible: `resolve_dtos` broke out of its loop and
//! returned the partial `DtoIndex`, `ScanResult::truncated` stayed `false`
//! because that flag only ever described the route search, and `start_export`
//! turned the partial graph into schemas.
//!
//! The damage is in `save_api`: it updates an existing API document by
//! `method + path`, so a schema missing `detail.properties.message` replaces a
//! correct document with a poorer one, and the run still reports success. These
//! tests pin the three properties that prevent it:
//!
//! 1. `ScanResult::dto_complete` is `false` when the closure stopped early, and
//!    `true` when it finished — a route-only scan counts as complete.
//! 2. `start_export` refuses before issuing a single request, so no document is
//!    overwritten.
//! 3. The partial result is not cached as a full scan, so a retry does real work
//!    instead of replaying the damaged graph.
//!
//! **Why the deadline is driven from the search hook**: `MockHost` answers
//! `read_file` from a hash map, so a real project's slow I/O cannot be
//! reproduced by waiting inside the scan. Instead the *search* hook sleeps, and
//! the budget is set to expire after the route pass has read its controllers but
//! before the DTO pass can finish. That places the stop exactly where the bug
//! was, without depending on machine speed.

use std::cell::RefCell;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use openidea_apimocktle::{ApiMocktle, scan};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{
    Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch,
};

const ROOT: &str = "/proj";

/// Text searches issued so far, so a test can tell a re-scan from a cache hit.
type SearchCounter = Rc<Cell<usize>>;
/// Every request the mocked server received, in order.
type Recorded = Rc<RefCell<Vec<HttpRequest>>>;

/// A controller whose request and response both mention `Root`, so the closure
/// has to resolve `Root` and then `Detail` behind it.
const CONTROLLER: &str = r#"
package com.acme;

@RestController
@RequestMapping("/api/review")
public class ReviewController {
    @PostMapping("/review")
    public Root save(@RequestBody Root value) { return null; }
}
"#;

const ROOT_MODEL: &str = r#"
package com.acme;

public class Root {
    private Detail detail;
}
"#;

const DETAIL: &str = r#"
package com.acme;

public class Detail {
    private String message;
}
"#;

/// Every `.java` file in the fixture project, keyed by project path.
fn files() -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    files.insert(format!("{ROOT}/src/ReviewController.java"), CONTROLLER.to_owned());
    files.insert(format!("{ROOT}/src/Root.java"), ROOT_MODEL.to_owned());
    files.insert(format!("{ROOT}/src/Detail.java"), DETAIL.to_owned());
    files
}

/// A project whose searches take `pause` each, plus a server that records every
/// request it is sent.
///
/// The pause is what makes the DTO pass miss its deadline: the route pass issues
/// one search; every DTO type the index cannot answer costs another. With the
/// budget sized to expire during those, the closure stops exactly where the bug
/// was.
fn host(pause: Duration) -> (MockHost, Recorded) {
    host_with_counter(pause).0
}

/// The same fixture, plus the number of text searches issued so far.
fn host_with_counter(pause: Duration) -> ((MockHost, Recorded), SearchCounter) {
    let host = MockHost::install()
        .with_capabilities(&["project.read", "storage", "net:localhost", "net:127.0.0.1"]);
    host.set_locale("en");
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    let indexed = files();
    for (path, text) in &indexed {
        host.add_file(path, text);
    }
    host.set_setting("serverUrl", "\"http://localhost:4000\"");
    host.set_setting("token", "\"secret-token\"");
    host.set_setting("schemaDepth", "2");
    // The DTOs are declared in the project, so no Maven repository is involved.
    host.set_setting("resolveDependencyJars", "false");

    let searches: SearchCounter = Rc::new(std::cell::Cell::new(0));
    let counted = searches.clone();
    let snapshot = indexed.clone();
    host.on_search(move |query: &TextQuery, _limit| {
        counted.set(counted.get() + 1);
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
        let declaration = declaration_name(&query.pattern);
        let mut matches = Vec::new();
        for (path, text) in &snapshot {
            if !path.ends_with(".java") {
                continue;
            }
            let hit = match &declaration {
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

    let requests: Recorded = Rc::new(RefCell::new(Vec::new()));
    let seen = requests.clone();
    host.on_fetch(move |request: &HttpRequest| -> Result<HttpResponse, Error> {
        seen.borrow_mut().push(request.clone());
        let url = request.url.as_str();
        let body = if url.contains("/api/project/list") {
            serde_json::json!({ "errcode": 0, "data": [{ "_id": "p1", "name": "Demo" }] })
        } else if url.contains("/api/interface/getCatMenu") {
            serde_json::json!({ "errcode": 0, "data": [] })
        } else if url.contains("/api/interface/add_cat") {
            serde_json::json!({ "errcode": 0, "data": { "_id": "c1", "name": "ReviewController" } })
        } else if url.contains("/api/interface/list_cat") {
            // The endpoint already exists on the server, so a save would update
            // it — which is exactly the damage a partial schema would do.
            serde_json::json!({
                "errcode": 0,
                "data": { "list": [{ "_id": "42", "method": "POST", "path": "/api/review/review" }] }
            })
        } else if url.contains("/api/interface/save") {
            serde_json::json!({ "errcode": 0 })
        } else {
            serde_json::json!({ "errcode": 404, "errmsg": "not found" })
        };
        Ok(HttpResponse { status: 200, headers: Vec::new(), body: body.to_string().into_bytes() })
    });
    ((host, requests), searches)
}

/// The `(class|interface|record)\s+Name\b` shape the type index falls back to.
fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// A budget that expires after the route pass but before the DTO pass finishes.
///
/// The route pass issues one search; resolving `Root` and `Detail` costs two
/// more (the index cannot answer either, because their files are named after
/// the classes only by luck — `Root.java` does answer `Root`; the fallback is
/// what a renamed file would need). Three pauses are therefore enough to spend
/// the budget mid-closure, and the margin keeps the test from flipping on a
/// fast machine.
fn dto_phase_budget(pause: Duration) -> Duration {
    pause + pause / 2
}

#[test]
fn a_dto_closure_that_misses_its_deadline_is_reported_as_incomplete() {
    let pause = Duration::from_millis(120);
    let (host, _) = host(pause);
    let project = host.invocation().project().expect("a project");
    let options = scan::Options { budget: Some(dto_phase_budget(pause)), ..scan::Options::default() };
    let scanned = scan::scan(&project, &options).expect("a deadline is not an error");

    // The routes are complete — that is what makes the bug silent.
    assert!(!scanned.truncated, "the route pass finished inside the budget");
    assert_eq!(scanned.endpoints.len(), 1, "the route is found");
    // ... but the DTO graph is not, and the result now says so.
    assert!(
        !scanned.dto_complete,
        "the closure stopped early, so `dto_complete` must be false (resolved {:?})",
        scanned.dto.fields.keys().collect::<Vec<_>>()
    );
}

#[test]
fn a_finished_dto_closure_is_reported_as_complete() {
    // The control: the same project with no deadline resolves both types. This
    // pins that `dto_complete` is not simply always false.
    let (host, _) = host(Duration::ZERO);
    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &scan::Options::default()).expect("a scan");

    assert!(scanned.dto_complete, "nothing was truncated");
    assert!(scanned.dto.fields.contains_key("Root"), "Root resolved: {:?}", scanned.dto.fields.keys().collect::<Vec<_>>());
    assert!(scanned.dto.fields.contains_key("Detail"), "Detail resolved through Root");
    let root = &scanned.dto.fields["Root"];
    assert_eq!(root[0].name, "detail");
}

#[test]
fn a_route_only_scan_is_complete_without_a_dto_graph() {
    // `resolve_dto: false` produces no graph at all by design. Export never uses
    // these options, and the dashboard must not report it as damage.
    let (host, _) = host(Duration::ZERO);
    let project = host.invocation().project().expect("a project");
    let options = scan::Options { resolve_dto: false, ..scan::Options::default() };
    let scanned = scan::scan(&project, &options).expect("a scan");

    assert!(scanned.dto_complete, "a route-only scan is complete for its purpose");
    assert!(scanned.dto.fields.is_empty(), "no DTO work was done");
}

#[test]
fn export_refuses_a_partial_dto_graph_and_writes_nothing() {
    // The regression the review reported: with the DTO pass cut short, export
    // used to send a `Root` schema whose `detail` was a bare object, updating
    // the existing document (id "42") and then reporting success.
    let pause = Duration::from_millis(120);
    let (host, requests) = host(pause);
    let mut plugin = ApiMocktle::default();

    // Force the same deadline the plugin's own entry point uses, but sized so
    // the closure is what runs out of time.
    let result = plugin.run_command_in_tests(
        "export-project",
        &host.invocation(),
        Some(dto_phase_budget(pause)),
    );

    let error = result.expect_err("export must refuse a partial DTO graph");
    let message = error.to_string();
    assert!(
        message.contains("DTO") || message.contains("not fully resolved"),
        "the message says why: {message}"
    );

    // Nothing was written, and nothing was even created: the refusal happens
    // before the project list is fetched, so no category, no save.
    let sent = requests.borrow();
    assert!(
        !sent.iter().any(|r| r.url.contains("/api/interface/save")),
        "no interface document was written: {:?}",
        sent.iter().map(|r| r.url.clone()).collect::<Vec<_>>()
    );
    assert!(
        !sent.iter().any(|r| r.url.contains("/api/interface/add_cat")),
        "no category was created either"
    );
}

#[test]
fn a_partial_graph_is_not_cached_as_a_full_scan() {
    // A partial graph that entered the full-scan cache would be replayed by every
    // later export in the same session: the second attempt would not even retry
    // the closure, so a transient deadline would poison the whole session.
    //
    // The observable difference: with the partial result cached, the second
    // export is refused from the cache immediately; without it, the second scan
    // runs again. Counting the marker searches tells them apart.
    let pause = Duration::from_millis(120);
    let ((host, _), searches) = host_with_counter(pause);
    let mut plugin = ApiMocktle::default();
    let first = plugin.run_command_in_tests(
        "export-project",
        &host.invocation(),
        Some(dto_phase_budget(pause)),
    );
    assert!(first.is_err(), "the short budget refuses");
    let after_first = searches.get();

    let second = plugin.run_command_in_tests(
        "export-project",
        &host.invocation(),
        Some(dto_phase_budget(pause)),
    );
    assert!(second.is_err(), "still refuses with the same budget");
    let after_second = searches.get();

    assert!(
        after_second > after_first,
        "the second attempt scanned again ({after_first} → {after_second} searches): \
         the partial result must not be cached as a full scan"
    );
}

#[test]
fn a_complete_graph_exports_and_keeps_the_nested_field() {
    // The control for the test above: with time to finish, the same project
    // exports, and the schema really carries `detail.properties.message`. If
    // this failed while the refusal test passed, the refusal would be covering
    // for a broken resolver.
    let (host, requests) = host(Duration::ZERO);
    let mut plugin = ApiMocktle::default();
    let text = plugin
        .run_command_in_tests("export-project", &host.invocation(), None)
        .expect("a complete scan exports");

    assert!(text.contains("synced"), "the command reports success: {text}");
    let saved = requests.borrow();
    let save = saved
        .iter()
        .find(|r| r.url.contains("/api/interface/save"))
        .expect("a save request");
    let doc: serde_json::Value =
        serde_json::from_slice(save.body.as_ref().expect("a body")).expect("valid json");
    let response: serde_json::Value =
        serde_json::from_str(doc["res_body"].as_str().expect("a schema")).expect("valid schema");
    assert_eq!(
        response["properties"]["detail"]["properties"]["message"]["type"], "string",
        "the nested field survived: {response}"
    );
}
