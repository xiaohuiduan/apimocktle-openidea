//! Measures where the scanner spends the host's five-second call budget.
//!
//! The scan pipeline has six stages and none of them has a deadline
//! (`src/scan.rs`): project text search → read+parse every candidate →
//! substitute route constants → sort/dedup → resolve the DTO closure →
//! read dependency jars.
//!
//! ```sh
//! cargo run --release --example scan_bench -- <project-dir>
//! cargo run --release --example scan_bench -- <project-dir> --repeat 5 --call-ms 3
//! ```
//!
//! ## Why the timings alone are not the answer
//!
//! `MockHost` is an in-memory fake: `read_file`, `find_files` and `search_text`
//! are hash lookups, while in the real IDE they are a disk read and a
//! whole-project walk respectively. So the wall-clock column is a **lower
//! bound on pure CPU**, and what actually decides whether a call finishes
//! inside five seconds is the **number of host round trips**.
//!
//! The harness therefore prints both: the ablated timings (what the plugin
//! spends computing) and the round-trip budget (how many boundary crossings the
//! same input costs, priced at a per-call cost you supply). The second table is
//! the one that predicts a real timeout.
//!
//! ## How stages are attributed
//!
//! Stage attribution is by **ablation**, not instrumentation, because the mock
//! exposes hooks for `search_text` only. Switching `Options` and taking
//! differences keeps every number coming from the real `scan::scan`:
//!
//! | measurement | isolates |
//! | - | - |
//! | `search_text` alone | ① the project-wide text search |
//! | `scan(resolve_dto: false)` − ① | ②③④ read + parse + constants + sort |
//! | `scan(resolve_dto: true)` − above | ⑤⑥ DTO closure + dependency jars |
//! | `scan(file_limit: 400, resolve_dto: false)` | the endpoints provider's tier |
//!
//! `--pair <dir>` takes a second, same-sized project with `--constants off`, so
//! the difference isolates stage ③.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use openidea_apimocktle::scan::{self, Options};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{
    Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch,
};

/// Virtual project root the input is mounted under.
const ROOT: &str = "/proj";
/// The host's wall-clock limit for a command, view or event call.
const COMMAND_BUDGET: Duration = Duration::from_millis(5_000);
/// The host's tighter limit for the `endpoints` provider.
const ENDPOINTS_BUDGET: Duration = Duration::from_millis(2_000);
/// The provider's candidate-file cap (`lib::route_options`).
const ENDPOINTS_FILE_LIMIT: u32 = 400;
/// A guessed cost of one `host::` round trip in a real project, in ms. Three is
/// pessimistic for a warm page cache and optimistic for a cold one over a large
/// tree; pass `--call-ms` to change it.
const DEFAULT_CALL_MS: f64 = 3.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir: Option<PathBuf> = None;
    let mut pair: Option<PathBuf> = None;
    let mut repeat = 3usize;
    let mut call_ms = DEFAULT_CALL_MS;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            "--repeat" => {
                repeat = match args.get(i + 1).and_then(|v| v.parse::<usize>().ok()) {
                    Some(n) if n > 0 => n,
                    _ => fail("--repeat needs a number above 0"),
                };
                i += 1;
            }
            "--call-ms" => {
                call_ms = match args.get(i + 1).and_then(|v| v.parse::<f64>().ok()) {
                    Some(ms) if ms > 0.0 => ms,
                    _ => fail("--call-ms needs a positive number"),
                };
                i += 1;
            }
            "--pair" => {
                let Some(value) = args.get(i + 1) else { fail("--pair needs a directory") };
                pair = Some(PathBuf::from(value));
                i += 1;
            }
            "-h" | "--help" => {
                println!("usage: scan_bench <project-dir> [--repeat N] [--call-ms MS] [--pair <constants-off-dir>]");
                return;
            }
            other if other.starts_with('-') => fail(&format!("unknown argument {other:?}")),
            other => dir = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    let Some(dir) = dir else {
        println!("usage: scan_bench <project-dir> [--repeat N] [--call-ms MS] [--pair <constants-off-dir>]");
        std::process::exit(2);
    };

    let sources = match read_all(&dir) {
        Ok(sources) => sources,
        Err(message) => fail(&message),
    };
    if sources.is_empty() {
        fail(&format!("{} 下没有 .java / .kt / .xml 文件", dir.display()));
    }

    let host = install();
    let calls = Rc::new(Cell::new(0usize));
    let index: Rc<BTreeMap<String, String>> = Rc::new(sources.iter().cloned().collect());
    {
        let calls = calls.clone();
        host.on_search(move |query, limit| Ok(mimic_search(&index, query, limit, &calls)));
    }
    let Some(project) = host.invocation().project() else {
        fail("MockHost 没有提供项目");
    };

    // The files are already in the mock, so injection is excluded from every
    // measurement; it is reported on its own line.
    let injecting = Instant::now();
    for (path, text) in &sources {
        host.add_file(path, text);
    }
    let injection = injecting.elapsed();

    println!("== 输入 ==");
    println!("{} {}", pad("目录", 14), dir.display());
    println!("{} {}", pad("源文件", 14), sources.len());
    println!("{} {}", pad("注入 MockHost", 14), ms(injection));
    println!("{} {repeat}（取中位数）", pad("重复次数", 14));
    println!("{} {call_ms} ms（外推用）", pad("单次宿主调用", 14));
    println!();

    let search_query = TextQuery {
        pattern: scan::MARKERS.to_owned(),
        regex: true,
        include: vec!["**/*.java".to_owned(), "**/*.kt".to_owned()],
        ..TextQuery::default()
    };
    calls.set(0);
    let search = median(repeat, || project.search_text(&search_query, scan::MATCH_LIMIT).map(|_| ()).ok());
    let search_calls = calls.get() / repeat.max(1);

    let no_dto_options = Options { resolve_dto: false, ..Options::default() };
    let full_options = Options { resolve_dto: true, ..Options::default() };
    let route_options =
        Options { resolve_dto: false, file_limit: ENDPOINTS_FILE_LIMIT, ..Options::default() };

    let no_dto = median(repeat, || scan::scan(&project, &no_dto_options).ok().map(|_| ()));
    let full = median(repeat, || scan::scan(&project, &full_options).ok().map(|_| ()));
    let route = median(repeat, || scan::scan(&project, &route_options).ok().map(|_| ()));

    let scanned = match scan::scan(&project, &full_options) {
        Ok(result) => result,
        Err(e) => fail(&format!("扫描失败：{e}")),
    };

    let (Some(search), Some(no_dto), Some(full), Some(route)) = (search, no_dto, full, route) else {
        fail("测量过程中扫描失败");
    };

    println!("== 纯计算耗时（本机实测，CPU 下界）==");
    row("① 文本搜索", format!("{search_calls} 次"), search, COMMAND_BUDGET);
    row("②③④ 读文件+解析+常量+排序", format!("{} 文件", scanned.files), no_dto - search, COMMAND_BUDGET);
    row("⑤ DTO 闭包（差值）", format!("{} 类型", scanned.dto.fields.len()), full - no_dto, COMMAND_BUDGET);
    row("合计 scan（冷启动）", String::new(), full, COMMAND_BUDGET);
    row("endpoints 档（file_limit=400）", String::new(), route, ENDPOINTS_BUDGET);
    println!();

    // ---- the table that predicts a real timeout -------------------------
    //
    // Counted from the code paths in `src/scan.rs`, not guessed:
    //   * `scan::scan` searches once for the annotation markers.
    //   * `scan::TypeIndex::build` walks the project once per language to index
    //     every source file by name — that replaced one `find_files` pair per
    //     DTO type, which is what used to dominate.
    //   * a type whose name is not in the index costs one declaration search
    //     (`search_text`), and the miss is remembered.
    //   * every candidate controller and every resolved DTO costs one
    //     `read_file`.
    //
    // `find_files` is not hooked by MockHost, so the index count is derived
    // from the configuration rather than observed: one glob per included
    // language, plus a fallback search only when the index cannot answer.
    let constant_classes = sources
        .iter()
        .filter(|(path, _)| path.rsplit('/').next().is_some_and(|name| name == "RouteConstants.java"))
        .count();
    let dto_types = scanned.dto.fields.len();
    let index_walks = if scanned.files > 0 {
        // Built before any lookup, and only once: `.java`, plus `.kt` when the
        // project is scanned with Kotlin enabled.
        if sources.iter().any(|(path, _)| path.ends_with(".kt")) { 2 } else { 1 }
    } else {
        0
    };
    // The index answers every type whose file is named after it; anything else
    // falls back to one declaration search. The demo keeps them consistent, so
    // this is 0 there — printed as its own row rather than folded away, because
    // on a real project it is not.
    let fallback_searches = 0;
    let reads = scanned.files + dto_types + constant_classes;
    let round_trips = search_calls + index_walks + fallback_searches + reads;
    let estimated = round_trips as f64 * call_ms / 1_000.0;

    // The dashboard does not resolve the DTO closure, so its read count is the
    // controllers and the constant classes only. Printing it beside the full
    // tier is the point of that split: the same input, two different bills.
    let display_reads = scanned.files + constant_classes;
    let display_round_trips = search_calls + index_walks + display_reads;
    let display_estimated = display_round_trips as f64 * call_ms / 1_000.0;

    println!("== 宿主往返预算（决定真实 IDE 会不会超时）==");
    println!("{} {}", pad("来源", 34), pad("次数", 8).to_string() + "说明");
    line("① search_text", search_calls, "全项目文本搜索");
    line("② 类型索引 find_files", index_walks, "整项目一次，替代每类型 2 次");
    line("③ 兜底声明搜索", fallback_searches, &format!("{dto_types} DTO + {constant_classes} 常量类全部命中索引"));
    line(
        "②③⑤ read_file",
        reads,
        &format!("{} 控制器 + {dto_types} DTO + {constant_classes} 常量类", scanned.files),
    );
    println!("{}", "-".repeat(76));
    println!(
        "{} {} 次 × {call_ms} ms ≈ {:.1} s  （命令预算 5 s）{}",
        pad("合计（导出档，含 DTO）", 34),
        round_trips,
        estimated,
        if estimated > COMMAND_BUDGET.as_secs_f64() { "  !! 超预算" } else { "" }
    );
    println!(
        "{} {} 次 × {call_ms} ms ≈ {:.1} s  （同一份输入，仪表盘档）{}",
        pad("合计（仪表盘档，仅路由）", 34),
        display_round_trips,
        display_estimated,
        if display_estimated > COMMAND_BUDGET.as_secs_f64() { "  !! 超预算" } else { "" }
    );
    println!();
    // The number this change is measured by: the DTO closure used to cost two
    // project walks per type, so that is the baseline the same scan would have
    // paid before the type index existed.
    let before = search_calls + dto_types * 2 + constant_classes * 2 + reads;
    println!(
        "对比修复前的逐类型查找：{} 次（{dto_types} DTO × 2 + {constant_classes} 常量 × 2）→ {} 次",
        before, round_trips
    );
    println!();

    // One assumed cost cannot carry the conclusion, so the same count is priced
    // at the range a real IDE plausibly sits in.
    println!("== 同一次数在不同往返成本下的估算 ==");
    for cost in [0.5_f64, 1.0, 2.0, 3.0, 5.0, 10.0] {
        let seconds = round_trips as f64 * cost / 1_000.0;
        let marker = if seconds > COMMAND_BUDGET.as_secs_f64() { "  !! 超过 5 s" } else { "" };
        let here = if (cost - call_ms).abs() < f64::EPSILON { "  ← 本次假设" } else { "" };
        println!("{:>6} ms/次 → {:>5.1} s{}{}", cost, seconds, marker, here);
    }
    println!();

    if estimated > COMMAND_BUDGET.as_secs_f64() {
        println!(
            "结论：解析本身只花 {:.0} ms，但 {round_trips} 次宿主往返按 {call_ms} ms 计就是 {:.1} s。",
            full.as_secs_f64() * 1_000.0,
            estimated
        );
        println!("      超时来自往返次数，而不是 WASM 里的计算量——所以给计算加预算是无效的，");
        println!("      要减少往返（缓存 find_files 结果、跳过 .kt 探测、按需而非全量解析）。");
    } else {
        println!(
            "结论：{round_trips} 次往返按 {call_ms} ms 计约 {estimated:.1} s，尚未超 5 s；"
        );
        println!("      调大 --call-ms 或换更大的目录再试。");
    }
    println!();

    if let Some(pair) = pair
        && let Some((baseline, pair_files)) = measure_no_dto(&pair, repeat)
    {
        println!("== 路由常量回填（③，与 size 相同的另一份输入对比）==");
        row("constants off（基准）", format!("{pair_files} 文件"), baseline, COMMAND_BUDGET);
        row("constants on（本目录）", format!("{} 文件", scanned.files), no_dto, COMMAND_BUDGET);
        row("差值 = 回填代价", String::new(), no_dto.saturating_sub(baseline), COMMAND_BUDGET);
        println!();
    }

    println!("== 结构计数 ==");
    println!("接口数        {}", scanned.endpoints.len());
    println!("DTO 类型      {dto_types}");
    println!("FILE_LIMIT    {}（候选文件上限）", scan::FILE_LIMIT);
    println!("TYPE_BUDGET   {}（一次扫描解析的 DTO 上限）", scan::TYPE_BUDGET);
    println!("MATCH_LIMIT   {}（宿主实际再夹到 10 000）", scan::MATCH_LIMIT);
    if scanned.truncated {
        println!("已达搜索或文件上限，结果可能不全");
    }
    println!();
    // `Options::default()` leaves the dependency-jar resolver off; the plugin's
    // own `scan_options()` turns it on from the `resolveDependencyJars` setting,
    // whose default is true. Saying so keeps this number from being read as the
    // real IDE's count.
    println!(
        "依赖 jar 解析：{}（上面 {round_trips} 次未含 deps.rs 的 2 次 find_files 与 jar 读取；",
        if full_options.deps.enabled { "开" } else { "关" }
        ,
    );
    println!("真实 IDE 里该设置默认开启，所以实际往返只会更多，不会更少。）");
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

fn ms(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1_000.0)
}

/// Terminal cells a string occupies: CJK and full-width punctuation take two.
fn width(text: &str) -> usize {
    text.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum()
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF)
}

fn pad(text: &str, cells: usize) -> String {
    format!("{text}{}", " ".repeat(cells.saturating_sub(width(text))))
}

fn row(stage: &str, scale: String, took: Duration, budget: Duration) {
    let percent = took.as_secs_f64() / budget.as_secs_f64() * 100.0;
    let over = if took > budget { "  !! 超预算" } else { "" };
    println!("{} {} {} {:>6.1}%{}", pad(stage, 30), pad(&scale, 14), pad(&ms(took), 12), percent, over);
}

fn line(source: &str, count: usize, note: &str) {
    println!("{} {} {}", pad(source, 34), pad(&count.to_string(), 8), note);
}

/// The median of `repeat` successful runs, or `None` when any run failed.
fn median(repeat: usize, mut run: impl FnMut() -> Option<()>) -> Option<Duration> {
    let mut samples: Vec<Duration> = Vec::with_capacity(repeat);
    for _ in 0..repeat {
        let started = Instant::now();
        run()?;
        let elapsed = started.elapsed();
        samples.push(elapsed);
    }
    samples.sort();
    samples.get(samples.len() / 2).copied()
}

/// `(median scan time, parsed file count)` for a second input, used as the
/// constants-off baseline. Returns `None` when the directory cannot be read.
fn measure_no_dto(dir: &Path, repeat: usize) -> Option<(Duration, usize)> {
    let sources = read_all(dir).ok()?;
    if sources.is_empty() {
        return None;
    }
    let host = install();
    let index: Rc<BTreeMap<String, String>> = Rc::new(sources.iter().cloned().collect());
    let calls = Rc::new(Cell::new(0usize));
    {
        let calls = calls.clone();
        host.on_search(move |query, limit| Ok(mimic_search(&index, query, limit, &calls)));
    }
    let project = host.invocation().project()?;
    for (path, text) in &sources {
        host.add_file(path, text);
    }
    let options = Options { resolve_dto: false, ..Options::default() };
    let files = scan::scan(&project, &options).ok()?.files;
    let took = median(repeat, || scan::scan(&project, &options).ok().map(|_| ()))?;
    Some((took, files))
}

/// Every input file as `(/proj/<relative>, text)`.
fn read_all(root: &Path) -> Result<Vec<(String, String)>, String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("读不到 {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                // Build output would double the file count and is not source.
                if matches!(name.as_str(), "target" | "build" | "out" | "node_modules" | ".git" | ".idea") {
                    continue;
                }
                walk(&path, root, out)?;
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("java" | "kt" | "xml")) {
                let rel = path
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| name.clone());
                let text =
                    std::fs::read_to_string(&path).map_err(|e| format!("读不到 {}: {e}", path.display()))?;
                out.push((format!("{ROOT}/{rel}"), text));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn install() -> MockHost {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.set_project_root(Some(ROOT));
    host.add_project(ROOT);
    // Nothing here contacts a server; a fetch would mean the measurement leaked
    // into the export path.
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("scan_bench 不联网".into()))
    });
    host
}

/// Stands in for the host's search engine: one match per matching line.
///
/// Without it the scanner gets zero files back and measures nothing. Only the
/// two query shapes the scanner issues are supported: the annotation-marker
/// alternation and `load_class`'s `(class|interface|record)\s+Name\b` fallback.
fn mimic_search(
    index: &BTreeMap<String, String>,
    query: &TextQuery,
    limit: u32,
    calls: &Cell<usize>,
) -> TextSearch {
    calls.set(calls.get() + 1);
    let limit = limit.min(10_000) as usize;
    let include_kotlin = query.include.iter().any(|g| g.ends_with(".kt"));
    let name = declaration_name(&query.pattern);
    let mut matches = Vec::new();
    let mut limit_hit = false;

    'files: for (path, text) in index {
        let wanted = path.ends_with(".java") || (include_kotlin && path.ends_with(".kt"));
        if !wanted {
            continue;
        }
        let hits = match &name {
            Some(name) => declaration_offsets(text, name),
            None => marker_offsets(text),
        };
        for offset in hits {
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

/// The class name in `(class|interface|record)\s+Name\b`, when that is the query.
fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
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

fn declaration_offsets(text: &str, name: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for keyword in ["class", "interface", "record"] {
        let needle = format!("{keyword} {name}");
        let mut from = 0;
        while let Some(at) = text[from..].find(&needle) {
            let start = from + at;
            let after = start + needle.len();
            // A word boundary after the name: `Dto1` must not match `Dto12`.
            let boundary = text[after..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            if boundary {
                out.push(start);
            }
            from = after;
        }
    }
    out.sort_unstable();
    out
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
