//! Project scanning: finds controller files, parses them, and resolves the DTO
//! classes their request and response bodies refer to.

use std::collections::{HashMap, HashSet};

use openidea_plugin::{Error, Project, TextQuery};
use serde_json::Value;

use crate::java::{LineIndex, lex};
use crate::model::{Endpoint, FieldSpec, ParamSpec, simple_name, split_type};
use crate::spring::{self, ParamKind};

/// The HTTP verbs whose annotations mark a controller file.
const MARKERS: &str = "@(RestController|Controller|RequestMapping|GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|FeignClient|Path)";
/// Candidate files handed to the text search.
const FILE_LIMIT: u32 = 5_000;
/// Text matches asked for; one controller file yields several.
const MATCH_LIMIT: u32 = 40_000;
/// DTO types resolved per scan, so a cyclic or huge model cannot run away.
const TYPE_BUDGET: usize = 400;

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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            include_kotlin: true,
            depth: 2,
            resolve_dto: true,
            file_limit: FILE_LIMIT,
            deps: crate::deps::Config::default(),
        }
    }
}

/// The DTO graph. Fields and declared type parameters travel together because
/// expanding a wrapper such as `Result<X>` needs both.
#[derive(Clone, Debug, Default)]
pub struct DtoIndex {
    pub fields: HashMap<String, Vec<FieldSpec>>,
    pub params: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    pub endpoints: Vec<Endpoint>,
    /// Every DTO type reachable from the endpoints, as YApi needs it.
    pub dto: DtoIndex,
    pub files: usize,
    /// The text search stopped at its limit, so some routes may be missing.
    pub truncated: bool,
}

/// Scans the project for controller routes.
pub fn scan(project: &Project, options: &Options) -> Result<ScanResult, Error> {
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
    let found = project.search_text(&query, MATCH_LIMIT)?;

    let mut paths: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let limit = options.file_limit.max(1);
    for m in &found.matches {
        if seen.insert(m.location.path.clone()) {
            paths.push(m.location.path.clone());
        }
        if paths.len() as u32 >= limit {
            break;
        }
    }
    paths.sort();

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

    for path in &paths {
        let src = match project.read_file(path) {
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

    for (path, src, mut model) in parsed {
        // `@PostMapping(RouteConstant.ITEM_QUERY)` names the route instead of
        // writing it, so the constant is substituted before the routes are
        // built; otherwise every method of the controller shares path `/`.
        constants.resolve_in(project, options, &mut model);
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

    let dto = if options.resolve_dto {
        let mut deps = crate::deps::Deps::new(options.deps.clone());
        resolve_dtos(project, options, &endpoints, &classes, &sources, &mut deps, &hints)
    } else {
        DtoIndex::default()
    };

    Ok(ScanResult {
        endpoints,
        dto,
        files,
        truncated: found.limit_hit || paths.len() as u32 >= limit,
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
    fn resolve_in(&mut self, project: &Project, options: &Options, model: &mut spring::FileModel) {
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
            resolve_anns(self, project, options, &imports, &mut class.anns, Some(&owner));
            for method in &mut class.methods {
                resolve_anns(self, project, options, &imports, &mut method.anns, Some(&owner));
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
            return self.qualified_class(project, options, &qualifier, name, imports, owner);
        }
        // `ITEM_QUERY` on its own: the enclosing class first, then a static
        // import, then a name only one class declares.
        if let Some(owner) = owner
            && let Some(value) = self.values.get(&format!("{owner}.{name}"))
        {
            return Some(value.clone());
        }
        for owner in static_import_owners(imports, name) {
            if let Some(value) = self.qualified_class(project, options, &owner, name, imports, None) {
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
    fn qualified_class(
        &mut self,
        project: &Project,
        options: &Options,
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
            if let Some(value) = self.value_of(project, options, &class, name, imports) {
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
            let Some(model) = load_source_for(project, options, candidate, class, imports) else { continue };
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
            if let Some(value) = consts.lookup(project, options, &arg.idents, imports, class) {
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
    name: &str,
    class: &str,
    imports: &[String],
) -> Option<spring::FileModel> {
    let mut candidates: Vec<String> = project.find_files(&format!("**/{name}.java"), 3).unwrap_or_default();
    if options.include_kotlin {
        candidates.extend(project.find_files(&format!("**/{name}.kt"), 3).unwrap_or_default());
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
        let Ok(src) = project.read_file(&path) else { continue };
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

/// Loads every type the endpoints mention, then every type those mention.
fn resolve_dtos(
    project: &Project,
    options: &Options,
    endpoints: &[Endpoint],
    classes: &HashMap<String, (Vec<String>, Vec<spring::Field>)>,
    sources: &HashMap<String, String>,
    deps: &mut crate::deps::Deps,
    hints: &HashMap<String, String>,
) -> DtoIndex {
    let mut queue: Vec<String> = Vec::new();
    for endpoint in endpoints {
        if let Some(ty) = &endpoint.body_type {
            push_names(ty, &mut queue);
        }
        push_names(&endpoint.return_type, &mut queue);
    }

    let mut dto = DtoIndex::default();
    let mut tried: HashSet<String> = HashSet::new();
    let mut budget = TYPE_BUDGET;
    while let Some(name) = queue.pop() {
        if !tried.insert(name.clone()) {
            continue;
        }
        if budget == 0 {
            break;
        }
        budget -= 1;
        let found = match classes.get(&name) {
            Some(hit) => Some(hit.clone()),
            None => load_class(project, options, &name, sources)
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
    }
    dto
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

fn load_class(
    project: &Project,
    options: &Options,
    name: &str,
    sources: &HashMap<String, String>,
) -> Option<(Vec<String>, Vec<spring::Field>)> {
    let mut candidates: Vec<String> = project.find_files(&format!("**/{name}.java"), 3).unwrap_or_default();
    if options.include_kotlin {
        candidates.extend(project.find_files(&format!("**/{name}.kt"), 3).unwrap_or_default());
    }
    if candidates.is_empty() {
        // Some projects keep a class away from its file name; search for the
        // declaration instead of guessing a path.
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
        if let Ok(found) = project.search_text(&query, 8) {
            let mut seen = HashSet::new();
            for hit in found.matches {
                if seen.insert(hit.location.path.clone()) {
                    candidates.push(hit.location.path);
                }
            }
        }
    }
    for path in candidates {
        let src = match sources.get(&path) {
            Some(src) => src.clone(),
            None => match project.read_file(&path) {
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
