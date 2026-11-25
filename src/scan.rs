//! Project scanning: finds controller files, parses them, and resolves the DTO
//! classes their request and response bodies refer to.

use std::collections::{HashMap, HashSet};

use openidea_plugin::{Error, Project, TextQuery};
use serde_json::Value;

use crate::java::{LineIndex, lex};
use crate::model::{Endpoint, FieldSpec, ParamSpec, simple_name, split_type};
use crate::spring::{self, ParamKind};

/// The HTTP verbs whose annotations mark a controller file.
///
/// Public together with the three budgets below so a benchmark can measure the
/// real pipeline with the real numbers instead of a drifting copy of them
/// (`examples/scan_bench.rs`).
pub const MARKERS: &str = "@(RestController|Controller|RequestMapping|GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|FeignClient|Path)";
/// Candidate files handed to the text search.
pub const FILE_LIMIT: u32 = 5_000;
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
    /// Names a declaration search ran for, with what it found. An empty list
    /// means the search ran and the name is genuinely absent, so a second
    /// lookup can answer without paying for the search again.
    ///
    /// This has to remember the *result*, not merely that a search happened:
    /// the type index is built from file names, so a class kept in a file named
    /// otherwise is never in it — yet it exists, and caching "already searched"
    /// would make only the first controller that mentions it work.
    searched: HashMap<String, Vec<String>>,
}

impl TypeIndex {
    fn build(project: &Project, options: &Options) -> TypeIndex {
        let limit = options.file_limit.max(1);
        let java = index_of(project.find_files("**/*.java", limit).unwrap_or_default());
        let kotlin = if options.include_kotlin {
            index_of(project.find_files("**/*.kt", limit).unwrap_or_default())
        } else {
            HashMap::new()
        };
        TypeIndex { java, kotlin, searched: HashMap::new() }
    }

    /// The candidate files for `name`, Java first. Empty means the index has
    /// nothing, and the caller should fall back to searching for the
    /// declaration (some projects keep a class away from its file name).
    fn candidates(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self.java.get(name).cloned().unwrap_or_default();
        out.extend(self.kotlin.get(name).cloned().unwrap_or_default());
        out
    }

    /// The declaration search, run at most once per name and cached by result.
    fn fallback(&mut self, project: &Project, options: &Options, name: &str) -> Vec<String> {
        if let Some(hit) = self.searched.get(name) {
            return hit.clone();
        }
        let found = declaration_candidates(project, options, name);
        self.searched.insert(name.to_owned(), found.clone());
        found
    }
}

/// Groups paths by file stem, skipping the directories a scan never parses.
fn index_of(paths: Vec<String>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for path in paths {
        let Some(stem) = path
            .rsplit(['/', '\\'])
            .next()
            .and_then(|file| file.strip_suffix(".java").or_else(|| file.strip_suffix(".kt")))
        else {
            continue;
        };
        out.entry(stem.to_owned()).or_default().push(path);
    }
    out
}

/// The DTO graph. Fields and declared type parameters travel together because
/// expanding a wrapper such as `Result<X>` needs both.
#[derive(Clone, Debug, Default)]
pub struct DtoIndex {
    pub fields: HashMap<String, Vec<FieldSpec>>,
    pub params: HashMap<String, Vec<String>>,
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
    path == target || path.starts_with(&format!("{target}/")) || path.starts_with(&format!("{target}\\"))
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
        if !options.only_path.as_deref().is_none_or(|path| under(&m.location.path, path)) {
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
    let mut classes: HashMap<String, (Vec<String>, Vec<spring::Field>)> = HashMap::new();
    let mut sources: HashMap<String, String> = HashMap::new();
    // Simple class name → the package it was imported from. Dependency jars
    // are ranked with it, so the jar that holds `ApiEnvelope` is read first.
    let mut hints: HashMap<String, String> = HashMap::new();
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
        for import in &model.imports {
            let Some((package, simple)) = import.rsplit_once('.') else { continue };
            if simple.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                hints.entry(simple.to_owned()).or_insert_with(|| package.to_owned());
            }
        }
        constants.absorb(&model);
        for class in &model.classes {
            classes
                .entry(class.name.clone())
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
    params.iter().filter(|(_, k)| *k == kind).map(|(p, _)| p.clone()).collect()
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
    fn resolve_in(&mut self, project: &Project, options: &Options, index: &mut TypeIndex, model: &mut spring::FileModel) {
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
            resolve_anns(self, project, options, index, &imports, &mut class.anns, Some(&owner));
            for method in &mut class.methods {
                resolve_anns(self, project, options, index, &imports, &mut method.anns, Some(&owner));
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
            if let Some(value) = self.qualified_class(project, options, index, &owner, name, imports, None) {
                return Some(value);
            }
        }
        if self.ambiguous.contains(name) {
            return None;
        }
        self.unique.get(name).and_then(|key| self.values.get(key)).cloned()
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
    fn class_candidates(&self, qualifier: &str, imports: &[String], owner: Option<&str>) -> Vec<String> {
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
            let Some(model) = load_source_for(project, options, index, candidate, class, imports) else { continue };
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
        let Some(rest) = import.strip_suffix(name) else { continue };
        let Some(rest) = rest.strip_suffix('.') else { continue };
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
            if let Some(value) = consts.lookup(project, options, index, &arg.idents, imports, class) {
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
        candidates = index.fallback(project, options, name);
    }
    // The class' directory is the part before the simple name:
    // `com.a.Routes` → `com/a`, `Outer.Inner` → nothing to match on.
    let segments: Vec<&str> = class.split('.').collect();
    let package: &[&str] = if segments.len() > 1 { &segments[..segments.len() - 1] } else { &[] };
    let wanted = if package.is_empty() { None } else { Some(package.join("/")) };
    // The import that names this class decides, then the package directory.
    let imported_tail = imports
        .iter()
        .find(|import| import.rsplit('.').next() == Some(name))
        .map(|import| format!("/{}.java", import.replace('.', "/")));
    let rank = |path: &str| {
        let normalized = path.replace('\\', "/");
        let by_import = imported_tail.as_ref().is_none_or(|tail| normalized.ends_with(tail));
        let by_package = wanted
            .as_ref()
            .is_none_or(|dir| normalized.contains(&format!("/{dir}/{name}.")));
        // 0 = both say yes, 1 = one does, 2 = neither: try the best first
        // rather than dropping the rest.
        (u8::from(!by_import) + u8::from(!by_package), 0u8)
    };
    candidates.sort_by_key(|path| rank(path));
    for path in candidates {
        let Ok(src) = read_file(project, &path) else { continue };
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
    classes: &HashMap<String, (Vec<String>, Vec<spring::Field>)>,
    sources: &HashMap<String, String>,
    deps: &mut crate::deps::Deps,
    hints: &HashMap<String, String>,
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
    let mut queue: Vec<String> = Vec::new();
    for endpoint in endpoints {
        if let Some(ty) = &endpoint.body_type {
            push_names(ty, &mut queue);
        }
        push_names(&endpoint.return_type, &mut queue);
    }

    let mut dto = DtoIndex::default();
    let mut tried: HashSet<String> = HashSet::new();
    let mut budget = options.type_budget.unwrap_or(TYPE_BUDGET);
    // The queue grows as types are resolved (each type adds the types its fields
    // mention), so `done + queue.len()` is not a fixed denominator: it rises as
    // the closure widens, which made the bar slide backwards. Tracking the
    // largest total seen so far and reporting `done / peak` keeps it monotonic —
    // the bar can stall, but never jump back, which is what "stuck at 80%" was.
    let mut peak_total = queue.len().max(1);
    let mut stop = None;
    while let Some(name) = queue.pop() {
        // The closure is the most expensive stage; stopping here keeps the
        // routes that were already found instead of losing the whole call. The
        // caller must still know the graph is partial, so this is reported back
        // instead of being swallowed.
        if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
            stop = Some(DtoStop::Deadline);
            break;
        }
        if !tried.insert(name.clone()) {
            continue;
        }
        if budget == 0 {
            stop = Some(DtoStop::TypeBudget);
            break;
        }
        budget -= 1;
        let found = match classes.get(&name) {
            Some(hit) => Some(hit.clone()),
            None => load_class(project, options, types, &name, sources)
                .or_else(|| deps.fields(project, &name, hints.get(&name).map(String::as_str))),
        };
        let Some((params, fields)) = found else { continue };
        for field in &fields {
            push_names(&field.ty, &mut queue);
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
        dto.params.insert(name.clone(), params);
        dto.fields.insert(name, specs);
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
                let into = if peak_total == 0 { 0 } else { done.min(peak_total) * half / peak_total };
                progress(half + into, base);
            }
        }
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
        && field.name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    !field
        .anns
        .iter()
        .any(|a| matches!(a.name.as_str(), "JsonIgnore" | "JsonBackReference" | "JsonIgnoreProperties"))
}

/// The name the field has in JSON, which a Jackson/Gson annotation may change.
fn wire_name(field: &spring::Field) -> String {
    for ann in &field.anns {
        if !matches!(ann.name.as_str(), "JsonProperty" | "SerializedName" | "JSONField") {
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
fn push_names(ty: &str, out: &mut Vec<String>) {
    let (base, args) = split_type(ty);
    let base = simple_name(&base);
    if !base.is_empty() && base.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        out.push(base);
    }
    for arg in args {
        push_names(&arg, out);
    }
}

/// The declaration search a class away from its file name falls back to.
fn declaration_candidates(project: &Project, options: &Options, name: &str) -> Vec<String> {
    let query = TextQuery {
        pattern: format!(r"(class|interface|record)\s+{name}\b"),
        regex: true,
        include: if options.include_kotlin {
            vec!["**/*.java".to_owned(), "**/*.kt".to_owned()]
        } else {
            vec!["**/*.java".to_owned()]
        },
        ..TextQuery::default()
    };
    let mut out = Vec::new();
    if let Ok(found) = search_text(project, &query, 8) {
        let mut seen = HashSet::new();
        for hit in found.matches {
            if seen.insert(hit.location.path.clone()) {
                out.push(hit.location.path);
            }
        }
    }
    out
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
    sources: &HashMap<String, String>,
) -> Option<(Vec<String>, Vec<spring::Field>)> {
    let mut candidates = index.candidates(name);
    if candidates.is_empty() {
        // Some projects keep a class away from its file name; search for the
        // declaration instead of guessing a path. `fallback` caches the result,
        // so a type that genuinely does not exist costs one search, not one per
        // endpoint that mentions it.
        candidates = index.fallback(project, options, name);
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
            return Some((class.type_params.clone(), class.fields.clone()));
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
