//! Project scanning: finds controller files, parses them, and resolves the DTO
//! classes their request and response bodies refer to.

use std::collections::{HashMap, HashSet};

use openidea_plugin::{Error, Project, TextQuery};
use serde_json::Value;

use crate::java::{LineIndex, lex};
use crate::model::{Endpoint, FieldSpec, ParamSpec, ResolvedClass, simple_name, split_type};
use crate::spring::{self, ParamKind};

/// The HTTP verbs whose annotations mark a controller file.
///
/// Public together with the three budgets below so a benchmark can measure the
/// real pipeline with the real numbers instead of a drifting copy of them
/// (`examples/scan_bench.rs`).
pub const MARKERS: &str = "@(RestController|Controller|RequestMapping|GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|FeignClient|Path)";
/// Candidate files handed to the text search.
///
/// Bounds the *controllers* a scan parses. A project with more controllers than
/// this gets a partial route list — which is visible in the result
/// (`truncated`), so it is a stated limit rather than a silent one.
pub const FILE_LIMIT: u32 = 5_000;
/// Source files the type index may hold.
///
/// Deliberately far above `FILE_LIMIT`: the index answers "which file declares
/// this class", and a project has many more sources than controllers. When the
/// index came up short, every type it could not answer fell back to a
/// project-wide regex search — measured at ~390 ms each on a 8000-file project,
/// so 125 misses spent 49 s. Missing entries are therefore expensive, not just
/// incomplete: the index is sized so a real project fits in it.
pub const INDEX_FILE_LIMIT: u32 = 200_000;
/// Files one fallback lookup may read.
///
/// The fallback answers "where is this class declared" without `search_text`
/// (see `TypeIndex::fallback`). It narrows to files whose name resembles the
/// class, then reads them — `read_file` is ~0.31 ms, so a handful is free. The
/// cap is what keeps an ambiguous name (`Result`, `Config`, or one that appears
/// in many file names) from reading hundreds of files.
pub const FALLBACK_CANDIDATES: usize = 8;
/// Text matches asked for; one controller file yields several.
pub const MATCH_LIMIT: u32 = 40_000;
/// DTO types resolved per scan, so a cyclic or huge model cannot run away.
///
/// Reaching this is **not** a timeout: it used to share the deadline's "give up"
/// path and the same message, which made a project with more types than this
/// report "not resolved within the 5-second limit" while no clock had been read.
/// Export runs inside a `host::begin_task`, so it has 60 s rather than 5; the
/// cap now only bounds the pathological case (a cyclic or generated model),
/// which is why it sits far above the largest real project measured.
pub const TYPE_BUDGET: usize = 5_000;

#[derive(Clone, Debug)]
pub struct Options {
    pub include_kotlin: bool,
    /// How many levels of nested DTO fields to resolve.
    pub depth: u32,
    /// Resolve request/response DTOs. The endpoints provider only needs the
    /// routes, and its call budget (2 s) does not fit the extra file reads.
    pub resolve_dto: bool,
    /// Candidate controller files to parse.
    pub file_limit: u32,
    /// Read dependency jars for types the project does not contain.
    pub deps: crate::deps::Config,
    /// Restrict the scan to the controller files under this path (a file or a
    /// folder), which is what "export the current file / folder" means.
    ///
    /// Without it the narrow export read every controller in the project and
    /// only filtered the endpoints afterwards, so the expensive half — parsing
    /// each controller and walking the DTO closure they all reach — was done at
    /// full project size. The advice the refusal gives ("narrow the scope") only
    /// ever helps if it reaches the scan.
    pub only_path: Option<String>,
    /// DTO types this scan may resolve; `TYPE_BUDGET` when `None`.
    ///
    /// Not a plugin setting: production does not lower it, and the tests that
    /// pin the cap's own refusal need to reach the cap without building a
    /// project of 5 000 types.
    pub type_budget: Option<usize>,
    /// Stop collecting and return what was found once this much wall clock has
    /// passed.
    ///
    /// `None` disables the guard, which is what tests and examples want. The
    /// plugin's own entry points set it just under the host's limit: a scan
    /// that overruns is *killed* by the host, and the instance is reaped along
    /// with the in-memory scan cache, so the next call starts over and overruns
    /// again. Returning a partial result instead keeps the cache alive.
    pub budget: Option<std::time::Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            include_kotlin: true,
            depth: 2,
            resolve_dto: true,
            file_limit: FILE_LIMIT,
            deps: crate::deps::Config::default(),
            only_path: None,
            type_budget: None,
            budget: None,
        }
    }
}

/// Simple class name → the source files that may declare it, built from **one**
/// `find_files` call instead of one per type.
///
/// The scanner used to ask the host for `**/{name}.java` and `**/{name}.kt` for
/// every DTO it reached — up to `TYPE_BUDGET` × 2 project-wide walks, each a
/// synchronous host call. On a few hundred controllers that alone spent most of
/// the host's five-second limit. Matching by file name is exactly what
/// `find_files("**/{name}.java")` did, so the candidate set is the same; only
/// the number of round trips changes.
#[derive(Default)]
struct TypeIndex {
    /// File stem → paths, in the order `find_files` returned them.
    java: HashMap<String, Vec<String>>,
    kotlin: HashMap<String, Vec<String>>,
    /// Every source path, for the fallback below.
    ///
    /// The fallback used to call `search_text`, which reads every file and runs
    /// a regex over it: measured at 390–1870 ms per call on a real 8000-file
    /// project, against 0.31 ms for `read_file`. A project that reached it a
    /// couple of dozen times spent the whole call budget there and returned a
    /// fraction of its endpoints. The paths are already in hand from
    /// `find_files`, so the fallback walks them in memory and only pays
    /// `read_file` for candidates.
    all_paths: Vec<String>,
    /// Names a declaration search ran for, with what it found. An empty list
    /// means the search ran and the name is genuinely absent, so a second
    /// lookup can answer without paying for the search again.
    searched: HashMap<String, Vec<String>>,
}

impl TypeIndex {
    fn build(project: &Project, options: &Options) -> TypeIndex {
        // Index every source, not just `file_limit` of them: a miss here is not
        // merely an absent answer, it costs a fallback walk (see
        // `INDEX_FILE_LIMIT`).
        let limit = INDEX_FILE_LIMIT.max(options.file_limit);
        let java_paths = project.find_files("**/*.java", limit).unwrap_or_default();
        let java = index_of(java_paths.clone());
        let mut all_paths = java_paths;
        let kotlin = if options.include_kotlin {
            let kotlin_paths = project.find_files("**/*.kt", limit).unwrap_or_default();
            all_paths.extend(kotlin_paths.iter().cloned());
            index_of(kotlin_paths)
        } else {
            HashMap::new()
        };
        TypeIndex {
            java,
            kotlin,
            all_paths,
            searched: HashMap::new(),
        }
    }

    /// The candidate files for `name`, Java first. Empty means the index has
    /// nothing, and the caller should fall back to looking for the declaration
    /// (some projects keep a class away from its file name).
    fn candidates(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self.java.get(name).cloned().unwrap_or_default();
        out.extend(self.kotlin.get(name).cloned().unwrap_or_default());
        out
    }

    /// Where `name` is declared, when the file-name index cannot say.
    ///
    /// **Why this does not call `search_text`**: that reads every file in the
    /// project and runs a regex over it — the single most expensive host call
    /// available, measured at 390 ms to 1.9 s on a real project against 0.31 ms
    /// for `read_file`. Twenty-six of them spent a 50-second budget and left the
    /// scan with 21 of 3598 endpoints. Since `find_files` has already listed
    /// every source, the same question can be answered from that list plus cheap
    /// reads.
    ///
    /// `owner` is the qualifier the annotation used (`com.acme.RouteConstants`
    /// for `@GetMapping(RouteConstants.LIST)`), so the package is known: the
    /// declaration must live under that package's directory, which narrows 8000
    /// files to a handful. Without a qualifier the whole list is searched, still
    /// in memory.
    fn fallback(
        &mut self,
        project: &Project,
        _options: &Options,
        name: &str,
        owner: Option<&str>,
    ) -> Vec<String> {
        // One namespace for both: a bare `name` would collide with an
        // `owner#name` key if a class were ever called `a.B`.
        let key = format!("{}#{name}", owner.unwrap_or(""));
        if let Some(hit) = self.searched.get(&key) {
            return hit.clone();
        }
        let found = self.declaration_candidates(project, name, owner);
        self.searched.insert(key, found.clone());
        found
    }

    /// Files that could declare `name`, best first.
    ///
    /// Ordered by how likely each is to hold the declaration, because the caller
    /// reads them in order and stops at the first hit — and `read_file` is the
    /// only cost that matters here:
    ///
    /// 1. same package, name related (`RouteConstants` in `Routes.java` — the
    ///    class name is a prefix of the file name or the file name of it);
    /// 2. same package, any name (the class lives beside its user);
    /// 3. elsewhere, name matches.
    ///
    /// `FALLBACK_CANDIDATES` bounds the reads.
    fn declaration_candidates(
        &self,
        _project: &Project,
        name: &str,
        owner: Option<&str>,
    ) -> Vec<String> {
        // `com.acme.RouteConstants` → `com/acme`; a nested class
        // (`com.a.Outer.Inner`) belongs to `com/a` too.
        let package_dir = owner.and_then(|owner| {
            let mut parts: Vec<&str> = owner.split('.').collect();
            parts.pop();
            (!parts.is_empty()).then(|| parts.join("/"))
        });
        let lower = name.to_lowercase();
        let stem_of = |path: &str| -> String {
            path.rsplit(['/', '\\'])
                .next()
                .and_then(|file| file.rsplit_once('.').map(|(stem, _)| stem))
                .unwrap_or_default()
                .to_owned()
        };

        let mut ranked: Vec<(u8, String)> = Vec::new();
        for path in &self.all_paths {
            let normalized = path.replace('\\', "/");
            let stem = stem_of(path);
            let stem_lower = stem.to_lowercase();
            let same_package = package_dir
                .as_ref()
                .is_some_and(|dir| normalized.contains(&format!("/{dir}/")));
            // `RouteConstants` vs `Routes`: neither contains the other, so relate
            // them by their common prefix instead — that is what a project does
            // when it keeps constants apart from the class that uses them.
            let related = stem_lower == lower
                || stem_lower.contains(&lower)
                || lower.starts_with(&stem_lower)
                || common_prefix_len(&stem_lower, &lower) >= 4;
            let rank = match (same_package, related) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                // Without a package to narrow by, a renamed file is only
                // findable by reading likely ones. Keeping these last (rather
                // than dropping them) is what lets `Hidden` in
                // `SomethingElse.java` resolve when no import names it; the
                // `FALLBACK_CANDIDATES` cap keeps the reads bounded, and the
                // exact/prefix matches above stay ahead of them.
                (false, false) => 3,
            };
            ranked.push((rank, path.clone()));
        }
        ranked.sort_by(|a, b| a.0.cmp(&b.0));
        ranked
            .into_iter()
            .take(FALLBACK_CANDIDATES)
            .map(|(_, p)| p)
            .collect()
    }
}

/// How many leading characters two lowercase strings share.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Groups paths by file stem, skipping the directories a scan never parses.
fn index_of(paths: Vec<String>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for path in paths {
        let Some(stem) = path.rsplit(['/', '\\']).next().and_then(|file| {
            file.strip_suffix(".java")
                .or_else(|| file.strip_suffix(".kt"))
        }) else {
            continue;
        };
        out.entry(stem.to_owned()).or_default().push(path);
    }
    out
}

/// The DTO graph. Fields and declared type parameters travel together because
/// expanding a wrapper such as `Result<X>` needs both.
///
/// Types are keyed by simple name, which is what a field mention (`List<Request>`)
/// carries. Two projects can declare the same simple name in different packages or
/// inside different controllers, and those are different types: resolving both to
/// one entry would build one endpoint's schema from the other's fields and, because
/// export updates by `method + path`, overwrite a correct document with a wrong one.
///
/// `scopes` keeps the qualified identity alongside, keyed by
/// `package::Outer::Name`, and `ambiguous` records the simple names that turned out
/// to mean more than one type. An export refuses to send a name that is ambiguous
/// rather than guessing which one the endpoint meant.
#[derive(Clone, Debug, Default)]
pub struct DtoIndex {
    pub fields: HashMap<String, Vec<FieldSpec>>,
    pub params: HashMap<String, Vec<String>>,
    /// `package::Outer::Name` -> fields, for the types that live in a package.
    scopes: HashMap<String, Vec<FieldSpec>>,
    /// `package::Outer::Name` -> declared type parameters.
    scope_params: HashMap<String, Vec<String>>,
    /// Simple names declared by more than one type.
    ambiguous: HashSet<String>,
    /// `(endpoint scope, simple name)` -> the qualified type that file's own
    /// `import` names. Consulted before the global simple-name fallback, because
    /// two controllers in one package can import different same-named types.
    aliases: HashMap<(String, String), String>,
}

impl DtoIndex {
    /// Records a type under its simple name and its qualified one.
    ///
    /// The first writer of a simple name wins, as before. A second, *different*
    /// type claiming the same simple name marks it ambiguous instead of silently
    /// replacing the first.
    pub fn insert(
        &mut self,
        qualified: &str,
        simple: &str,
        params: Vec<String>,
        fields: Vec<FieldSpec>,
    ) {
        match self.fields.get(simple) {
            Some(existing) => {
                if *existing != fields {
                    self.ambiguous.insert(simple.to_owned());
                }
            }
            None => {
                self.fields.insert(simple.to_owned(), fields.clone());
                self.params.insert(simple.to_owned(), params.clone());
            }
        }
        self.scopes.insert(qualified.to_owned(), fields);
        self.scope_params.insert(qualified.to_owned(), params);
    }

    /// The fields of `name` as seen from `scope` (a `package::Outer::...` prefix).
    ///
    /// The endpoint's own scope is tried first, then each enclosing scope outward,
    /// then the simple name. That order is what makes `AController.Request` and
    /// `BController.Request` resolve to their own types.
    pub fn resolve_in(&self, scope: &str, name: &str) -> Option<ResolvedClass> {
        // JLS 6.4.1 orders a simple type name: member types of the enclosing
        // types, then a single-type import, then types declared in the same
        // package. Each step is tried in that order, because they genuinely
        // disagree: a controller that imports `com.alpha.Payload` while
        // `com/web/Payload.java` exists means the imported one, and resolving by
        // package first picked the wrong type and overwrote a correct document.
        let class_itself = |qualified: &str, fields: Vec<FieldSpec>| ResolvedClass {
            params: self
                .scope_params
                .get(qualified)
                .cloned()
                .unwrap_or_default(),
            fields,
            qualified: qualified.to_owned(),
        };

        // 1. Member types: the declaring class and its enclosing types, innermost
        //    first, stopping before the package. `AController.Request.Detail` beats
        //    any import of `Detail`, but a type merely *in the same package* does
        //    not: walking one level further used to land on `com.web::Payload` and
        //    return before the import was consulted.
        let mut prefix = scope;
        loop {
            let key = format!("{prefix}::{name}");
            if let Some(fields) = self.scopes.get(&key) {
                return Some(class_itself(&key, fields.clone()));
            }
            match prefix.rsplit_once("::") {
                // The last segment is the type itself; one level above it is the
                // package, and that step belongs after the import.
                Some((outer, _)) if outer.contains("::") => prefix = outer,
                _ => break,
            }
        }

        // 2. The file's own single-type import. `import com.alpha.Payload` is the
        //    source saying which type it means, and it outranks a same-package one.
        if let Some(qualified) = self.aliases.get(&(scope.to_owned(), name.to_owned()))
            && let Some(fields) = self.scopes.get(qualified)
        {
            return Some(class_itself(qualified, fields.clone()));
        }

        // 3. A type declared in the endpoint's own package.
        if let Some((package, _)) = scope.rsplit_once("::")
            && let Some(fields) = self.scopes.get(&format!("{package}::{name}"))
        {
            return Some(class_itself(&format!("{package}::{name}"), fields.clone()));
        }

        // 4. One type in the whole project answers to the name. A name with several
        //    meanings was refused above, so any single entry is the answer; the
        //    bare name is used when no qualified entry exists at all.
        if self.ambiguous.contains(name) {
            return None;
        }
        let fields = self.fields.get(name)?;
        let qualified = self
            .scopes
            .keys()
            .find(|key| {
                key.rsplit_once("::")
                    .is_some_and(|(_, simple)| simple == name)
            })
            .cloned()
            .unwrap_or_else(|| name.to_owned());
        Some(class_itself(&qualified.clone(), fields.clone()))
    }

    /// Records that files of `scope` mean `qualified` when they write `name`.
    ///
    /// `api_doc` resolves from the endpoint's scope and cannot read the file's
    /// imports, so the scan records them here: a bare `Payload` in a controller
    /// that imported `com.alpha.Payload` then means that type, and one that
    /// imported `com.beta.Payload` means the other.
    fn add_alias(&mut self, scope: &str, name: &str, qualified: &str) {
        self.aliases
            .insert((scope.to_owned(), name.to_owned()), qualified.to_owned());
    }

    /// Marks `name` as meaning more than one type.
    fn mark_ambiguous(&mut self, name: String) {
        self.ambiguous.insert(name);
    }

    /// Whether `name` means more than one type in this project.
    pub fn is_ambiguous(&self, name: &str) -> bool {
        self.ambiguous.contains(name)
    }

    /// Ambiguous simple names, sorted, for a refusal message.
    pub fn ambiguous_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.ambiguous.iter().cloned().collect();
        names.sort();
        names
    }
}

/// Why a DTO closure stopped before every reachable type was resolved.
///
/// The two reasons need different words because they need different actions: a
/// deadline is worth retrying (the next call may be faster, or the export may be
/// narrowed), while the type cap is a fixed property of the project and will
/// stop at exactly the same type every time, however often it is retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DtoStop {
    /// The wall clock ran out (`Options::budget`).
    Deadline,
    /// `TYPE_BUDGET` types were resolved while more were still queued.
    TypeBudget,
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    pub endpoints: Vec<Endpoint>,
    /// Every DTO type reachable from the endpoints, as YApi needs it.
    pub dto: DtoIndex,
    pub files: usize,
    /// The text search stopped at its limit, so some routes may be missing.
    pub truncated: bool,
    /// The wall-clock budget ran out before every controller was parsed.
    ///
    /// Distinct from `truncated`: that one means the *search* hit its limit, so
    /// files were never offered to the parse. This one means the parse ran out
    /// of time, which the caller can act on (retry, narrow the scope) while a
    /// search-limit truncation is a fixed property of the project.
    pub stopped_early: bool,
    /// Why the DTO closure stopped, or `None` when it finished.
    ///
    /// A partial graph must not be exported: schemas built from it silently
    /// lose fields, and `dto` is written to the server as if it were the whole
    /// document. `truncated` covers routes only, and a partial route list is
    /// still useful for display — a partial DTO graph is not. Export refuses it,
    /// and the reason decides what the refusal tells the user to do.
    pub dto_stop: Option<DtoStop>,
}

impl ScanResult {
    /// The closure finished, so `dto` describes every reachable type.
    ///
    /// A route-only scan (`resolve_dto: false`) produces no graph at all and is
    /// complete for its purpose; export never uses those options, and the
    /// dashboard must not report them as damage.
    pub fn dto_complete(&self) -> bool {
        self.dto_stop.is_none()
    }
}

/// `path` is `target` or inside it, whichever separator the host used.
///
/// The same test `lib::under` makes, repeated here because the scan takes the
/// path as an option rather than reaching back into the plugin module.
fn under(path: &str, target: &str) -> bool {
    let target = target.trim_end_matches(['/', '\\']);
    path == target
        || path.starts_with(&format!("{target}/"))
        || path.starts_with(&format!("{target}\\"))
}

/// Scans the project for controller routes.
pub fn scan(project: &Project, options: &Options) -> Result<ScanResult, Error> {
    scan_with_progress(project, options, |_, _| {})
}

/// `scan`, reporting DTO progress as `(resolved, reached)`.
///
/// Kept beside `scan` rather than folded into `Options` because a callback would
/// cost the option struct its `Clone` and `Debug`, which the plugin uses for its
/// shared configuration. Export passes `host::task_report`, so the call keeps
/// renewing the 60-second budget a `host::begin_task` grants.
pub fn scan_with_progress(
    project: &Project,
    options: &Options,
    mut progress: impl FnMut(usize, usize),
) -> Result<ScanResult, Error> {
    let started = std::time::Instant::now();
    // Fresh tally for this scan: the plugin's calls are single-threaded, so a
    // thread-local is enough and no signature has to carry it. The counts are
    // what transfer from `MockHost` to the real host — durations are meaningless
    // on a hash map.
    io_meter::reset();
    // One wall-clock deadline for the whole scan. `Options::default()` leaves it
    // off; the plugin's entry points set it below the host's own limit. See
    // `Options::budget` for why overrunning is worse than returning less.
    let deadline = options.budget.map(|budget| started + budget);
    let out_of_time = || deadline.is_some_and(|at| std::time::Instant::now() >= at);

    let mut include = vec!["**/*.java".to_owned()];
    if options.include_kotlin {
        include.push("**/*.kt".to_owned());
    }
    let query = TextQuery {
        pattern: MARKERS.to_owned(),
        regex: true,
        include,
        ..TextQuery::default()
    };
    let found = search_text(project, &query, MATCH_LIMIT)?;

    let mut paths: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let limit = options.file_limit.max(1);
    for m in &found.matches {
        // A narrowed scan only reads the controllers under the chosen path, so
        // both the parse and — above all — the DTO closure that follows from
        // these endpoints stay proportional to what was asked for.
        if !options
            .only_path
            .as_deref()
            .is_none_or(|path| under(&m.location.path, path))
        {
            continue;
        }
        if seen.insert(m.location.path.clone()) {
            paths.push(m.location.path.clone());
        }
        if paths.len() as u32 >= limit {
            break;
        }
    }
    paths.sort();

    // Built before any type lookup: one project-wide `find_files` per language
    // instead of one per type. The DTO closure is what makes this matter — it
    // used to issue two walks for each of `TYPE_BUDGET` types.
    let mut types = TypeIndex::build(project, options);

    let mut endpoints: Vec<Endpoint> = Vec::new();
    let mut classes: HashMap<String, ClassEntry> = HashMap::new();
    let mut sources: HashMap<String, String> = HashMap::new();
    // Simple class name → the package it was imported from. Dependency jars
    // are ranked with it, so the jar that holds `ApiEnvelope` is read first.
    // Imports are a per-file fact. Two controllers in one package can each import
    // a different `Payload`, and a project-wide table keeps only the first, so the
    // second endpoint would be built from the other's type.
    let mut hints: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut files = 0usize;
    // Parsed files are kept so route constants can be resolved across them
    // before any endpoint is built (a second pass over the sources).
    let mut parsed: Vec<(String, String, spring::FileModel)> = Vec::new();
    let mut constants = Consts::default();
    let mut stopped_early = false;

    // The controller pass is the expensive half on a large project — one
    // `read_file` per controller, and the host charges a few milliseconds each.
    // Reporting here is what keeps a task's budget alive: the host renews the
    // allowance on every report, so a scan that only reported during the (much
    // later, and on the dashboard non-existent) DTO pass would be cut off
    // exactly where it was slow. The dashboard runs `resolve_dto: false`, so
    // without this the only report is the opening one and the whole controller
    // pass runs on a budget that is never renewed.
    let controller_total = paths.len();
    for (index, path) in paths.iter().enumerate() {
        if out_of_time() {
            stopped_early = true;
            break;
        }
        // Every controller, because one slow file must not starve the rest of
        // the budget of its renewal. The controller pass is reported across the
        // first half of the bar; the DTO closure takes the second half. Two
        // phases reporting their own `done/total` into one bar makes it jump
        // backwards — the controller pass reaching ~100% and the closure then
        // starting again from 0 is what a stalled bar looked like.
        let half = controller_total.max(1);
        progress(index.min(half), half * 2);
        let src = match read_file(project, path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        files += 1;
        let model = spring::parse(&src);
        let file_hints = hints.entry(path.clone()).or_default();
        for import in &model.imports {
            let Some((package, simple)) = import.rsplit_once('.') else {
                continue;
            };
            if simple
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
            {
                file_hints
                    .entry(simple.to_owned())
                    .or_insert_with(|| package.to_owned());
            }
        }
        constants.absorb(&model);
        for class in &model.classes {
            let qualified = qualified_name(&model.package, class);
            classes
                .entry(qualified)
                .or_insert_with(|| (class.type_params.clone(), class.fields.clone()));
        }
        sources.insert(path.clone(), src.clone());
        parsed.push((path.clone(), src, model));
    }
    // The controller pass owns the first half of the bar; mark it finished so a
    // route-only scan (no closure to follow) does not appear stuck at 50%. The
    // half is `controller_total * 2`, so reaching it means "controllers done".
    progress(controller_total, controller_total * 2);

    for (path, src, mut model) in parsed {
        if out_of_time() {
            stopped_early = true;
            break;
        }
        // `@PostMapping(RouteConstant.ITEM_QUERY)` names the route instead of
        // writing it, so the constant is substituted before the routes are
        // built; otherwise every method of the controller shares path `/`.
        constants.resolve_in(project, options, &mut types, &mut model);
        let lexed = lex(&src);
        let mut index = LineIndex::new(&src);
        for raw in spring::endpoints(&model, &src) {
            let byte = lexed.toks.get(raw.at).map_or(0, |t| t.start);
            let (line, col) = index.of(byte);
            let params: Vec<(ParamSpec, ParamKind)> = raw
                .params
                .iter()
                .map(|p| {
                    (
                        ParamSpec {
                            name: p.name.clone(),
                            ty: p.ty.clone(),
                            required: p.required,
                            description: p.summary.clone(),
                        },
                        p.kind,
                    )
                })
                .collect();
            endpoints.push(Endpoint {
                method: raw.http_method.clone(),
                path: raw.path.clone(),
                framework: raw.framework.to_owned(),
                class_name: raw.class_name.clone(),
                method_name: raw.method_name.clone(),
                package: raw.package.clone(),
                file: path.clone(),
                line,
                col,
                summary: raw.summary.clone(),
                description: raw.description.clone(),
                path_params: pick(&params, ParamKind::Path),
                query_params: pick(&params, ParamKind::Query),
                header_params: pick(&params, ParamKind::Header),
                form_params: pick(&params, ParamKind::Form),
                body_type: raw.body_type.clone(),
                return_type: raw.return_type.clone(),
            });
        }
    }

    endpoints.sort_by(|a, b| a.path.cmp(&b.path).then(a.method.cmp(&b.method)));
    endpoints.dedup_by(|a, b| a.key() == b.key() && a.class_name == b.class_name);

    // `resolve_dto: false` is a route-only scan by construction: it produces no
    // DTO graph at all, so there is no partial graph to warn about. Export never
    // uses those options, and `dto_stop` stays `None` so the dashboard's
    // route-only results are not mistaken for damage.
    let (dto, dto_stop) = if options.resolve_dto && !out_of_time() {
        let mut deps = crate::deps::Deps::new(options.deps.clone());
        // The controller pass took the first half of the bar, so the closure
        // continues from there instead of restarting at zero.
        let base = controller_total * 2;
        resolve_dtos(
            project,
            options,
            &mut types,
            &endpoints,
            &classes,
            &sources,
            &mut deps,
            &hints,
            deadline,
            Some(base),
            &mut progress,
        )
    } else if options.resolve_dto {
        // No time left for the DTO closure: report the routes found so far
        // rather than losing the whole scan to the host's trap. The graph is
        // empty, which is the most partial it can be.
        stopped_early = true;
        (DtoIndex::default(), Some(DtoStop::Deadline))
    } else {
        (DtoIndex::default(), None)
    };

    // Say what the host cost, in counts and in time. On the real host this is
    // the line that explains a stalled progress bar: a DTO phase that made
    // hundreds of `search_text` calls is a different problem (missing index
    // entries) from one that made hundreds of `read_file` calls (a big closure),
    // and they need different fixes.
    let counts = io_meter::counts();
    if counts.read_file > 0 || counts.search_text > 0 {
        crate::host::log(
            openidea_plugin::LogLevel::Info,
            &counts.summary(&format!(
                "扫描 {} 个接口 / {} 个 DTO（{}）",
                endpoints.len(),
                dto.fields.len(),
                match dto_stop {
                    None => "已完成".to_owned(),
                    Some(stop) => format!("{stop:?} 停止"),
                }
            )),
        );
    }

    Ok(ScanResult {
        endpoints,
        dto,
        files,
        truncated: found.limit_hit || paths.len() as u32 >= limit,
        stopped_early,
        dto_stop,
    })
}

/// The parameters of one kind, in declaration order. `ParamKind::Cookie` is
/// deliberately never picked: neither YApi nor the upstream plugin exports
/// cookie parameters, so they stop here.
fn pick(params: &[(ParamSpec, ParamKind)], kind: ParamKind) -> Vec<ParamSpec> {
    params
        .iter()
        .filter(|(_, k)| *k == kind)
        .map(|(p, _)| p.clone())
        .collect()
}

/// Static string constants that route annotations can name instead of writing
/// the path: `@PostMapping(RouteConstant.ITEM_QUERY)`. Without them every
/// method of such a controller would be published on `/` and collapse into a
/// single record in ApiMocktle.
///
/// A constant is identified by its *fully qualified* declaring class plus the
/// field name: `com.a.Routes.PATH` and `com.b.Routes.PATH` are different
/// values, while the simple name `Routes` is only a shorthand the file's
/// imports resolve. Keying by the simple name alone let two packages collide.
/// A bare name is only used when it is unambiguous for the file — the
/// enclosing class, a static import, or a name exactly one class declares.
#[derive(Default)]
struct Consts {
    /// `com.a.Routes.PATH` → the string it assigns.
    values: HashMap<String, String>,
    /// A simple field name only one class declares → its fully qualified key.
    unique: HashMap<String, String>,
    /// Simple field names declared by more than one class with different
    /// values, so a bare reference to them means nothing on its own.
    ambiguous: HashSet<String>,
    /// Fully qualified classes whose source has already been read.
    loaded: HashSet<String>,
    /// Simple class name → the packages declaring it. A qualifier such as
    /// `Routes.PATH` is resolved through the file's imports; an import naming
    /// exactly one of these settles it, two do not.
    packages: HashMap<String, HashSet<String>>,
}

impl Consts {
    fn absorb(&mut self, model: &spring::FileModel) {
        for class in &model.classes {
            let owner = if model.package.is_empty() {
                class.name.clone()
            } else {
                format!("{}.{}", model.package, class.name)
            };
            self.packages
                .entry(class.name.clone())
                .or_default()
                .insert(model.package.clone());
            for field in &class.fields {
                let Some(value) = &field.value else { continue };
                let key = format!("{owner}.{}", field.name);
                if let Some(known) = self.values.get(&key) {
                    if known == value {
                        continue;
                    }
                    // Same class declaring the same field twice: the last
                    // initializer is not the one an annotation sees anyway.
                    self.values.insert(key, value.clone());
                    continue;
                }
                self.values.insert(key, value.clone());
                match self.unique.get(&field.name) {
                    None => {
                        self.unique
                            .insert(field.name.clone(), format!("{owner}.{}", field.name));
                    }
                    // The same name with the same value is not a conflict: it
                    // still means one thing.
                    Some(existing) if self.values.get(existing) == Some(value) => {}
                    Some(_) => {
                        self.ambiguous.insert(field.name.clone());
                    }
                }
            }
        }
    }

    /// Substitutes constants in every mapping annotation of `model`.
    fn resolve_in(
        &mut self,
        project: &Project,
        options: &Options,
        index: &mut TypeIndex,
        model: &mut spring::FileModel,
    ) {
        let imports = model.imports.clone();
        let package = model.package.clone();
        for class in &mut model.classes {
            // The controller's own class is the first place a bare name looks,
            // and it is identified by its package too.
            let owner = if package.is_empty() {
                class.name.clone()
            } else {
                format!("{package}.{}", class.name)
            };
            resolve_anns(
                self,
                project,
                options,
                index,
                &imports,
                &mut class.anns,
                Some(&owner),
            );
            for method in &mut class.methods {
                resolve_anns(
                    self,
                    project,
                    options,
                    index,
                    &imports,
                    &mut method.anns,
                    Some(&owner),
                );
            }
        }
    }

    /// `owner` is the fully qualified type the annotation is written in, if
    /// the reference is unqualified: its own `static final` wins before any
    /// import.
    fn lookup(
        &mut self,
        project: &Project,
        options: &Options,
        index: &mut TypeIndex,
        idents: &[String],
        imports: &[String],
        owner: Option<&str>,
    ) -> Option<String> {
        let name = idents.last()?;
        if idents.len() > 1 {
            // `RouteConstant.ITEM_QUERY` / `com.a.Routes.PATH`: the qualifier
            // names the declaring class, so only that class' constant may
            // answer.
            let qualifier = idents[..idents.len() - 1].join(".");
            return self.qualified_class(project, options, index, &qualifier, name, imports, owner);
        }
        // `ITEM_QUERY` on its own: the enclosing class first, then a static
        // import, then a name only one class declares.
        if let Some(owner) = owner
            && let Some(value) = self.values.get(&format!("{owner}.{name}"))
        {
            return Some(value.clone());
        }
        for owner in static_import_owners(imports, name) {
            if let Some(value) =
                self.qualified_class(project, options, index, &owner, name, imports, None)
            {
                return Some(value);
            }
        }
        if self.ambiguous.contains(name) {
            return None;
        }
        self.unique
            .get(name)
            .and_then(|key| self.values.get(key))
            .cloned()
    }

    /// Resolves a class name as it appears in an annotation (`Routes`,
    /// `com.a.Routes`, `R.Constants`) to the value of `NAME` in that class.
    #[allow(clippy::too_many_arguments)]
    fn qualified_class(
        &mut self,
        project: &Project,
        options: &Options,
        index: &mut TypeIndex,
        qualifier: &str,
        name: &str,
        imports: &[String],
        owner: Option<&str>,
    ) -> Option<String> {
        let mut tried: Vec<String> = Vec::new();
        for class in self.class_candidates(qualifier, imports, owner) {
            if tried.contains(&class) {
                continue;
            }
            if let Some(value) = self.value_of(project, options, index, &class, name, imports) {
                return Some(value);
            }
            tried.push(class);
        }
        None
    }

    /// The fully qualified classes a written qualifier may name, most likely
    /// first. `Routes` becomes `com.a.Routes` when the file imports
    /// `com.a.Routes`; an already qualified `com.b.Routes` names itself.
    fn class_candidates(
        &self,
        qualifier: &str,
        imports: &[String],
        owner: Option<&str>,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |candidate: String| {
            if !candidate.is_empty() && !out.contains(&candidate) {
                out.push(candidate);
            }
        };
        let segments: Vec<&str> = qualifier.split('.').collect();
        // A package segment is lower case, a class name is not: the qualifier
        // already carries its package, and imports have nothing to add.
        if segments[0].chars().next().is_some_and(|c| c.is_lowercase()) {
            push(qualifier.to_owned());
            return out;
        }
        // The part after the class name, prefixed with a dot — empty for a
        // simple name, so nothing like `com.a.Routes.` is ever built.
        let rest = if segments.len() > 1 {
            format!(".{}", segments[1..].join("."))
        } else {
            String::new()
        };
        // A nested class of the enclosing type: `Outer.Inner` inside `Outer`.
        // The source parser flattens class bodies, so this only ever answers
        // for a top-level class; it keeps the shape right for a future parser
        // that reads nested declarations.
        if let Some(owner) = owner
            && let Some(outer) = owner.rsplit('.').next()
            && segments[0] == outer
        {
            push(format!("{owner}{rest}"));
        }
        // The file's imports decide what a simple name means.
        for import in imports
            .iter()
            .filter(|import| !import.starts_with("static"))
        {
            if import.rsplit('.').next() == Some(segments[0]) {
                push(format!("{import}{rest}"));
            }
        }
        // A same-package class needs no import; `owner` is fully qualified, so
        // its package is the controller's own.
        match owner.and_then(|o| o.rsplit_once('.')) {
            Some((package, _)) => push(format!("{package}.{}{rest}", segments[0])),
            None => push(format!("{}{rest}", segments[0])),
        }
        // Wildcard imports, then any package the project is known to declare
        // the class in: a simple name that only exists once still resolves.
        for import in imports {
            if let Some(base) = import.strip_suffix(".*") {
                push(format!("{base}.{}", segments.join(".")));
            }
        }
        if let Some(declaring) = self.packages.get(segments[0]) {
            let mut packages: Vec<&String> = declaring.iter().collect();
            packages.sort();
            for package in packages {
                push(format!("{package}.{}", segments.join(".")));
            }
        }
        out
    }

    /// The value of `Class.NAME`, reading `Class`'s source on demand: the
    /// annotation search only returns files that declare a mapping, so a
    /// constants-only class is never among the scanned paths.
    fn value_of(
        &mut self,
        project: &Project,
        options: &Options,
        index: &mut TypeIndex,
        class: &str,
        name: &str,
        imports: &[String],
    ) -> Option<String> {
        let key = format!("{class}.{name}");
        if let Some(value) = self.values.get(&key) {
            return Some(value.clone());
        }
        if !self.loaded.insert(class.to_owned()) {
            return None;
        }
        // The file is named after the outermost class; a nested one lives
        // inside it (`com.a.Outer.Inner` → file `Outer`).
        let simple = class.rsplit('.').next().unwrap_or(class);
        let outer = class.rsplit('.').nth(1).unwrap_or(simple);
        for candidate in [simple, outer] {
            // `com.a.Routes` must not be read out of `com/b/Routes.java`: the
            // package decides which file is meant.
            let Some(model) = load_source_for(project, options, index, candidate, class, imports)
            else {
                continue;
            };
            self.absorb(&model);
            if let Some(value) = self.values.get(&key) {
                return Some(value.clone());
            }
        }
        None
    }
}

/// The classes a static import may take `name` from: `import static a.b.C.NAME`
/// and `import static a.b.C.*`. The source parser glues `static` onto the
/// package name without a separator (`staticcom.acme.b.BRoutes.PATH`), so that
/// prefix is stripped here. The full qualified name is returned — `a.b.C`, not
/// `C` — or two packages' `C` would collide here exactly as in the cache.
fn static_import_owners(imports: &[String], name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |owner: &str| {
        let owner = owner.strip_prefix("static").unwrap_or(owner);
        let owner = owner.trim_start_matches('.');
        if !owner.is_empty() && !out.iter().any(|o| o == owner) {
            out.push(owner.to_owned());
        }
    };
    for import in imports {
        let Some(rest) = import.strip_suffix(name) else {
            continue;
        };
        let Some(rest) = rest.strip_suffix('.') else {
            continue;
        };
        push(rest);
    }
    for import in imports {
        if let Some(rest) = import.strip_suffix(".*") {
            push(rest);
        }
    }
    out
}

fn resolve_anns(
    consts: &mut Consts,
    project: &Project,
    options: &Options,
    index: &mut TypeIndex,
    imports: &[String],
    anns: &mut [spring::Ann],
    class: Option<&str>,
) {
    for ann in anns {
        for arg in &mut ann.args {
            // A literal is already the route; only an identifier reference needs
            // substituting (`"a" + CONST` is left alone rather than guessed).
            if !arg.strings.is_empty() || arg.idents.is_empty() {
                continue;
            }
            if let Some(value) = consts.lookup(project, options, index, &arg.idents, imports, class)
            {
                arg.strings.push(value);
            }
        }
    }
}

/// The source declaring the class `class` (`com.a.Routes`, `Outer.Inner`),
/// whose file is named after `name`. The package has to match the file's, or
/// two packages' `Routes` would answer for each other.
fn load_source_for(
    project: &Project,
    options: &Options,
    index: &mut TypeIndex,
    name: &str,
    class: &str,
    imports: &[String],
) -> Option<spring::FileModel> {
    let mut candidates = index.candidates(name);
    if candidates.is_empty() {
        // The route-constant class is never among the scanned controllers (it
        // declares no mapping), so a miss here is normal. The name is looked up
        // through `index.fallback` so the declaration search runs once — but
        // only when that search *finds nothing* is the name remembered as
        // absent. Remembering it up front would poison the very lookup that
        // populates the candidates, and every controller after the first would
        // lose its constants.
        candidates = index.fallback(project, options, name, Some(class));
    }
    // The class' directory is the part before the simple name:
    // `com.a.Routes` → `com/a`, `Outer.Inner` → nothing to match on.
    let segments: Vec<&str> = class.split('.').collect();
    let package: &[&str] = if segments.len() > 1 {
        &segments[..segments.len() - 1]
    } else {
        &[]
    };
    let wanted = if package.is_empty() {
        None
    } else {
        Some(package.join("/"))
    };
    // The import that names this class decides, then the package directory.
    let imported_tail = imports
        .iter()
        .find(|import| import.rsplit('.').next() == Some(name))
        .map(|import| format!("/{}.java", import.replace('.', "/")));
    let rank = |path: &str| {
        let normalized = path.replace('\\', "/");
        let by_import = imported_tail
            .as_ref()
            .is_none_or(|tail| normalized.ends_with(tail));
        let by_package = wanted
            .as_ref()
            .is_none_or(|dir| normalized.contains(&format!("/{dir}/{name}.")));
        // 0 = both say yes, 1 = one does, 2 = neither: try the best first
        // rather than dropping the rest.
        (u8::from(!by_import) + u8::from(!by_package), 0u8)
    };
    candidates.sort_by_key(|path| rank(path));
    for path in candidates {
        let Ok(src) = read_file(project, &path) else {
            continue;
        };
        let model = spring::parse(&src);
        // The file has to declare the class under the package the qualifier
        // named; a same-named class in another package is a different type.
        if !model.classes.iter().any(|declared| declared.name == name) {
            continue;
        }
        if let Some(wanted) = &wanted
            && model.package.replace('.', "/") != *wanted
        {
            continue;
        }
        return Some(model);
    }
    None
}

/// Resolves the DTO closure and reports why it stopped, if it did.
///
/// `Some(DtoStop::Deadline)` and `Some(DtoStop::TypeBudget)` both mean the
/// returned graph is partial and must not be exported as a complete document,
/// but they are kept apart because only the first is worth retrying.
///
/// `progress` is called once per resolved type with how many are done and how
/// many were reached so far. Export passes `host::task_report` here: a task that
/// keeps reporting keeps its 60-second call budget, so a long closure is not cut
/// off at the first deadline while a task that goes quiet still is.
#[allow(clippy::too_many_arguments)]
fn resolve_dtos(
    project: &Project,
    options: &Options,
    types: &mut TypeIndex,
    endpoints: &[Endpoint],
    classes: &HashMap<String, ClassEntry>,
    sources: &HashMap<String, String>,
    deps: &mut crate::deps::Deps,
    hints: &HashMap<String, HashMap<String, String>>,
    deadline: Option<std::time::Instant>,
    // Where this phase starts on a shared progress bar, if it shares one.
    base: Option<usize>,
    mut progress: impl FnMut(usize, usize),
) -> (DtoIndex, Option<DtoStop>) {
    // `push_names` appends every name a type mentions, and the same name is
    // mentioned by many fields, so the raw queue is far longer than the set of
    // types it stands for — yudao's reaches ten thousand entries for a thousand
    // distinct types. Deduplicating here is what makes the queue length a usable
    // progress denominator; `tried` below already prevented duplicate *work*,
    // but a queue of duplicates made the bar's total meaningless.
    // The imports of each endpoint's own file, keyed by the endpoint's scope, so
    // both the type load below and the schema lookup later resolve a bare name the
    // way that file wrote it.
    let mut imports_of: HashMap<String, HashMap<String, String>> = HashMap::new();
    for endpoint in endpoints {
        if let Some(table) = hints.get(&endpoint.file) {
            imports_of.insert(endpoint_scope(endpoint), table.clone());
        }
    }
    let mut queue: Vec<(String, String)> = Vec::new();
    for endpoint in endpoints {
        let scope = endpoint_scope(endpoint);
        if let Some(ty) = &endpoint.body_type {
            push_names(ty, &mut queue, &scope);
        }
        push_names(&endpoint.return_type, &mut queue, &scope);
    }

    let mut dto = DtoIndex::default();
    let mut tried: HashSet<String> = HashSet::new();
    // Bare names more than one type answers to; the export refuses on any.
    let ambiguous: HashSet<String> = HashSet::new();
    let mut budget = options.type_budget.unwrap_or(TYPE_BUDGET);
    // The queue grows as types are resolved (each type adds the types its fields
    // mention), so `done + queue.len()` is not a fixed denominator: it rises as
    // the closure widens, which made the bar slide backwards. Tracking the
    // largest total seen so far and reporting `done / peak` keeps it monotonic —
    // the bar can stall, but never jump back, which is what "stuck at 80%" was.
    let mut peak_total = queue.len().max(1);
    let mut stop = None;
    while let Some((name, scope)) = queue.pop() {
        // The closure is the most expensive stage; stopping here keeps the
        // routes that were already found instead of losing the whole call. The
        // caller must still know the graph is partial, so this is reported back
        // instead of being swallowed.
        if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
            stop = Some(DtoStop::Deadline);
            break;
        }
        // De-duplicated by the qualified identity: two endpoints naming their own
        // `Request` are two different types and both must be resolved.
        // Identity is settled by, in order: the endpoint's own scope, the package
        // its imports name for this type, then a unique candidate anywhere. When
        // none of them settles it, the name is ambiguous and export refuses rather
        // than save a document built from the wrong type.
        // This endpoint's file says which package a bare name comes from; that is
        // the only thing that tells two same-named top-level types apart.
        let imported_package = imports_of.get(&scope).and_then(|t| t.get(&name)).cloned();
        // JLS 6.4.1, in the same order `DtoIndex::resolve_in` uses so the scan and
        // the schema agree: member types, then the file's single-type import, then a
        // type in the same package. A controller that imports `com.alpha.Payload`
        // while `com/web/Payload.java` exists means the imported one, so the package
        // step comes after the import.
        let same_package = scope
            .rsplit_once("::")
            .and_then(|(package, _)| classes.get_key_value(&format!("{package}::{name}")))
            .map(|(key, hit)| (key.clone(), hit.clone()));
        let scoped = scoped_lookup(classes, &scope, &name)
            .or_else(|| imported_lookup(classes, imports_of.get(&scope), &name))
            .or(same_package);
        let identity = match &scoped {
            Some((qualified, _)) => qualified.clone(),
            // With no table entry the import decides the type's identity, so two files
            // importing different same-named types are resolved separately. Without an
            // import the bare name is used, so one type reached from several endpoints
            // is resolved once and the budget goes to distinct types.
            None => match &imported_package {
                Some(package) => format!("{package}::{name}"),
                None => name.clone(),
            },
        };
        if !tried.insert(identity) {
            continue;
        }
        if budget == 0 {
            stop = Some(DtoStop::TypeBudget);
            break;
        }
        budget -= 1;
        // The endpoint's scope decides which declaration a bare name means, so the
        // scoped key is tried before the global one.
        let found = match scoped.or_else(|| class_lookup(classes, &name)) {
            // A controller from the class table: its file's imports were recorded
            // in the controller pass.
            Some((qualified, hit)) => Some((qualified, hit, HashMap::new())),
            None => {
                let lookup_scope = match &imported_package {
                    Some(package) => format!("{package}::{name}"),
                    None => scope.clone(),
                };
                load_class(project, options, types, &name, &lookup_scope, sources)
                    .map(|(path, params, fields, imports)| {
                        (package_of_path(&path, &name), (params, fields), imports)
                    })
                    .or_else(|| {
                        deps.fields(project, &name, imported_package.as_deref())
                            .map(|hit| (name.clone(), hit, HashMap::new()))
                    })
            }
        };
        let Some((qualified, (params, fields), file_imports)) = found else {
            continue;
        };
        // The type's own file imports are what its fields resolve against, so they
        // are registered under the type's identity — the scope the queue carries to
        // the next level. Without this, a DTO that lives in its own file lost its
        // imports and its fields fell back to a global simple name, which is how
        // `AlphaRequest.detail` and `BetaRequest.detail` became the same type.
        if !file_imports.is_empty() {
            imports_of
                .entry(qualified.clone())
                .or_insert_with(|| file_imports.clone());
        }
        for field in &fields {
            push_names(&field.ty, &mut queue, &qualified);
        }
        let specs: Vec<FieldSpec> = fields
            .iter()
            .filter(|f| keep_field(f))
            .map(|f| FieldSpec {
                name: wire_name(f),
                ty: f.ty.clone(),
                description: crate::spring::summary_of(&f.doc),
                children: Vec::new(),
            })
            .collect();
        // Recorded under its qualified identity so an endpoint that names the
        // same simple type resolves to its own declaration; the simple name is
        // kept for the types that carry no package.
        dto.insert(&qualified, &name, params, specs);
        // Every endpoint file that imports this name from the package it resolved
        // in gets an alias, which is what the schema pass later reads.
        for (endpoint_scope, table) in &imports_of {
            if table
                .get(&name)
                .is_some_and(|p| format!("{p}::{name}") == qualified)
            {
                dto.add_alias(endpoint_scope, &name, &qualified);
            }
        }
        // `queue` shrinks as types are resolved, so the total is what has been
        // finished plus what is still waiting. Reported across the second half of
        // the bar (`base`) so the closure does not restart it from zero after the
        // controller pass — that backwards jump is what a stalled bar looked
        // like. The base is `None` for a scan that has no controller pass to
        // share the bar with (a narrowed or library scan).
        let done = dto.fields.len();
        let total = done + queue.len();
        peak_total = peak_total.max(total);
        // Each phase owns its own half of the bar, computed independently:
        // `base`/`base*2` would let the closure's denominator (which grows with
        // the queue) shrink the controller half's percentage and jump the bar
        // backwards at the handover. Reporting the closure as
        // `50% + 50% * done/peak` keeps the two halves separate.
        match base {
            None => progress(done.min(peak_total), peak_total.max(1)),
            Some(base) => {
                let half = base / 2;
                let into = if peak_total == 0 {
                    0
                } else {
                    done.min(peak_total) * half / peak_total
                };
                progress(half + into, base);
            }
        }
    }
    for name in ambiguous {
        dto.mark_ambiguous(name);
    }
    (dto, stop)
}

/// Fields that never reach the wire format.
fn keep_field(field: &spring::Field) -> bool {
    if field.name == "serialVersionUID" || field.name.starts_with("this$") || field.ty.is_empty() {
        return false;
    }
    // Constants such as `MAX_SIZE` are not part of the payload either.
    if field.name.len() > 1
        && field
            .name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    !field.anns.iter().any(|a| {
        matches!(
            a.name.as_str(),
            "JsonIgnore" | "JsonBackReference" | "JsonIgnoreProperties"
        )
    })
}

/// The name the field has in JSON, which a Jackson/Gson annotation may change.
fn wire_name(field: &spring::Field) -> String {
    for ann in &field.anns {
        if !matches!(
            ann.name.as_str(),
            "JsonProperty" | "SerializedName" | "JSONField"
        ) {
            continue;
        }
        for key in ["value", "name"] {
            if let Some(name) = ann.strings(key).into_iter().find(|s| !s.is_empty()) {
                return name.to_owned();
            }
        }
    }
    field.name.clone()
}

/// Every class name `ty` refers to, type arguments included.
/// The DTO scope of `endpoint`: its package and declaring class.
fn endpoint_scope(endpoint: &Endpoint) -> String {
    if endpoint.package.is_empty() {
        endpoint.class_name.clone()
    } else {
        format!("{}::{}", endpoint.package, endpoint.class_name)
    }
}

/// The class `name` means through the import that names it.
///
/// `import com.alpha.Payload;` is the project saying which `Payload` a file means,
/// so it settles a case the package scope cannot: the type lives in another
/// package entirely. The hint is per project rather than per file, so it only
/// answers when the name was imported exactly once.
fn imported_lookup(
    classes: &HashMap<String, ClassEntry>,
    hints: Option<&HashMap<String, String>>,
    name: &str,
) -> Option<(String, ClassEntry)> {
    let package = hints?.get(name)?;
    let key = format!("{package}::{name}");
    classes
        .get_key_value(&key)
        .map(|(key, hit)| (key.clone(), hit.clone()))
}

/// The class `name` means inside `scope`, walking the scope outward.
///
/// `com.acme::AController` then `com.acme` then the bare name: a nested type in
/// the controller wins, then one in its package, then a unique one anywhere.
fn scoped_lookup(
    classes: &HashMap<String, ClassEntry>,
    scope: &str,
    name: &str,
) -> Option<(String, ClassEntry)> {
    // Member types only: the declaring class and its enclosing types. The package
    // is NOT walked here — a single-type import outranks a type declared in the
    // same package (JLS 6.4.1), so imported_lookup has to run before the
    // package step, and walking into the package here would pre-empt it.
    let mut prefix = scope;
    let mut first = true;
    loop {
        let key = format!("{prefix}::{name}");
        if let Some(hit) = classes.get_key_value(&key) {
            return Some((key, hit.1.clone()));
        }
        match prefix.rsplit_once("::") {
            Some((outer, _)) => {
                // Stop before the package: one :: level above the class body is
                // the package, and that step belongs after the import.
                if !first && !outer.contains("::") {
                    break;
                }
                prefix = outer;
            }
            None => break,
        }
        first = false;
    }
    None
}

/// A parsed class: its declared type parameters and its fields.
type ClassEntry = (Vec<String>, Vec<spring::Field>);

/// A class loaded from its source file: the file it came from, its declared type
/// parameters, its fields, and the imports that file declares (which its own fields
/// resolve against).
type LoadedClass = (
    String,
    Vec<String>,
    Vec<spring::Field>,
    HashMap<String, String>,
);

/// Every class `name` could mean, as `(qualified key, entry)`.
///
/// A qualified key answers with itself. A bare simple name can match several
/// packages or controllers, and which one an endpoint means is not decidable from
/// the name alone when none of them is in the endpoint's own scope — that is the
/// case an export must refuse instead of guessing.
fn class_candidates(
    classes: &HashMap<String, ClassEntry>,
    name: &str,
) -> Vec<(String, ClassEntry)> {
    if let Some(hit) = classes.get_key_value(name) {
        return vec![(name.to_owned(), hit.1.clone())];
    }
    let suffix = format!("::{name}");
    let mut found: Vec<(String, ClassEntry)> = classes
        .iter()
        .filter(|(key, _)| key.ends_with(&suffix))
        .map(|(key, hit)| (key.clone(), hit.clone()))
        .collect();
    // HashMap order is arbitrary; sorting keeps the chosen type and the refusal
    // message stable between runs.
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// The one class `name` means, or `None` when it is missing or ambiguous.
fn class_lookup(classes: &HashMap<String, ClassEntry>, name: &str) -> Option<(String, ClassEntry)> {
    let mut candidates = class_candidates(classes, name);
    if candidates.len() == 1 {
        return candidates.pop();
    }
    None
}

/// The qualified name of the class `name` declared in `path`.
///
/// A source file lives under a package directory, so the package is what follows
/// the last source root in the path: `.../src/main/java/com/alpha/Payload.java`
/// yields `com.alpha::Payload`. Running past the root instead would make the
/// project directory part of every package name, and then no scope would ever
/// match.
fn package_of_path(path: &str, name: &str) -> String {
    let normalised = path.replace('\\', "/");
    let segments: Vec<&str> = normalised.split('/').collect();
    // The package starts after the last `java`/`kotlin` source root, or after
    // `src` when the layout has no such level.
    let root = segments
        .iter()
        .rposition(|s| matches!(*s, "java" | "kotlin"))
        .or_else(|| segments.iter().rposition(|s| *s == "src"));
    let package: Vec<&str> = match root {
        Some(at) => segments[at + 1..]
            .iter()
            .copied()
            .filter(|s| !s.is_empty() && !s.ends_with(".java") && !s.ends_with(".kt"))
            .collect(),
        None => Vec::new(),
    };
    if package.is_empty() {
        return name.to_owned();
    }
    format!("{}::{name}", package.join("."))
}

/// Whether `path` lives in the directory of `scope`'s package.
///
/// `scope` is `com.acme::Ctrl`, so the package directory is `com/acme`. A type the
/// endpoint names without a qualifier is the one in its own package when there is
/// one; a same-named file elsewhere is a different type.
fn path_in_scope(path: &str, scope: &str) -> bool {
    let Some((package, _)) = scope.rsplit_once("::") else {
        return false;
    };
    let dir = package.replace('.', "/");
    let normalised = path.replace('\\', "/");
    normalised.contains(&format!("/{dir}/")) || normalised.contains(&format!("/{dir}::"))
}

/// The identity of a class: its package, the type that declares it, and its name.
///
/// `com.acme::AController::Request` and `com.acme::BController::Request` are two
/// different types that a field mention spells the same way. Keeping the qualified
/// name is what lets the scan resolve each endpoint against its own declaration
/// instead of whichever came first.
fn qualified_name(package: &str, class: &spring::Class) -> String {
    let mut out = String::new();
    if !package.is_empty() {
        out.push_str(package);
        out.push_str("::");
    }
    if !class.outer.is_empty() {
        out.push_str(&class.outer);
        out.push_str("::");
    }
    out.push_str(&class.name);
    out
}

fn push_names(ty: &str, out: &mut Vec<(String, String)>, scope: &str) {
    let (base, args) = split_type(ty);
    let base = simple_name(&base);
    if !base.is_empty() && base.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        out.push((base, scope.to_owned()));
    }
    for arg in args {
        push_names(&arg, out, scope);
    }
}

/// Counts the host calls a scan makes, so a slow one can be explained instead of
/// guessed at.
///
/// The absolute durations mean nothing on `MockHost` (which answers from a hash
/// map), but the *counts* transfer to the real host: a DTO that costs three
/// `read_file` calls is three times as expensive there as one that costs one.
/// A thread-local keeps this out of every function signature; a scan runs on one
/// thread, and the plugin's calls are single-threaded.
mod io_meter {
    use std::cell::RefCell;

    #[derive(Default, Clone, Copy)]
    pub struct Counts {
        pub read_file: usize,
        pub read_file_ns: u128,
        pub search_text: usize,
        pub search_text_ns: u128,
    }

    thread_local! {
        static COUNTS: RefCell<Counts> = const { RefCell::new(Counts {
            read_file: 0,
            read_file_ns: 0,
            search_text: 0,
            search_text_ns: 0,
        }) };
    }

    /// Starts a fresh tally and returns the one that was there.
    pub fn reset() -> Counts {
        COUNTS.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }

    pub fn counts() -> Counts {
        COUNTS.with(|c| *c.borrow())
    }

    pub fn add_read_file(ns: u128) {
        COUNTS.with(|c| {
            let mut c = c.borrow_mut();
            c.read_file += 1;
            c.read_file_ns += ns;
        });
    }

    pub fn add_search_text(ns: u128) {
        COUNTS.with(|c| {
            let mut c = c.borrow_mut();
            c.search_text += 1;
            c.search_text_ns += ns;
        });
    }

    impl Counts {
        /// One line naming what the scan spent on the host.
        pub fn summary(&self, stage: &str) -> String {
            format!(
                "ApiMocktle: {} — read_file {} 次（{:.1} ms），search_text {} 次（{:.1} ms）",
                stage,
                self.read_file,
                self.read_file_ns as f64 / 1e6,
                self.search_text,
                self.search_text_ns as f64 / 1e6
            )
        }
    }
}

/// `project.read_file`, counted for the scan summary.
fn read_file(project: &Project, path: &str) -> Result<String, Error> {
    let started = std::time::Instant::now();
    let out = project.read_file(path);
    io_meter::add_read_file(started.elapsed().as_nanos());
    out
}

/// `project.search_text`, counted for the scan summary.
fn search_text(
    project: &Project,
    query: &TextQuery,
    limit: u32,
) -> Result<openidea_plugin::TextSearch, Error> {
    let started = std::time::Instant::now();
    let out = project.search_text(query, limit);
    io_meter::add_search_text(started.elapsed().as_nanos());
    out
}

/// The names a type mentions, resolved through the index.
fn load_class(
    project: &Project,
    options: &Options,
    index: &mut TypeIndex,
    name: &str,
    scope: &str,
    sources: &HashMap<String, String>,
) -> Option<LoadedClass> {
    let mut candidates = index.candidates(name);
    // A same-named file in the endpoint's own package is the type it means; without
    // this, two `Payload.java` in different packages resolved to whichever the index
    // listed first and the other endpoint got the wrong fields.
    candidates.sort_by_key(|path| !path_in_scope(path, scope));
    if candidates.is_empty() {
        // Some projects keep a class away from its file name; search for the
        // declaration instead of guessing a path. `fallback` caches the result,
        // so a type that genuinely does not exist costs one search, not one per
        // endpoint that mentions it.
        candidates = index.fallback(project, options, name, None);
    }
    for path in candidates {
        let src = match sources.get(&path) {
            Some(src) => src.clone(),
            None => match read_file(project, &path) {
                Ok(src) => src,
                Err(_) => continue,
            },
        };
        let model = spring::parse(&src);
        if let Some(class) = model.classes.iter().find(|c| c.name == name) {
            // This file's imports: its own fields resolve against them, and no
            // other pass has parsed this file.
            let mut file_imports: HashMap<String, String> = HashMap::new();
            for import in &model.imports {
                let Some((package, simple)) = import.rsplit_once('.') else {
                    continue;
                };
                if simple
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                {
                    file_imports
                        .entry(simple.to_owned())
                        .or_insert_with(|| package.to_owned());
                }
            }
            return Some((
                path.clone(),
                class.type_params.clone(),
                class.fields.clone(),
                file_imports,
            ));
        }
    }
    None
}

/// The dashboard's JSON view of a scan.
pub fn to_json(result: &ScanResult) -> Value {
    serde_json::json!({
        "count": result.endpoints.len(),
        "files": result.files,
        "truncated": result.truncated,
        "stoppedEarly": result.stopped_early,
        "dtoComplete": result.dto_complete(),
        "items": result.endpoints.iter().map(|e| serde_json::json!({
            "method": e.method.clone().unwrap_or_else(|| "ANY".into()),
            "path": e.path,
            "summary": e.summary,
            "handler": e.handler(),
            "class": e.class_name,
            "framework": e.framework,
            "file": e.file,
            "line": e.line + 1,
            "params": e.query_params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "body": e.body_type,
        })).collect::<Vec<_>>(),
    })
}
