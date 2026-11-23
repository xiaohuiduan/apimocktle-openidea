//! The scan at real-project scale, as a unit test.
//!
//! `examples/scan_bench.rs` measures the big demo project and prints round-trip
//! counts, but that is a tool a person runs, not something CI fails on. This
//! file generates a project of the same shape in memory and asserts the two
//! properties the scan-timeout fix is supposed to hold at any size:
//!
//! 1. **The type index scales.** A scan must not issue one declaration search
//!    per DTO type. `MockHost` exposes a hook for `search_text` only, and the
//!    index's whole job is to keep that hook quiet: with the index working, the
//!    only searches are the marker search and — rarely — a fallback for a name
//!    no file declares. Reintroducing a per-type lookup makes the count jump by
//!    the number of DTOs, which is what the assertion below catches.
//! 2. **The budget guard holds.** A scan with a spent budget returns quickly
//!    with `truncated` set, instead of running on until the host reaps the
//!    instance.
//!
//! **What this cannot test**: the host's five-second wall clock. `MockHost` is
//! an in-memory fake — `read_file` and `find_files` are hash lookups, not disk
//! reads — so a passing run here says nothing about how many milliseconds a real
//! project takes. That is what `examples/scan_bench.rs` is for, and why the fix
//! was measured in round trips rather than seconds.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use openidea_apimocktle::scan::{self, Options};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{
    Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch,
};

const ROOT: &str = "/proj";
/// Controllers to generate. Chosen so the debug-build scan stays around a
/// second — big enough to expose anything that scales per type, small enough to
/// keep `cargo test` quick.
const CONTROLLERS: usize = 200;
/// DTOs to generate. Deliberately larger than `TYPE_BUDGET` (400)? No — at 400
/// the closure stops early and the test would not exercise a full sweep, so
/// this stays under it and the whole closure resolves.
const DTOS: usize = 380;
const METHODS_PER_CONTROLLER: usize = 4;

struct Fixture {
    files: Vec<(String, String)>,
    endpoints: usize,
}

/// A multi-module Spring project shaped like a real one: controllers that
/// return a shared `Result<T>`, DTOs that reference each other, route constants
/// referenced from the annotations, and a couple of Feign clients.
fn fixture() -> Fixture {
    let modules = 6;
    let mut files: Vec<(String, String)> = Vec::new();
    let mut endpoints = 0usize;

    // `Result<T>` wrapper, shared by every controller.
    files.push((
        "module-0/src/main/java/com/acme/Result.java".to_owned(),
        r#"package com.acme;

public class Result<T> {
    private int code;
    private String msg;
    private T data;
}
"#
        .to_owned(),
    ));

    // One constants class per module, referenced by that module's controllers.
    for k in 0..modules {
        files.push((
            format!("module-{k}/src/main/java/com/acme/m{k}/RouteConstants.java"),
            format!(
                r#"package com.acme.m{k};

public final class RouteConstants {{
    public static final String LIST = "/list";
    public static final String DETAIL = "/{{id}}";
    public static final String CREATE = "/create";
    public static final String UPDATE = "/update/{{id}}";
}}
"#
            ),
        ));
    }

    // DTOs form a chain so the closure walks it, the way a real model does.
    for i in 0..DTOS {
        let module = i % modules;
        let next = (i + 1) % DTOS;
        let next_module = next % modules;
        files.push((
            format!("module-{module}/src/main/java/com/acme/m{module}/dto/Dto{i}.java"),
            format!(
                r#"package com.acme.m{module}.dto;

import com.acme.m{next_module}.dto.Dto{next};

public class Dto{i} {{
    private String name{i};
    private long seq{i};
    private boolean enabled{i};
    private Dto{next} next;
}}
"#
            ),
        ));
    }

    for j in 0..CONTROLLERS {
        let module = j % modules;
        let dto = j % DTOS;
        let dto_module = dto % modules;
        files.push((
            format!("module-{module}/src/main/java/com/acme/m{module}/controller/Ctrl{j}Controller.java"),
            format!(
                r#"package com.acme.m{module}.controller;

import com.acme.Result;
import com.acme.m{module}.RouteConstants;
import com.acme.m{dto_module}.dto.Dto{dto};
import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/m{module}/c{j}")
public class Ctrl{j}Controller {{

    @GetMapping(RouteConstants.LIST)
    public Result<Dto{dto}> list(@RequestParam(value = "page", defaultValue = "1") int page) {{
        return null;
    }}

    @GetMapping(RouteConstants.DETAIL)
    public Result<Dto{dto}> detail(@PathVariable("id") long id) {{
        return null;
    }}

    @PostMapping(RouteConstants.CREATE)
    public Result<Dto{dto}> create(@RequestBody Dto{dto} form) {{
        return null;
    }}

    @PutMapping(RouteConstants.UPDATE)
    public Result<Dto{dto}> update(@RequestBody Dto{dto} form) {{
        return null;
    }}
}}
"#
            ),
        ));
        endpoints += METHODS_PER_CONTROLLER;
    }

    Fixture { files, endpoints }
}

/// Counts how often the scanner asks the host for a declaration by name.
fn install(files: &[(String, String)]) -> (MockHost, Arc<AtomicUsize>) {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("scale tests never touch the network".into()))
    });

    let mut controller_paths = Vec::new();
    for (rel, text) in files {
        let path = format!("{ROOT}/{rel}");
        host.add_file(&path, text);
        if text.contains("@RestController") {
            controller_paths.push(path);
        }
    }

    let searches = Arc::new(AtomicUsize::new(0));
    let counter = searches.clone();
    host.on_search(move |query: &TextQuery, _limit| {
        counter.fetch_add(1, Ordering::Relaxed);
        // The marker search returns the controllers; a declaration fallback is
        // counted but is not expected to fire on this fixture (every file is
        // named after the type it declares).
        let mut matches = Vec::new();
        if declaration_name(&query.pattern).is_none() {
            for path in &controller_paths {
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
    (host, searches)
}

fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

#[test]
fn a_large_project_scans_completely_and_correctly() {
    let fixture = fixture();
    let (host, searches) = install(&fixture.files);
    let project = host.invocation().project().expect("a project");

    let started = Instant::now();
    let scanned = scan::scan(&project, &Options::default()).expect("the fixture must scan");
    let elapsed = started.elapsed();

    // Four distinct paths per controller, so nothing is deduplicated away.
    let expected = CONTROLLERS * METHODS_PER_CONTROLLER;
    assert_eq!(
        scanned.endpoints.len(),
        expected,
        "every controller method in a {CONTROLLERS}-controller project is found; got {:?}",
        scanned
            .endpoints
            .iter()
            .take(8)
            .map(|e| (e.method_name.as_str(), e.path.as_str()))
            .collect::<Vec<_>>()
    );
    // `files` counts the candidate controller files the marker search returned; the
    // `Result` wrapper and the DTOs are resolved by the index, not by the search.
    assert_eq!(scanned.files, CONTROLLERS, "every controller file is read");
    assert!(!scanned.truncated, "the fixture is under every limit");
    assert_eq!(
        scanned.dto.fields.len(),
        DTOS + 1,
        "the closure resolves all {DTOS} DTOs plus the `Result` wrapper"
    );
    assert!(scanned.dto.fields.contains_key("Result"), "the wrapper is resolved");

    // Every route keeps its constant-substituted path rather than collapsing
    // onto the controller prefix.
    let collapsed: Vec<&str> = scanned
        .endpoints
        .iter()
        .filter(|e| e.path.matches('/').count() < 3)
        .map(|e| e.path.as_str())
        .collect();
    assert!(collapsed.is_empty(), "routes lost their constants: {collapsed:?}");

    // The point of the fix: the index answers every type in the project, so the
    // host is asked for a declaration only for names no file declares. Here that
    // is `String` and `T` — the wrapper's `data` field is typed by the type
    // parameter, and both are scalars a project never declares. Each costs one
    // search and is then remembered, so the count is a small constant rather
    // than a multiple of the type count.
    assert_eq!(
        searches.load(Ordering::Relaxed),
        3,
        "one marker search plus the two names no file declares (`String`, `T`)"
    );

    // MockHost is in-memory, so this only guards against a pathological
    // regression (an accidental quadratic loop), not the host's 5 s limit.
    assert!(
        elapsed < Duration::from_secs(20),
        "a {CONTROLLERS}-controller scan took {elapsed:?} in a debug build"
    );
}

#[test]
fn the_search_count_does_not_grow_with_the_number_of_types() {
    // The regression this prevents: resolving types by asking the host for each
    // one. With the index the marker search is all that happens, so doubling the
    // DTO count must not double the searches.
    let fixture = fixture();
    let (host, searches) = install(&fixture.files);
    let project = host.invocation().project().expect("a project");

    let scanned = scan::scan(&project, &Options::default()).expect("a scan");
    assert_eq!(scanned.dto.fields.len(), DTOS + 1);

    let count = searches.load(Ordering::Relaxed);
    // A constant, not a function of `DTOS`: one marker search plus the scalar
    // names (`String`, `T`) that no file declares. Reinstating a per-type lookup
    // would push this into the hundreds.
    assert!(
        count <= 5,
        "{count} searches for {DTOS} types: the type index is not being used"
    );
}

#[test]
fn a_spent_budget_stops_a_large_scan_instead_of_running_on() {
    // The host kills a call that overruns and reaps the instance with it. The
    // guard must cut in first, returning a partial result fast.
    let fixture = fixture();
    let (host, _) = install(&fixture.files);
    let project = host.invocation().project().expect("a project");

    let options = Options { budget: Some(Duration::ZERO), ..Options::default() };
    let started = Instant::now();
    let scanned = scan::scan(&project, &options).expect("a spent budget is not an error");
    let elapsed = started.elapsed();

    assert!(scanned.truncated, "the result says it is incomplete");
    assert!(
        elapsed < Duration::from_secs(5),
        "the guard must return at once, took {elapsed:?}"
    );
}

#[test]
fn a_generous_budget_changes_nothing() {
    // A budget the scan cannot reach must be invisible: same answer as no
    // budget at all. This catches a guard that fires on a misread clock.
    let fixture = fixture();
    let (host, _) = install(&fixture.files);
    let project = host.invocation().project().expect("a project");

    let options = Options { budget: Some(Duration::from_secs(60)), ..Options::default() };
    let scanned = scan::scan(&project, &options).expect("a scan");

    assert!(!scanned.truncated, "a 60 s budget is not reached");
    assert_eq!(scanned.endpoints.len(), fixture.endpoints);
    assert_eq!(scanned.dto.fields.len(), DTOS + 1);
}
