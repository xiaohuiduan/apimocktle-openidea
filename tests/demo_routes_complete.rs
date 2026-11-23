//! The generated demo project must publish every route of every controller.
//!
//! This is the end-to-end shape the dashboard shows: a controller with four
//! methods must yield four distinct `method + path` pairs, or the page collapses
//! it into fewer rows. It covers the route-constant path in particular — a
//! controller whose constants fail to substitute lands all its methods on the
//! class prefix and dedups down to one row.
//!
//! Skips (with a note) when the demo has not been generated.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use openidea_apimocktle::scan::{self, Options};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextSearch};

const ROOT: &str = "/proj";

fn demo_dir() -> PathBuf {
    std::env::temp_dir().join("apimocktle-demo").join("big")
}

fn read_sources(dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !matches!(name.as_str(), "target" | "build" | ".git") {
                    walk(&path, root, out);
                }
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("java" | "kt")) {
                let rel = path
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((format!("{ROOT}/{rel}"), text));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn every_generated_controller_publishes_all_its_routes() {
    let dir = demo_dir();
    if !dir.is_dir() {
        println!(
            "跳过：{} 不存在。先生成：\n  cargo run --release --example gen_demo -- --preset big",
            dir.display()
        );
        return;
    }
    let sources = read_sources(&dir);
    assert!(!sources.is_empty(), "{} 里没有源文件", dir.display());

    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("no network".into()))
    });
    let mut controllers = Vec::new();
    for (path, text) in &sources {
        host.add_file(path, text);
        if text.contains("@RestController") {
            controllers.push(path.clone());
        }
    }
    host.on_search(move |_q, _l| {
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

    let project = host.invocation().project().expect("a project");
    // The dashboard's tier: routes only, which is what the page shows.
    let options = Options { resolve_dto: false, ..Options::default() };
    let scanned = scan::scan(&project, &options).expect("the demo must scan");

    let mut by_class: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for endpoint in &scanned.endpoints {
        by_class.entry(endpoint.class_name.as_str()).or_default().insert(endpoint.key());
    }
    println!("{} endpoints across {} classes", scanned.endpoints.len(), by_class.len());

    // The generated Java controllers each have four methods on four distinct
    // paths; the Kotlin one has two. If route constants fail to substitute, the
    // Java ones all collapse onto the class prefix and the count drops — which
    // is exactly the regression the dashboard showed as one row per controller.
    let short: Vec<(&str, usize)> = by_class
        .iter()
        .filter(|(class, keys)| {
            let expected = if class.starts_with("Kt") { 2 } else { 4 };
            keys.len() != expected
        })
        .map(|(c, k)| (*c, k.len()))
        .collect();
    assert!(
        short.is_empty(),
        "controllers not publishing their expected routes: {:?}",
        &short[..short.len().min(6)]
    );

    // And the Java controllers' paths are the constants', not the bare prefix.
    // `DETAIL` and `UPDATE` are both `/{id}` in the generated constants, so the
    // verb is what distinguishes them — the uniqueness check above is on
    // `method + path`, which is also what `Endpoint::key` uses.
    let java: Vec<_> = by_class.iter().filter(|(c, _)| !c.starts_with("Kt")).take(3).collect();
    for (class, keys) in java {
        println!("{class}: {keys:?}");
        assert!(keys.iter().any(|k| k.contains("/list")), "{class} lost /list: {keys:?}");
        assert!(keys.iter().any(|k| k.contains("/create")), "{class} lost /create: {keys:?}");
        assert!(keys.iter().any(|k| k.starts_with("GET ") && k.contains("/{id}")), "{class} lost GET /{{id}}: {keys:?}");
        assert!(keys.iter().any(|k| k.starts_with("PUT ") && k.contains("/{id}")), "{class} lost PUT /{{id}}: {keys:?}");
    }
}
