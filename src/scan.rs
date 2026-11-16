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
#[derive(Default)]
struct Consts {
    /// Both `ITEM_QUERY` and `RouteConstant.ITEM_QUERY`.
    values: HashMap<String, String>,
    /// Qualifiers whose declaring file has already been read.
    loaded: HashSet<String>,
}

impl Consts {
    fn absorb(&mut self, model: &spring::FileModel) {
        for class in &model.classes {
            for field in &class.fields {
                let Some(value) = &field.value else { continue };
                self.values.entry(field.name.clone()).or_insert_with(|| value.clone());
                self.values.entry(format!("{}.{}", class.name, field.name)).or_insert_with(|| value.clone());
            }
        }
    }

    /// Substitutes constants in every mapping annotation of `model`.
    fn resolve_in(&mut self, project: &Project, options: &Options, model: &mut spring::FileModel) {
        let imports = model.imports.clone();
        for class in &mut model.classes {
            resolve_anns(self, project, options, &imports, &mut class.anns);
            for method in &mut class.methods {
                resolve_anns(self, project, options, &imports, &mut method.anns);
            }
        }
    }

    fn lookup(&mut self, project: &Project, options: &Options, idents: &[String], imports: &[String]) -> Option<String> {
        let name = idents.last()?;
        if let Some(value) = self.values.get(name) {
            return Some(value.clone());
        }
        // `RouteConstant.ITEM_QUERY`: the qualifier is the declaring class,
        // which usually lives in another module, so it is read on demand.
        let qualifier = idents.iter().rev().nth(1)?;
        let key = format!("{qualifier}.{name}");
        if self.loaded.insert(qualifier.clone())
            && let Some(model) = load_source(project, options, qualifier, imports)
        {
            self.absorb(&model);
        }
        self.values.get(&key).cloned()
    }
}

fn resolve_anns(
    consts: &mut Consts,
    project: &Project,
    options: &Options,
    imports: &[String],
    anns: &mut [spring::Ann],
) {
    for ann in anns {
        for arg in &mut ann.args {
            // A literal is already the route; only an identifier reference needs
            // substituting (`"a" + CONST` is left alone rather than guessed).
            if !arg.strings.is_empty() || arg.idents.is_empty() {
                continue;
            }
            if let Some(value) = consts.lookup(project, options, &arg.idents, imports) {
                arg.strings.push(value);
            }
        }
    }
}

/// The parsed source of the class `name`, preferring the file its import names.
fn load_source(project: &Project, options: &Options, name: &str, imports: &[String]) -> Option<spring::FileModel> {
    let mut candidates: Vec<String> = project.find_files(&format!("**/{name}.java"), 3).unwrap_or_default();
    if options.include_kotlin {
        candidates.extend(project.find_files(&format!("**/{name}.kt"), 3).unwrap_or_default());
    }
    let imported = imports.iter().find(|import| import.rsplit('.').next() == Some(name));
    if let Some(import) = imported {
        let tail = format!("/{}.java", import.replace('.', "/"));
        candidates.sort_by_key(|path| !path.replace('\\', "/").ends_with(&tail));
    }
    for path in candidates {
        let Ok(src) = project.read_file(&path) else { continue };
        let model = spring::parse(&src);
        if model.classes.iter().any(|class| class.name == name) {
            return Some(model);
        }
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
