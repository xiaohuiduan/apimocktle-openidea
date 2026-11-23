//! Guards the demo generator against drifting away from what the scanner
//! accepts: a project that no longer parses would silently stop reproducing the
//! timeout this repository is diagnosing.
//!
//! The generator is an `examples/` binary, so a test cannot call it directly
//! without a `#[path]` include, and moving it into `src/` would ship a
//! development-only tool inside the 491 KiB component. The test therefore reads
//! what `scan_bench` leaves behind: run the generator once, and this file
//! verifies the result scans to the endpoints the generator's own manifest
//! declares.
//!
//! ```sh
//! cargo run --release --example gen_demo -- --preset small
//! cargo test --test demo_scan -- --nocapture
//! ```
//!
//! When the demo is absent the test reports that it skipped and why. It must
//! never pass silently while covering nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use openidea_apimocktle::scan::{self, Options};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch};

const ROOT: &str = "/proj";

/// The project `gen_demo` writes by default for a preset.
fn demo_dir() -> PathBuf {
    std::env::temp_dir().join("apimocktle-demo").join("small")
}

/// What `gen_demo` recorded about the project it wrote.
struct Manifest {
    controllers: usize,
    methods_per_controller: usize,
    source_files: usize,
    endpoints: usize,
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dir.join("demo.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let number = |key: &str| value.get(key).and_then(serde_json::Value::as_u64).map(|n| n as usize);
    Some(Manifest {
        controllers: number("controllers")?,
        methods_per_controller: number("methodsPerController")?,
        source_files: number("sourceFiles")?,
        endpoints: number("endpoints")?,
    })
}

fn read_sources(dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !matches!(name.as_str(), "target" | "build" | "out" | ".git" | ".idea") {
                    walk(&path, root, out);
                }
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("java" | "kt" | "xml")) {
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

/// The same stand-in for Find in Files that `scan_bench` uses: without it the
/// scanner gets no candidate files back and the assertions would test nothing.
fn install(sources: &[(String, String)]) -> MockHost {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.set_project_root(Some(ROOT));
    host.add_project(ROOT);
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("demo_scan 不联网".into()))
    });
    let index: BTreeMap<String, String> = sources.iter().cloned().collect();
    host.on_search(move |query, limit| Ok(mimic_search(&index, query, limit)));
    host
}

fn mimic_search(index: &BTreeMap<String, String>, query: &TextQuery, limit: u32) -> TextSearch {
    let limit = limit.min(10_000) as usize;
    let include_kotlin = query.include.iter().any(|g| g.ends_with(".kt"));
    let mut matches = Vec::new();
    let mut limit_hit = false;
    'files: for (path, text) in index {
        let wanted = path.ends_with(".java") || (include_kotlin && path.ends_with(".kt"));
        if !wanted {
            continue;
        }
        for offset in marker_offsets(text) {
            if matches.len() >= limit {
                limit_hit = true;
                break 'files;
            }
            let (line, col) = line_col(text, offset);
            matches.push(TextMatch {
                location: Location {
                    path: path.clone(),
                    range: Range::new(Position::new(line, col), Position::new(line, col + 1)),
                },
                line: String::new(),
            });
        }
    }
    TextSearch { matches, limit_hit }
}

const MARKERS: &[&str] = &[
    "RestController",
    "Controller",
    "RequestMapping",
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PatchMapping",
    "FeignClient",
    "Path",
];

fn marker_offsets(text: &str) -> Vec<usize> {
    text.match_indices('@')
        .filter(|(i, _)| MARKERS.iter().any(|name| text[i + 1..].starts_with(name)))
        .map(|(i, _)| i)
        .collect()
}

fn line_col(text: &str, byte: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, c) in text.char_indices() {
        if i >= byte {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += c.len_utf16() as u32;
        }
    }
    (line, col)
}

#[test]
fn the_generated_demo_scans_into_the_endpoints_its_manifest_declares() {
    let dir = demo_dir();
    let Some(manifest) = read_manifest(&dir) else {
        println!(
            "跳过：{} 下没有 demo.json。\n\
             先生成一次再跑这个用例：\n  \
             cargo run --release --example gen_demo -- --preset small\n\
             （这个用例在没有输入时跳过，不算通过；它守的是生成物仍能被扫描器解析。）",
            dir.display()
        );
        return;
    };
    let sources = read_sources(&dir);
    assert!(!sources.is_empty(), "{} 里没有源文件", dir.display());

    let host = install(&sources);
    let project = host.invocation().project().expect("MockHost 应当提供项目");
    for (path, text) in &sources {
        host.add_file(path, text);
    }

    let scanned = scan::scan(&project, &Options::default()).expect("生成的工程必须能扫描");

    // Structural counts only: `TYPE_BUDGET` and `FILE_LIMIT` cap the DTO and
    // file totals, so asserting those against the generator's numbers would tie
    // this test to constants it does not own.
    assert_eq!(
        scanned.endpoints.len(),
        manifest.endpoints,
        "接口数应与 manifest 一致（{} 控制器 × {} 接口 + Feign + Kotlin）",
        manifest.controllers,
        manifest.methods_per_controller
    );
    assert!(
        scanned.files > 0 && scanned.files <= manifest.source_files,
        "解析了 {} 个文件，manifest 说有 {} 个源文件",
        scanned.files,
        manifest.source_files
    );
    assert!(scanned.dto.fields.contains_key("Result"), "Result<T> 包装必须被解析到");

    // Every route carries a real path, not the `/` fallback a failed constant
    // substitution would leave behind. Each controller declares four methods on
    // four different paths, so a collapsed controller (all four landing on
    // `/api/mK/cJ`) shows up here as a duplicate.
    for endpoint in &scanned.endpoints {
        assert!(endpoint.path.starts_with('/'), "路径写法不对：{}", endpoint.path);
        assert!(
            endpoint.path.matches('/').count() >= 3 || endpoint.framework == "Feign",
            "{} 的常量没有回填",
            endpoint.path
        );
    }
    // A controller that failed to substitute its constants collapses every
    // method onto the same class prefix. Two methods may legitimately share a
    // path under different verbs (`GET /x/{id}` and `PUT /x/{id}`), so the
    // duplicate check keys on `method + path`, which is also what
    // `Endpoint::key` uses.
    let mut by_class: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for endpoint in &scanned.endpoints {
        by_class.entry(endpoint.class_name.as_str()).or_default().push(endpoint.key());
    }
    for (class, keys) in &by_class {
        let distinct: std::collections::BTreeSet<&String> = keys.iter().collect();
        assert_eq!(
            distinct.len(),
            keys.len(),
            "{class} 的路由塌成了同一组 {keys:?}：常量回填失效"
        );
    }
    assert!(
        scanned.endpoints.iter().any(|e| e.framework == "Spring"),
        "至少要有 Spring 接口"
    );
    assert!(scanned.endpoints.iter().any(|e| e.framework == "Feign"), "Feign 客户端也要被识别");
    println!("通过：{} 个接口，{} 个文件，{} 个 DTO 类型", scanned.endpoints.len(), scanned.files, scanned.dto.fields.len());
}
