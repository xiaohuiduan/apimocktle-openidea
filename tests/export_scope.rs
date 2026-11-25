//! What an export is allowed to refuse, and what it can do about it.
//!
//! The 0.3.1 release added a refusal: a DTO closure that stopped early produces
//! schemas with missing fields, and `save_api` *updates* a document by
//! `method + path`, so sending one would replace a correct document with a
//! poorer copy and still report success. That refusal is right — these tests
//! pin that it is now also *recoverable*, in the three ways 0.3.1 lacked:
//!
//! 1. **The export gets 60 seconds, not 5.** `host::begin_task` raises the call's
//!    wall clock and every `report` renews it, so the scan no longer has to stop
//!    at 4.2 s. A project that missed the old deadline finishes here.
//! 2. **A spent type cap is not reported as a timeout.** `TYPE_BUDGET` used to
//!    share the deadline's `false` and its "5-second limit" wording, so a project
//!    with more types than the cap was told to retry something that could never
//!    succeed. It now has its own reason and its own message.
//! 3. **"Export the current file or folder" really narrows the scan.** The path
//!    used to be applied to the endpoint list *after* a full-project scan, so the
//!    advice the refusal gives did nothing. `Options::only_path` reaches the
//!    scan, and the DTO closure then only covers the types those controllers
//!    reach.
//!
//! The third is the one a 900-DTO project depends on: with the cap reached, it
//! is the only way in that does not depend on machine speed.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use openidea_apimocktle::{ApiMocktle, scan};
use openidea_plugin::Plugin;
use openidea_plugin::mock::MockHost;
use openidea_plugin::{
    Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch,
};

const ROOT: &str = "/proj";

/// Text searches issued so far, so a cache hit can be told from a re-scan.
type SearchCounter = Rc<Cell<usize>>;
/// Every request the mocked server received, in order.
type Recorded = Rc<RefCell<Vec<HttpRequest>>>;

/// A controller per module, each returning its own DTO chain, so the closure of
/// one controller is a small slice of the project's types.
fn controller(module: usize) -> String {
    format!(
        r#"
package com.acme.m{module};

import com.acme.m{module}.dto.Dto{module}x0;
import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/m{module}")
public class Ctrl{module}Controller {{
    @GetMapping("/list")
    public Dto{module}x0 list() {{ return null; }}
}}
"#
    )
}

fn dto(module: usize, index: usize, chain: usize) -> String {
    let next = if index + 1 < chain {
        format!("    private Dto{module}x{} next;\n", index + 1)
    } else {
        String::new()
    };
    format!(
        r#"package com.acme.m{module}.dto;

public class Dto{module}x{index} {{
    private String name{index};
    private long seq{index};
{next}}}
"#
    )
}

/// Every file in the fixture, keyed by project path, plus the path of each
/// controller. Types per controller form separate chains, so a narrowed scan
/// reaches one chain and a full scan reaches all of them.
fn files(modules: usize, chain: usize) -> (BTreeMap<String, String>, Vec<String>) {
    let mut files = BTreeMap::new();
    let mut controllers = Vec::new();
    for m in 0..modules {
        let controller_path = format!("{ROOT}/src/m{m}/Ctrl{m}Controller.java");
        files.insert(controller_path.clone(), controller(m));
        controllers.push(controller_path);
        for i in 0..chain {
            files.insert(format!("{ROOT}/src/m{m}/dto/Dto{m}x{i}.java"), dto(m, i, chain));
        }
    }
    (files, controllers)
}

/// A project of `modules` controllers with `chain` DTOs each, plus a recording
/// server. `pause` is how long each text search takes, which is how a test puts
/// the scan's wall clock under its own control — `MockHost::read_file` is a hash
/// lookup, so real I/O speed cannot be reproduced by waiting inside the scan.
fn host(modules: usize, chain: usize, pause: Duration) -> ((MockHost, Recorded), SearchCounter) {
    let host = MockHost::install()
        .with_capabilities(&["project.read", "storage", "net:localhost", "net:127.0.0.1"]);
    host.set_locale("en");
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    let (indexed, controllers) = files(modules, chain);
    for (path, text) in &indexed {
        host.add_file(path, text);
    }
    host.set_setting("serverUrl", "\"http://localhost:4000\"");
    host.set_setting("token", "\"secret-token\"");
    host.set_setting("schemaDepth", "2");
    host.set_setting("resolveDependencyJars", "false");

    let searches: SearchCounter = Rc::new(Cell::new(0));
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
    // Kept alive so the paths are still reachable from a narrowed assertion.
    assert_eq!(controllers.len(), modules, "one controller per module");

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
            serde_json::json!({ "errcode": 0, "data": { "_id": "c1", "name": "Ctrl" } })
        } else if url.contains("/api/interface/list_cat") {
            serde_json::json!({ "errcode": 0, "data": { "list": [] } })
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

/// The saves the server received, as parsed documents.
fn saved(requests: &Recorded) -> Vec<serde_json::Value> {
    requests
        .borrow()
        .iter()
        .filter(|r| r.url.contains("/api/interface/save"))
        .filter_map(|r| r.body.as_ref().and_then(|b| serde_json::from_slice(b).ok()))
        .collect()
}

#[test]
fn a_narrowed_scan_only_reaches_the_types_of_its_own_controllers() {
    // The property the scoped export depends on: `only_path` has to reach the
    // scan, not just filter the endpoint list afterwards. With three modules of
    // four DTOs each, the narrowed closure reaches 4 types, not 12.
    let ((host, _), _) = host(3, 4, Duration::ZERO);
    let project = host.invocation().project().expect("a project");

    let full = scan::scan(&project, &scan::Options::default()).expect("a full scan");
    assert_eq!(full.endpoints.len(), 3, "every controller is found");
    assert_eq!(full.dto.fields.len(), 12, "the full closure reaches all 12 types");
    assert!(full.dto_complete(), "nothing was truncated");

    let narrow_path = format!("{ROOT}/src/m1/Ctrl1Controller.java");
    let narrowed = scan::scan(
        &project,
        &scan::Options { only_path: Some(narrow_path.clone()), ..scan::Options::default() },
    )
    .expect("a narrowed scan");

    assert_eq!(narrowed.endpoints.len(), 1, "one controller");
    assert_eq!(narrowed.endpoints[0].file, narrow_path, "the right one");
    assert_eq!(
        narrowed.dto.fields.len(),
        4,
        "only the four types that controller reaches, not all twelve: {:?}",
        narrowed.dto.fields.keys().collect::<Vec<_>>()
    );
    assert!(
        narrowed.dto.fields.keys().all(|name| name.starts_with("Dto1x")),
        "no type from another module leaked in: {:?}",
        narrowed.dto.fields.keys().collect::<Vec<_>>()
    );
    assert!(narrowed.dto_complete(), "the smaller closure finished");
}

#[test]
fn a_narrowed_scan_of_a_folder_covers_every_controller_under_it() {
    // A folder selection is the other half of the same command.
    let ((host, _), _) = host(3, 4, Duration::ZERO);
    let project = host.invocation().project().expect("a project");
    let narrowed = scan::scan(
        &project,
        &scan::Options {
            only_path: Some(format!("{ROOT}/src/m1")),
            ..scan::Options::default()
        },
    )
    .expect("a narrowed scan");
    assert_eq!(narrowed.endpoints.len(), 1, "the folder holds one controller");
    assert_eq!(narrowed.dto.fields.len(), 4, "and only its types");
}

#[test]
fn a_spent_type_cap_is_reported_as_a_cap_not_as_a_timeout() {
    // The 0.3.1 regression this pins: `TYPE_BUDGET` and the deadline shared one
    // `false`, so a project with more types than the cap was told it had missed
    // "the 5-second limit" — while no clock had been read at all, and retrying
    // stopped at the same type every time.
    let ((host, _), _) = host(3, 4, Duration::ZERO);
    let project = host.invocation().project().expect("a project");

    // A cap of zero leaves types queued without any clock involvement.
    let scanned = scan::scan(
        &project,
        &scan::Options { type_budget: Some(0), ..scan::Options::default() },
    )
    .expect("a scan");
    assert_eq!(scanned.endpoints.len(), 3, "the routes are all there");
    assert!(
        !scanned.dto_complete(),
        "the graph stopped at the cap, so it is partial"
    );
    assert_eq!(
        scanned.dto_stop,
        Some(scan::DtoStop::TypeBudget),
        "the reason is the cap, not the deadline"
    );
}

#[test]
fn a_deadline_is_reported_as_a_deadline() {
    // The control for the test above: the two reasons are distinguishable.
    let pause = Duration::from_millis(60);
    let ((host, _), _) = host(3, 4, pause);
    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(
        &project,
        &scan::Options { budget: Some(pause / 2), ..scan::Options::default() },
    )
    .expect("a deadline is not an error");
    assert_eq!(
        scanned.dto_stop,
        Some(scan::DtoStop::Deadline),
        "the clock ran out, not the cap"
    );
}

#[test]
fn export_refuses_a_deadline_with_the_retry_wording() {
    // The marker search alone costs `pause`, so a budget below it is already
    // spent when the closure would start — the same `DtoStop::Deadline` the
    // slow-search tests used to produce, without depending on the closure
    // issuing searches (it no longer does: the fallback reads files instead).
    let pause = Duration::from_millis(60);
    let ((host, _), _) = host(3, 4, pause);
    let mut plugin = ApiMocktle::default();
    let result = plugin.run_command_in_tests(
        "export-project",
        &host.invocation(),
        Some(pause / 2),
    );
    let error = result.expect_err("a partial graph is refused").to_string();
    assert!(
        error.contains("time limit") || error.contains("限时"),
        "the deadline message: {error}"
    );
}

#[test]
fn export_refuses_a_type_cap_with_the_cap_wording() {
    // The message has to differ: "retry" is wrong advice for a fixed cap.
    let ((host, _), _) = host(3, 4, Duration::ZERO);
    let mut plugin = ApiMocktle::default();
    plugin.set_type_budget_for_tests(Some(0));
    let result = plugin.run_command_in_tests("export-project", &host.invocation(), None);
    let error = result.expect_err("a partial graph is refused").to_string();
    assert!(
        !error.contains("time limit") && !error.contains("限时"),
        "a cap is not a timeout: {error}"
    );
    assert!(
        error.contains("more DTO types than a single scan") || error.contains("超过了单次扫描的上限"),
        "the cap message says what it is: {error}"
    );
}

#[test]
fn the_cap_message_does_not_claim_the_five_second_limit() {
    // The wording the report quoted. It must not survive on this path.
    let ((host, _), _) = host(3, 4, Duration::ZERO);
    let mut plugin = ApiMocktle::default();
    plugin.set_type_budget_for_tests(Some(0));
    let error = plugin
        .run_command_in_tests("export-project", &host.invocation(), None)
        .expect_err("refused")
        .to_string();
    assert!(!error.contains("5 秒"), "no five-second claim: {error}");
    assert!(!error.contains("5-second"), "no five-second claim: {error}");
}

#[test]
fn export_file_narrows_a_project_that_cannot_be_exported_whole() {
    // The advice the refusal gives, followed to the end: the whole project is
    // refused, the chosen file works.
    //
    // The cap drives it rather than the clock because `MockHost::read_file` is a
    // hash lookup — a narrowed scan costs the same wall clock there, so the mock
    // cannot express "less work, less time". The cap is the honest stand-in and
    // the more common real failure besides: a project whose full closure reaches
    // more types than one scan resolves is refused identically at any speed, and
    // narrowing is the only way in that does not depend on the machine.
    let ((host, requests), _) = host(3, 4, Duration::ZERO);
    let mut plugin = ApiMocktle::default();
    let ctx = host.invocation();

    // 12 types in the project, 6 allowed: the full closure stops at the cap.
    plugin.set_type_budget_for_tests(Some(6));
    let whole = plugin.run_command_in_tests("export-project", &ctx, None);
    assert!(whole.is_err(), "the whole project exceeds the cap");
    assert!(saved(&requests).is_empty(), "nothing was sent");

    // One controller reaches 4 types, under the same cap, so it exports.
    let arg = format!("{ROOT}/src/m1/Ctrl1Controller.java");
    let text = plugin
        .run_command("export-file", &ctx, Some(arg.as_str()))
        .expect("the narrowed export runs")
        .expect("a result");
    assert!(text.contains("synced"), "it reports success: {text}");

    let docs = saved(&requests);
    assert_eq!(docs.len(), 1, "one document, for the one controller: {docs:?}");
    assert_eq!(
        docs[0]["path"], "/api/m1/list",
        "and it is that controller's route, not another module's"
    );
}

#[test]
fn a_narrowed_scan_is_not_cached_as_the_whole_project() {
    // A narrowed result describes one corner of the project. Handing it back as
    // the full scan would silently drop every other controller's endpoints — so
    // a full export after a narrowed one must still see all three controllers.
    let pause = Duration::from_millis(60);
    let ((host, requests), _) = host(3, 4, pause);
    let mut plugin = ApiMocktle::default();
    let ctx = host.invocation();
    let arg = format!("{ROOT}/src/m1/Ctrl1Controller.java");
    plugin
        .run_command("export-file", &ctx, Some(arg.as_str()))
        .expect("the narrowed export runs");
    let after_narrow = saved(&requests).len();
    assert_eq!(after_narrow, 1, "one document from the narrowed export");

    // A full export with a budget that lets the whole project finish: it has to
    // scan again and see every controller, not reuse the narrowed list.
    let text = plugin
        .run_command_in_tests("export-project", &ctx, None)
        .expect("the full export runs")
        .clone();
    assert!(text.contains("synced"), "it reports success: {text}");
    let docs = saved(&requests);
    assert_eq!(
        docs.len(),
        4,
        "one narrowed save plus three from the full scan: {:?}",
        docs.iter().map(|d| d["path"].clone()).collect::<Vec<_>>()
    );
    let mut paths: Vec<&str> = docs.iter().filter_map(|d| d["path"].as_str()).collect();
    paths.sort();
    paths.dedup();
    assert_eq!(
        paths,
        vec!["/api/m0/list", "/api/m1/list", "/api/m2/list"],
        "every module is visible again, not just the narrowed one"
    );
}

#[test]
fn an_export_opens_a_task_so_the_scan_is_not_on_the_five_second_clock() {
    // The mechanism that makes a big project exportable at all: the call raises
    // its own wall clock through `host::begin_task`, and reports progress while
    // the closure runs so the host keeps renewing it.
    let ((host, _), _) = host(3, 4, Duration::ZERO);
    let mut plugin = ApiMocktle::default();
    plugin
        .run_command_in_tests("export-project", &host.invocation(), None)
        .expect("the export runs");

    // Two tasks: the scan opens one, and the send opens its own so a whole
    // project fits in a single call instead of 200 interfaces per minute.
    let tasks = host.tasks();
    assert_eq!(tasks.len(), 2, "a scan task and a send task: {tasks:?}");
    for task in &tasks {
        assert!(
            task.title.contains("ApiMocktle"),
            "each task names the plugin: {}",
            task.title
        );
        assert!(
            !task.reports.is_empty(),
            "progress was reported, which is what renews the budget: {task:?}"
        );
        assert!(task.ended, "both tasks were dropped when their work finished");
    }
    assert!(
        tasks.iter().any(|t| t.title.contains("同步") || t.title.contains("syncing")),
        "one of them is the send task: {:?}",
        tasks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>()
    );
}
