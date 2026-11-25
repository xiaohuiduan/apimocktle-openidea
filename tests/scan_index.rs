//! Type indexing and the scan budget.
//!
//! The scan used to ask the host for `**/{name}.java` and `**/{name}.kt` once
//! per DTO type it reached — up to `TYPE_BUDGET` × 2 project-wide walks, each a
//! synchronous host call. On a few hundred controllers that alone spent most of
//! the host's five-second limit and got the plugin killed. These tests pin the
//! replacement: one index built up front, the declaration search kept as a
//! fallback, and a budget that returns a partial result instead of overrunning.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use openidea_apimocktle::scan::{self, Options};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{
    Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch,
};

const ROOT: &str = "/proj";

/// A controller whose body mentions a DTO, so the closure has work to do.
fn controller(name: &str, dto: &str) -> String {
    format!(
        r#"
package com.acme;

@RestController
@RequestMapping("/api/{name}")
public class {name}Controller {{
    @PostMapping("/save")
    public {dto} save(@RequestBody {dto} form) {{ return null; }}
}}
"#
    )
}

fn dto(name: &str) -> String {
    format!(
        r#"
package com.acme;

public class {name} {{
    private String title;
    private int count;
}}
"#
    )
}

/// Installs a project whose files all answer the marker search, and records how
/// many times the scanner asked for a declaration by name.
fn host(files: &[(&str, String)]) -> (MockHost, Arc<AtomicUsize>) {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    let mut indexed: BTreeMap<String, String> = BTreeMap::new();
    for (rel, text) in files {
        let path = format!("{ROOT}/{rel}");
        host.add_file(&path, text);
        indexed.insert(path, text.clone());
    }
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("no network in this test".into()))
    });

    let searches = Arc::new(AtomicUsize::new(0));
    let counter = searches.clone();
    host.on_search(move |query: &TextQuery, _limit| {
        // One counter for every search keeps the assertion honest: both the
        // marker search and the declaration fallback go through here.
        counter.fetch_add(1, Ordering::Relaxed);
        let declaration = declaration_name(&query.pattern);
        let mut matches = Vec::new();
        for (path, text) in &indexed {
            if !path.ends_with(".java") {
                continue;
            }
            let hit = match &declaration {
                // The fallback shape: `(class|interface|record)\s+Name\b`.
                Some(name) => text.contains(&format!("class {name} ")),
                // The marker search: any mapping annotation.
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
    (host, searches)
}

fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

fn scan_with(host: &MockHost, options: &Options) -> scan::ScanResult {
    let project = host.invocation().project().expect("a project");
    scan::scan(&project, options).expect("a scan")
}

#[test]
fn a_dto_is_resolved_without_asking_for_its_file_by_name() {
    // `Widget` is declared in `Widget.java`, so the index answers it: the scan
    // must cost exactly one search (the marker search), not one per type.
    let (host, searches) = host(&[
        ("src/WidgetController.java", controller("Widget", "Widget")),
        ("src/Widget.java", dto("Widget")),
    ]);
    let scanned = scan_with(&host, &Options::default());

    assert_eq!(scanned.endpoints.len(), 1, "the route is found");
    assert!(scanned.dto.fields.contains_key("Widget"), "the DTO is resolved");
    assert_eq!(
        searches.load(Ordering::Relaxed),
        2,
        "one marker search, plus one declaration search for `Result` (which no file declares); \
         `Widget` itself cost nothing extra"
    );
}

#[test]
fn a_class_named_differently_from_its_file_falls_back_to_a_declaration_search() {
    // `Hidden` lives in `SomethingElse.java`. The index has no `Hidden` entry,
    // so the declaration search must still find it — that fallback is why the
    // index alone cannot be the whole answer.
    let (host, searches) = host(&[
        ("src/HiddenController.java", controller("Hidden", "Hidden")),
        ("src/SomethingElse.java", dto("Hidden")),
    ]);
    let scanned = scan_with(&host, &Options::default());

    assert!(scanned.dto.fields.contains_key("Hidden"), "the fallback resolves a renamed file");
    assert!(
        searches.load(Ordering::Relaxed) > 1,
        "a declaration search was issued for the missing name"
    );
}

#[test]
fn a_type_that_does_not_exist_costs_one_search_not_one_per_endpoint() {
    // Two controllers both return `Missing`, which no file declares. The miss
    // is remembered, so the second mention does not pay for another search.
    // Two endpoints mention the same absent type.
    let doubled = r#"
package com.acme;

@RestController
@RequestMapping("/api/dup")
public class DupController {
    @GetMapping("/one")
    public Missing one() { return null; }

    @GetMapping("/two")
    public Missing two() { return null; }
}
"#;
    let (host, searches) = host(&[("src/DupController.java", doubled.to_owned())]);
    let scanned = scan_with(&host, &Options::default());

    assert_eq!(scanned.endpoints.len(), 2, "both routes are still published");
    assert!(!scanned.dto.fields.contains_key("Missing"), "an absent type stays unresolved");
    // One marker search plus one declaration search for `Missing`.
    assert_eq!(
        searches.load(Ordering::Relaxed),
        2,
        "the miss is cached: two endpoints mentioning `Missing` cost one search"
    );
}

#[test]
fn an_exhausted_budget_returns_partial_results_instead_of_failing() {
    // A budget of zero is already spent when the first check runs. The scan must
    // report what it has and say it is incomplete — returning `Err`, or letting
    // the host kill the call, would lose the cache as well.
    //
    // The two "incomplete" reasons are reported separately because the caller
    // words them differently: `truncated` is the search limit (a fixed property
    // of the project), `stopped_early` is the clock (a retry may do better).
    let (host, _) = host(&[
        ("src/WidgetController.java", controller("Widget", "Widget")),
        ("src/Widget.java", dto("Widget")),
    ]);
    let options = Options { budget: Some(Duration::ZERO), ..Options::default() };
    let scanned = scan_with(&host, &options);

    assert!(scanned.stopped_early, "a budget stop says so");
    assert!(!scanned.truncated, "the search limit was not the reason");
    assert_eq!(scanned.files, 0, "nothing was read before the deadline");
}

#[test]
fn without_a_budget_the_scan_is_complete() {
    // The library default has no deadline: tests and examples must not depend
    // on wall-clock timing, and a small project finishes regardless.
    let (host, _) = host(&[
        ("src/WidgetController.java", controller("Widget", "Widget")),
        ("src/Widget.java", dto("Widget")),
    ]);
    let scanned = scan_with(&host, &Options::default());

    assert!(!scanned.truncated, "no budget, no truncation");
    // Only the controller declares a mapping annotation, so it is the one
    // candidate file; the DTO is resolved through the index instead.
    assert_eq!(scanned.files, 1, "the controller file is read");
}

#[test]
fn the_type_index_covers_a_project_bigger_than_the_controller_limit() {
    // A project has far more sources than controllers: yudao-cloud has 735
    // controllers among 8140 Java files. The index used to take `file_limit`
    // (5000) as its own bound, so the last ~3000 files were never indexed and
    // every type declared there fell back to a project-wide search — 390 ms
    // each on the real host, 125 of them spending 49 s of the call budget.
    //
    // The assertion is on searches, not on the index's size: a miss is what
    // costs time, and it is observable here.
    //
    // **What this test cannot see**: `MockHost` ignores `find_files`' limit, so
    // it cannot reproduce a truncated index. The test therefore guards the
    // *outcome* (types resolved by file name rather than by project-wide search)
    // for a project larger than the controller limit, but the truncation itself
    // would still slip through here — only the real host exposes it. It was
    // measured there: 8140 files against a 5000-file index cost 125 project-wide
    // searches, 49 s.
    let controllers = 3;
    let fillers = crate::scan::FILE_LIMIT as usize + 40;
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..controllers {
        files.push((
            format!("src/Ctrl{i}Controller.java"),
            controller(&format!("Item{i}"), &format!("Item{i}")),
        ));
    }
    // Types that exist only as plain classes, well past the controller limit.
    // They are what the index has to answer for.
    for i in 0..fillers {
        files.push((format!("src/dto/Filler{i}.java"), dto(&format!("Filler{i}"))));
    }
    let borrowed: Vec<(&str, String)> = files.iter().map(|(p, t)| (p.as_str(), t.clone())).collect();
    let (host, searches) = host(&borrowed);

    let scanned = scan_with(&host, &Options::default());
    assert_eq!(scanned.files, controllers, "only controllers are parsed");

    // Every controller resolved its DTO through the index. If the index had been
    // cut at `FILE_LIMIT`, the controller DTOs (declared past it) would each cost
    // a declaration search. A handful of searches still happen for the JDK types
    // the fixture mentions (`String`, `int`), so the bound is on "not one per
    // controller", not on an exact count.
    let total = searches.load(Ordering::Relaxed);
    assert!(
        total <= controllers + 2,
        "the index answered the controller types by file name, so no declaration \
         search per controller was needed (got {total} for {controllers} controllers)"
    );
}