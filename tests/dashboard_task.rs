//! The dashboard scans on a background task, so a real project's controller count
//! is not bounded by the host's five-second view budget.
//!
//! A real project needs one `read_file` per controller, and the host charges a
//! few milliseconds for each: a project with ~730 controllers spends over five
//! seconds in `read_file` alone. The plugin opens a task around the scan (the
//! same mechanism export uses), which raises the call to the host's 60-second
//! background allowance. These tests pin that mechanism, because `MockHost`
//! answers `read_file` from a hash map and cannot reproduce the cost that makes
//! it necessary — the only observable here is the task itself.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use openidea_apimocktle::ApiMocktle;
use openidea_plugin::mock::MockHost;
use openidea_plugin::views::Views;
use openidea_plugin::{
    Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch,
};

const ROOT: &str = "/proj";

type Recorded = Rc<RefCell<Vec<HttpRequest>>>;
type SearchCounter = Rc<Cell<usize>>;

fn controller(index: usize) -> String {
    format!(
        r#"
package com.acme.c{index};

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/c{index}")
public class Ctrl{index}Controller {{
    @GetMapping("/list")
    public String list() {{ return null; }}
}}
"#
    )
}

/// `count` controllers, one route each, plus a recording server.
fn host(count: usize) -> ((MockHost, Recorded), SearchCounter) {
    let host = MockHost::install()
        .with_capabilities(&["project.read", "storage", "net:localhost", "net:127.0.0.1"]);
    host.set_locale("en");
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.set_setting("serverUrl", "\"http://localhost:4000\"");
    host.set_setting("token", "\"t\"");
    host.set_setting("resolveDependencyJars", "false");

    let mut controllers = Vec::new();
    for i in 0..count {
        let path = format!("{ROOT}/src/Ctrl{i}Controller.java");
        host.add_file(&path, &controller(i));
        controllers.push(path);
    }

    let searches: SearchCounter = Rc::new(Cell::new(0));
    let counted = searches.clone();
    host.on_search(move |_q: &TextQuery, _limit| {
        counted.set(counted.get() + 1);
        Ok(TextSearch {
            matches: controllers
                .iter()
                .map(|p| TextMatch {
                    location: Location {
                        path: p.clone(),
                        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    },
                    line: String::new(),
                })
                .collect(),
            limit_hit: false,
        })
    });

    let requests: Recorded = Rc::new(RefCell::new(Vec::new()));
    let seen = requests.clone();
    host.on_fetch(move |request: &HttpRequest| -> Result<HttpResponse, Error> {
        seen.borrow_mut().push(request.clone());
        let body = if request.url.contains("/api/project/list") {
            serde_json::json!({ "errcode": 0, "data": [{ "_id": "p1", "name": "Demo" }] })
        } else {
            serde_json::json!({ "errcode": 0, "data": [] })
        };
        Ok(HttpResponse { status: 200, headers: Vec::new(), body: body.to_string().into_bytes() })
    });

    ((host, requests), searches)
}

#[test]
fn the_dashboard_scan_opens_a_task_so_a_big_project_fits_the_call() {
    // `MockHost` cannot make `read_file` slow, so what is pinned here is the
    // mechanism that makes a slow one survivable: the scan runs inside a task,
    // which is the only thing that raises the host's wall clock for the call.
    let ((host, _), _) = host(3);
    let mut plugin = ApiMocktle::default();
    plugin.render("dashboard", None, &host.invocation()).expect("render");
    plugin
        .run_view_message_in_tests(
            &host.invocation(),
            &serde_json::json!({ "op": "scan" }).to_string(),
            None,
        )
        .expect("the scan message");

    let tasks = host.tasks();
    assert_eq!(tasks.len(), 1, "exactly one background task: {tasks:?}");
    assert!(
        tasks[0].title.contains("ApiMocktle"),
        "the task names the plugin: {}",
        tasks[0].title
    );
    // More than the single opening report: the scan has to keep reporting while
    // it runs, because the host renews the task's allowance on every `report`.
    // A task that only reports once and then goes quiet is cut off like any
    // other call, so "at least one report" is not the property that matters.
    assert!(
        tasks[0].reports.len() > 1,
        "progress was reported during the scan, not only at the start: {:?}",
        tasks[0].reports
    );
    assert!(tasks[0].ended, "the task is closed once the scan is done");
}

#[test]
fn the_dashboard_scan_is_complete_when_the_budget_suffices() {
    // The control: the task does not change what the scan returns. Every
    // controller is in the payload, and nothing reports itself as cut short.
    let ((host, _), _) = host(3);
    let mut plugin = ApiMocktle::default();
    plugin.render("dashboard", None, &host.invocation()).expect("render");
    plugin
        .run_view_message_in_tests(
            &host.invocation(),
            &serde_json::json!({ "op": "scan" }).to_string(),
            None,
        )
        .expect("the scan message");

    let payload: serde_json::Value = host
        .view_messages()
        .into_iter()
        .filter_map(|(_, raw)| serde_json::from_str(&raw).ok())
        .find(|v: &serde_json::Value| v["op"] == "items")
        .expect("an items payload");

    assert_eq!(payload["count"], 3, "every controller is listed: {payload}");
    assert_eq!(payload["truncated"], false, "no search limit was hit");
    assert_eq!(payload["stoppedEarly"], false, "the budget was enough");
}

#[test]
fn a_budget_stop_is_reported_apart_from_the_search_limit() {
    // The two are different failures and the page words them differently: the
    // search limit is a fixed property of the project, while a budget stop means
    // the host cut the scan short and a retry on a quieter machine may do better.
    // With a deadline already expired the scan stops before reading anything, so
    // the flag has to come back rather than being folded into `truncated`.
    let ((host, _), _) = host(3);
    let mut plugin = ApiMocktle::default();
    plugin.render("dashboard", None, &host.invocation()).expect("render");
    plugin
        .run_view_message_in_tests(
            &host.invocation(),
            &serde_json::json!({ "op": "scan" }).to_string(),
            Some(Duration::ZERO),
        )
        .expect("the scan message");

    let payload: serde_json::Value = host
        .view_messages()
        .into_iter()
        .filter_map(|(_, raw)| serde_json::from_str(&raw).ok())
        .find(|v: &serde_json::Value| v["op"] == "items")
        .expect("an items payload");

    assert_eq!(payload["stoppedEarly"], true, "the budget was the reason: {payload}");
    assert_eq!(payload["truncated"], false, "the search limit was not the reason");
}
