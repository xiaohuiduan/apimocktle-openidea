//! Resolves DTOs that live in dependency jars.
//!
//! A plugin can only read files inside the project, so a wrapper such as
//! `ApiEnvelope<T>` — packaged in a shared `common` artifact — had no fields
//! to show. This module closes that gap: it finds the local Maven repository,
//! reads the project's POMs (and their transitive POMs) to learn the jar
//! paths, then reads and parses the one `.class` that declares the missing
//! type. Access outside the project needs the `fs.read:<path>` capability; when
//! the user has not granted it, the resolver quietly turns itself off.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use openidea_plugin::{Error, LogLevel, NotificationLevel, Project, host};

use crate::classfile::{self, ClassFile, JarField};
use crate::spring::Field;
use crate::zip::Zip;

/// One notification per component instance is enough; a scan loop would
/// otherwise repeat it for every missing type.
static WARNED: AtomicBool = AtomicBool::new(false);

/// Cap on jars read for one scan, and on their total size: a scan runs inside
/// the host's five-second command budget.
const MAX_READS: usize = 120;
const MAX_BYTES: usize = 64 << 20;
/// One jar bigger than this is skipped; a class file never needs a fat jar.
const MAX_JAR: usize = 12 << 20;
/// Dependency POMs walked for the jar list.
const MAX_POMS: usize = 200;
/// Inheritance chain depth.
const MAX_SUPER: usize = 6;
/// Jar reads one class may spend: a class that no jar has must not eat the
/// whole scan's budget before the next class gets a chance.
const MAX_CLASS_READS: usize = 32;
/// Classes never resolved from a jar even when a jar contains them.
const SKIP: &[&str] = &["Object", "String", "Integer", "Long", "Boolean", "Double", "Float", "List", "Map"];

/// Settings that decide whether and where jars are looked for.
#[derive(Clone, Debug)]
pub struct Config {
    pub enabled: bool,
    /// Maven local repository, `~/.m2/repository` style.
    pub repo: String,
}

/// Maven's default, and the path the manifest asks to read. A repository
/// elsewhere needs its own `fs.read:<path>` in `plugin.toml`: the host grants
/// file access at install time and has no way to ask for it later.
pub const DEFAULT_REPO: &str = "~/.m2/repository";

impl Default for Config {
    fn default() -> Self {
        Config { enabled: false, repo: DEFAULT_REPO.to_owned() }
    }
}

/// The repository to read: what the settings page says, else Maven's default.
pub fn repository() -> String {
    host::setting_string("mavenRepository")
        .map(|path| path.trim().replace('\\', "/"))
        .map(|path| path.trim_end_matches('/').to_owned())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_owned())
}

/// One class as the resolver found it, with inherited fields merged in.
#[derive(Clone, Debug, Default)]
pub struct Shape {
    pub params: Vec<String>,
    pub fields: Vec<JarField>,
}

/// A jar to look in: project jars are read through `project.read`, everything
/// from the local Maven repository through `fs.read`. The coordinates let the
/// resolver put the most likely jar first — the one whose group matches the
/// package the class is imported from.
#[derive(Clone, Debug)]
struct JarRef {
    path: String,
    external: bool,
    group: Option<String>,
    artifact: Option<String>,
}

/// The per-scan jar resolver. State is intentionally local to a scan: the
/// dependency list only changes when the project does, and a plugin instance
/// keeps its scan cache anyway.
pub struct Deps {
    cfg: Config,
    jars: Option<Vec<JarRef>>,
    /// The project's own Maven group (`com.acme`), used to rank jars when a
    /// class has no import to tell us where it comes from.
    home: Option<String>,
    shapes: HashMap<String, Option<Shape>>,
    /// Class entries of the jars read during this scan: path → (simple class
    /// name → entry name). `None` marks a jar that could not be read.
    index: HashMap<String, Option<HashMap<String, String>>>,
    reads: usize,
    bytes: usize,
    denied: bool,
    /// Read budget for the lookup in progress. A scan runs inside the host's
    /// five-second command budget, so a deep chain has to stop early.
    limit: usize,
}

impl Deps {
    pub fn new(cfg: Config) -> Self {
        // The host wants `/` separators even on Windows.
        let repo = cfg.repo.trim().replace('\\', "/");
        let repo = repo.trim_end_matches('/').to_owned();
        Deps {
            cfg: Config { enabled: cfg.enabled, repo },
            jars: None,
            home: None,
            shapes: HashMap::new(),
            index: HashMap::new(),
            reads: 0,
            bytes: 0,
            denied: false,
            limit: MAX_READS,
        }
    }

    /// The fields of `name`, or `None` when no jar has it (or the budget is
    /// spent, or the capability is missing). `imported_from` is the package the
    /// source imported the class from, when known.
    pub fn fields(
        &mut self,
        project: &Project,
        name: &str,
        imported_from: Option<&str>,
    ) -> Option<(Vec<String>, Vec<Field>)> {
        if !self.cfg.enabled || SKIP.contains(&name) {
            return None;
        }
        let shape = match self.shapes.get(name) {
            Some(cached) => cached.clone(),
            None => {
                self.limit = (self.reads + MAX_CLASS_READS).min(MAX_READS);
                let found = self.search(project, name, 0, None, imported_from);
                self.shapes.insert(name.to_owned(), found.clone());
                found
            }
        }?;
        let fields = shape
            .fields
            .into_iter()
            .map(|field| Field { name: field.name, ty: field.ty, anns: Vec::new(), doc: String::new(), value: None })
            .collect();
        Some((shape.params, fields))
    }

    fn search(
        &mut self,
        project: &Project,
        name: &str,
        depth: usize,
        must_try: Option<&str>,
        imported_from: Option<&str>,
    ) -> Option<Shape> {
        if depth > MAX_SUPER || self.denied {
            return None;
        }
        let jars = self.jars(project).to_vec();
        let preferred = self.cached(project, name);
        let hint = imported_from.map(str::to_owned).or_else(|| self.home.clone());

        // The previous scan's jar, then the superclass's jar, then the jars
        // whose coordinates look like the class's package.
        let mut scored: Vec<(i32, JarRef)> = jars
            .into_iter()
            .map(|jar| (jar_score(&jar, hint.as_deref(), preferred.as_deref(), must_try), jar))
            .collect();
        scored.sort_by_key(|(score, _)| -*score);

        for (_, jar) in scored {
            if self.reads >= self.limit || self.bytes >= MAX_BYTES {
                return None;
            }
            // Some(None): indexed, no such class. Some(Some(entry)): a hit.
            // None: the jar has not been read yet.
            let indexed: Option<Option<String>> = self
                .index
                .get(&jar.path)
                .map(|known| known.as_ref().and_then(|entries| entries.get(name).cloned()));
            match indexed {
                Some(None) => continue,
                Some(Some(entry)) => {
                    if let Some(shape) = self.load(project, &jar, &entry, name, depth) {
                        return Some(shape);
                    }
                    // The index is stale (the jar changed): read it again.
                    self.index.insert(jar.path.clone(), None);
                    continue;
                }
                None => {}
            }

            let Some(bytes) = self.read_jar(project, &jar) else {
                self.index.insert(jar.path.clone(), None);
                continue;
            };
            self.reads += 1;
            self.bytes += bytes.len();
            let Some(zip) = Zip::open(&bytes).filter(|_| bytes.len() <= MAX_JAR) else {
                self.index.insert(jar.path.clone(), None);
                continue;
            };
            let mut entries: HashMap<String, String> = HashMap::new();
            for entry in zip.names() {
                if let Some(class) = entry.strip_suffix(".class") {
                    let simple = class.rsplit('/').next().unwrap_or(class);
                    entries.entry(simple.to_owned()).or_insert_with(|| entry.to_owned());
                }
            }
            let hit = entries.get(name).cloned();
            self.index.insert(jar.path.clone(), Some(entries));
            // The zip is already open, so the class comes out without a second
            // trip through the central directory.
            if let Some(entry) = hit
                && let Some(shape) = self.decode(project, &jar, &zip, &entry, name, depth)
            {
                return Some(shape);
            }
        }
        None
    }

    /// Decompresses `entry` out of a jar that has already been indexed.
    fn load(
        &mut self,
        project: &Project,
        jar: &JarRef,
        entry: &str,
        name: &str,
        depth: usize,
    ) -> Option<Shape> {
        if self.reads >= self.limit || self.bytes >= MAX_BYTES {
            return None;
        }
        let bytes = self.read_jar(project, jar)?;
        self.reads += 1;
        self.bytes += bytes.len();
        let zip = Zip::open(&bytes)?;
        self.decode(project, jar, &zip, entry, name, depth)
    }

    /// Decompresses `entry` out of a jar the caller has already opened, so a
    /// jar that declares several types is parsed once.
    fn decode(
        &mut self,
        project: &Project,
        jar: &JarRef,
        zip: &Zip<'_>,
        entry: &str,
        name: &str,
        depth: usize,
    ) -> Option<Shape> {
        let class_bytes = zip.read(entry, 8 << 20)?;
        let class = classfile::parse(&class_bytes)?;
        self.remember(project, name, &jar.path);
        Some(self.merge(project, class, depth, &jar.path))
    }

    /// The jar a previous scan found `name` in, per project.
    fn cached(&self, project: &Project, name: &str) -> Option<String> {
        let root = project.root().ok()?;
        host::storage::get(&format!("deps:{root}:{name}")).ok().flatten().filter(|jar| !jar.is_empty())
    }

    fn remember(&self, project: &Project, name: &str, jar: &str) {
        if let Ok(root) = project.root() {
            let _ = host::storage::set(&format!("deps:{root}:{name}"), jar);
        }
    }

    /// Own fields plus the inherited ones, with the parent's parameters
    /// substituted by the arguments of `extends Base<...>`.
    fn merge(&mut self, project: &Project, class: ClassFile, depth: usize, jar: &str) -> Shape {
        let mut shape = Shape { params: class.params.clone(), fields: class.fields.clone() };
        let Some(super_name) = class.super_class.clone().filter(|name| name != "Object") else {
            return shape;
        };
        let Some(mut parent) = self.search(project, &super_name, depth + 1, Some(jar), None) else {
            return shape;
        };
        for field in &mut parent.fields {
            field.ty = crate::model::substitute_type(&field.ty, &parent.params, &class.super_args);
        }
        parent.fields.extend(shape.fields);
        shape.fields = parent.fields;
        shape
    }

    fn read_jar(&mut self, project: &Project, jar: &JarRef) -> Option<Vec<u8>> {
        if !jar.external {
            return project.read_bytes(&jar.path).ok();
        }
        match host::fs::read_bytes(&jar.path) {
            Ok(bytes) => Some(bytes),
            Err(Error::Denied(_)) => {
                self.deny(&jar.path);
                None
            }
            Err(_) => None,
        }
    }

    fn deny(&mut self, path: &str) {
        if self.denied {
            return;
        }
        self.denied = true;
        // The manifest is the only place a grant can come from, so the advice
        // has to name the exact line to add, not "grant permission".
        let repo = &self.cfg.repo;
        // The notification follows the IDE language; the log keeps Chinese, like
        // every other `host::log` line in this plugin (logs are for diagnosing,
        // not part of the UI).
        let advice = crate::i18n::dep_jar_advice(crate::i18n::Lang::Zh, path, repo);
        host::log(LogLevel::Warn, &format!("ApiMocktle: {advice}"));
        if !WARNED.swap(true, Ordering::Relaxed) {
            let lang = crate::i18n::lang();
            host::notify(
                NotificationLevel::Warning,
                crate::i18n::DEP_JAR_TITLE.get(lang),
                &crate::i18n::dep_jar_advice(lang, path, repo),
            );
        }
    }

    fn jars(&mut self, project: &Project) -> &[JarRef] {
        if self.jars.is_none() {
            let mut jars: Vec<JarRef> = project
                .find_files("**/*.jar", 12)
                .unwrap_or_default()
                .into_iter()
                .filter(|path| {
                    let path = path.replace('\\', "/");
                    !path.contains("/target/") && !path.contains("/build/")
                })
                .map(|path| JarRef { path, external: false, group: None, artifact: None })
                .collect();
            if !self.denied {
                jars.extend(self.repo_jars(project));
            }
            let mut seen = HashSet::new();
            jars.retain(|jar| seen.insert(jar.path.clone()));
            self.jars = Some(jars);
        }
        self.jars.as_deref().unwrap_or_default()
    }

    /// The jars of the project's Maven dependencies, read from the local
    /// repository. Returns only the ones that exist; a denied capability stops
    /// the whole lookup and is remembered.
    fn repo_jars(&mut self, project: &Project) -> Vec<JarRef> {
        let poms = project.find_files("**/pom.xml", 24).unwrap_or_default();
        let mut parsed = Vec::new();
        for path in poms {
            if let Ok(text) = project.read_file(&path) {
                parsed.push(parse_pom(&text));
            }
        }
        if parsed.is_empty() {
            return Vec::new();
        }
        if self.home.is_none() {
            self.home = parsed
                .iter()
                .find_map(|pom| pom.group.clone().or_else(|| pom.parent.as_ref().map(|parent| parent.g.clone())))
                .filter(|group| !group.is_empty());
        }
        let global = merged_props(&parsed);
        let mut managed = HashMap::new();
        for pom in &parsed {
            let props = PomProps::new(&global, pom);
            for dep in &pom.managed {
                if let Some(version) = dep.v.as_deref().and_then(|v| props.resolve(v)) {
                    managed.entry((dep.g.clone(), dep.a.clone())).or_insert(version);
                }
            }
        }

        let mut queue: Vec<(PomDep, usize)> = Vec::new();
        for pom in &parsed {
            let props = PomProps::new(&global, pom);
            for dep in &pom.deps {
                if dep.is_test() {
                    continue;
                }
                if let Some(version) = dep_version(dep, &props, &managed) {
                    queue.push((dep.with_version(&version), 0));
                }
            }
        }

        let mut jars: Vec<JarRef> = Vec::new();
        let mut seen_poms: HashSet<(String, String, String)> = HashSet::new();
        let mut poms_read = 0usize;
        while let Some((dep, depth)) = queue.pop() {
            for path in jar_paths(&self.cfg.repo, &dep) {
                jars.push(JarRef {
                    path,
                    external: true,
                    group: Some(dep.g.clone()),
                    artifact: Some(dep.a.clone()),
                });
            }
            if depth >= 3 || poms_read >= MAX_POMS || self.denied {
                continue;
            }
            if !seen_poms.insert((dep.g.clone(), dep.a.clone(), dep.v.clone().unwrap_or_default())) {
                continue;
            }
            let path = pom_path(&self.cfg.repo, &dep);
            let text = match host::fs::read_file(&path) {
                Ok(text) => text,
                Err(Error::Denied(_)) => {
                    self.deny(&path);
                    continue;
                }
                Err(_) => continue,
            };
            poms_read += 1;
            let pom = parse_pom(&text);
            let props = PomProps::new(&global, &pom);
            for child in &pom.deps {
                if child.is_test() {
                    continue;
                }
                if let Some(version) = dep_version(child, &props, &managed) {
                    queue.push((child.with_version(&version), depth + 1));
                }
            }
        }
        jars
    }
}

/// One dependency declaration from a POM.
#[derive(Clone, Debug, Default)]
struct PomDep {
    g: String,
    a: String,
    v: Option<String>,
    classifier: Option<String>,
    scope: Option<String>,
    optional: bool,
}

impl PomDep {
    fn is_test(&self) -> bool {
        self.scope.as_deref() == Some("test") || self.optional
    }

    fn with_version(&self, version: &str) -> PomDep {
        let mut dep = self.clone();
        dep.v = Some(version.to_owned());
        dep
    }
}

#[derive(Clone, Debug, Default)]
struct Pom {
    group: Option<String>,
    artifact: Option<String>,
    version: Option<String>,
    parent: Option<PomDep>,
    props: HashMap<String, String>,
    managed: Vec<PomDep>,
    deps: Vec<PomDep>,
}

/// A tolerant POM reader: comments are stripped and tags are walked in order,
/// so namespaces, attribute order and formatting don't matter. Only direct
/// children count (`<exclusion><groupId>` must not overwrite the dependency's
/// own coordinates), which is what the tag stack is for.
fn parse_pom(text: &str) -> Pom {
    let text = strip_comments(text);
    let mut pom = Pom::default();
    let mut buf = String::new();
    let mut stack: Vec<String> = Vec::new();
    let mut dep: Option<PomDep> = None;
    let mut parent: Option<PomDep> = None;
    let mut rest = text.as_str();

    while let Some(open) = rest.find('<') {
        buf.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>').map(|i| open + i) else { break };
        let raw = &rest[open + 1..close];
        rest = &rest[close + 1..];

        if let Some(name) = raw.strip_prefix('/') {
            let name = name.trim();
            let value = buf.trim().to_owned();
            let container = if stack.len() >= 2 { stack[stack.len() - 2].as_str() } else { "" };
            match name {
                "parent" => {
                    if let Some(parent) = parent.take() {
                        pom.parent = Some(parent);
                    }
                }
                "dependency" => {
                    if let Some(dep) = dep.take() {
                        if stack.iter().any(|open| open == "dependencyManagement") {
                            pom.managed.push(dep);
                        } else {
                            pom.deps.push(dep);
                        }
                    }
                }
                _ if container == "properties" => {
                    if !name.is_empty() {
                        pom.props.insert(name.to_owned(), value);
                    }
                }
                _ if container == "dependency" && dep.is_some() => {
                    let dep = dep.as_mut().expect("checked");
                    match name {
                        "groupId" => dep.g = value,
                        "artifactId" => dep.a = value,
                        "version" => dep.v = Some(value),
                        "scope" => dep.scope = Some(value),
                        "classifier" => dep.classifier = Some(value).filter(|s| !s.is_empty()),
                        "optional" => dep.optional = value == "true",
                        _ => {}
                    }
                }
                _ if container == "parent" && parent.is_some() => {
                    let parent = parent.as_mut().expect("checked");
                    match name {
                        "groupId" => parent.g = value,
                        "artifactId" => parent.a = value,
                        "version" => parent.v = Some(value),
                        _ => {}
                    }
                }
                _ if container == "project" => match name {
                    "groupId" => pom.group = Some(value),
                    "artifactId" => pom.artifact = Some(value),
                    "version" => pom.version = Some(value),
                    _ => {}
                },
                _ => {}
            }
            if stack.pop().is_none() {
                break;
            }
            buf.clear();
            continue;
        }

        let self_closing = raw.ends_with('/');
        let name = raw
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        if !self_closing {
            if name == "parent" {
                parent = Some(PomDep::default());
            }
            // A plugin's own `<dependencies>` are not project dependencies.
            if name == "dependency" && !stack.iter().any(|open| open == "plugin") {
                dep = Some(PomDep::default());
            }
            stack.push(name);
        }
        buf.clear();
    }
    pom
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        match rest[open..].find("-->") {
            Some(close) => rest = &rest[open + close + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Property maps of every project POM, first definition wins.
fn merged_props(poms: &[Pom]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pom in poms {
        for (key, value) in &pom.props {
            out.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    out
}

/// Property lookup over the project's POMs: the reactor-wide map, overlaid by
/// one POM's own `<properties>`. Borrows instead of cloning both maps for every
/// POM, and resolves `project.*` / `parent.*` against the POM in hand.
struct PomProps<'a> {
    global: &'a HashMap<String, String>,
    own: &'a HashMap<String, String>,
    pom: &'a Pom,
}

impl<'a> PomProps<'a> {
    fn new(global: &'a HashMap<String, String>, pom: &'a Pom) -> Self {
        PomProps { global, own: &pom.props, pom }
    }

    fn get(&self, key: &str) -> Option<String> {
        if let Some(value) = self.own.get(key).or_else(|| self.global.get(key)) {
            return Some(value.clone());
        }
        let parent = self.pom.parent.as_ref();
        match key {
            "project.groupId" | "pom.groupId" => {
                self.pom.group.clone().or_else(|| parent.map(|p| p.g.clone()))
            }
            "project.artifactId" | "pom.artifactId" => self.pom.artifact.clone(),
            "project.version" | "pom.version" | "version" => {
                self.pom.version.clone().or_else(|| parent.and_then(|p| p.v.clone()))
            }
            "parent.version" | "project.parent.version" => parent.and_then(|p| p.v.clone()),
            _ => None,
        }
    }

    /// `value` with every `${...}` expanded; `None` when one cannot be resolved.
    fn resolve(&self, value: &str) -> Option<String> {
        expand(value, &|key| self.get(key))
    }
}

fn expand(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'$'
            && bytes.get(at + 1) == Some(&b'{')
            && let Some(close) = value[at + 2..].find('}')
        {
            let key = &value[at + 2..at + 2 + close];
            out.push_str(&lookup(key)?);
            at += close + 3;
            continue;
        }
        let ch = value[at..].chars().next()?;
        out.push(ch);
        at += ch.len_utf8();
    }
    (!out.contains("${")).then_some(out)
}

/// The version a dependency resolves to: its own, else the managed one.
fn dep_version(
    dep: &PomDep,
    props: &PomProps<'_>,
    managed: &HashMap<(String, String), String>,
) -> Option<String> {
    if let Some(version) = dep.v.as_deref().and_then(|v| props.resolve(v)) {
        return Some(version);
    }
    managed.get(&(dep.g.clone(), dep.a.clone())).cloned()
}

/// The jar paths a resolved dependency may live at.
fn jar_paths(repo: &str, dep: &PomDep) -> Vec<String> {
    let Some(version) = dep.v.as_deref() else { return Vec::new() };
    let dir = format!("{repo}/{}/{}/{version}", dep.g.replace('.', "/"), dep.a);
    let file = match &dep.classifier {
        Some(classifier) => format!("{}-{version}-{classifier}", dep.a),
        None => format!("{}-{version}", dep.a),
    };
    if version.ends_with("-SNAPSHOT") {
        snapshots(&dir, &dep.a, version)
    } else {
        vec![format!("{dir}/{file}.jar")]
    }
}

/// A snapshot version's directory holds timestamped jars; the plain file is
/// the local build's own copy.
fn snapshots(dir: &str, artifact: &str, version: &str) -> Vec<String> {
    let plain = format!("{dir}/{artifact}-{version}.jar");
    let mut out = vec![plain];
    if let Ok(entries) = host::fs::read_dir(dir) {
        let mut jars: Vec<String> = entries
            .into_iter()
            .filter(|entry| entry.name.ends_with(".jar"))
            .filter(|entry| !entry.name.contains("-sources") && !entry.name.contains("-javadoc"))
            .map(|entry| format!("{dir}/{}", entry.name))
            .collect();
        jars.sort();
        jars.reverse();
        out.extend(jars);
    }
    out
}

/// Ranks jars for a lookup: the remembered one first, the superclass's jar
/// next, the project's own jars (small) above repository jars, and jars whose
/// Maven group/artifact look like the class's import package above the rest.
fn jar_score(jar: &JarRef, hint: Option<&str>, preferred: Option<&str>, must_try: Option<&str>) -> i32 {
    let mut score = 0;
    if preferred == Some(jar.path.as_str()) {
        score += 1000;
    }
    if must_try == Some(jar.path.as_str()) {
        score += 900;
    }
    if !jar.external {
        score += 300;
    }
    let (Some(hint), Some(group)) = (hint, jar.group.as_deref()) else { return score };
    score += common_segments(hint, group) as i32 * 20;
    if let Some(artifact) = jar.artifact.as_deref() {
        score += overlap(artifact, hint) as i32 * 15;
    }
    score
}

/// How many leading dot-separated segments two identifiers share.
fn common_segments(left: &str, right: &str) -> usize {
    left.split('.').zip(right.split('.')).take_while(|(a, b)| a == b).count()
}

/// How many words of `hint` also appear in `artifact` (`example-services-common`
/// vs `com.example.services.common.http` → 2).
fn overlap(artifact: &str, hint: &str) -> usize {
    let words: HashSet<&str> = artifact.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect();
    hint.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty() && words.contains(word))
        .count()
}

fn pom_path(repo: &str, dep: &PomDep) -> String {
    let version = dep.v.as_deref().unwrap_or_default();
    format!("{repo}/{}/{}/{version}/{}-{version}.pom", dep.g.replace('.', "/"), dep.a, dep.a)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = r#"
<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
    <parent>
        <groupId>com.example</groupId>
        <artifactId>parent</artifactId>
        <version>1.0</version>
    </parent>
    <groupId>com.example</groupId>
    <artifactId>demo</artifactId>
    <version>2.0</version>
    <properties>
        <common.version>3.1.0</common.version>
    </properties>
    <dependencyManagement>
        <dependencies>
            <dependency>
                <groupId>com.example</groupId>
                <artifactId>managed</artifactId>
                <version>9.0</version>
            </dependency>
        </dependencies>
    </dependencyManagement>
    <dependencies>
        <dependency>
            <groupId>com.example</groupId>
            <artifactId>common</artifactId>
            <version>${common.version}</version>
        </dependency>
        <dependency>
            <groupId>com.example</groupId>
            <artifactId>managed</artifactId>
        </dependency>
        <dependency>
            <groupId>com.example</groupId>
            <artifactId>tests</artifactId>
            <version>1.0</version>
            <scope>test</scope>
        </dependency>
    </dependencies>
</project>
"#;

    #[test]
    fn reads_dependencies_properties_and_management() {
        let pom = parse_pom(ROOT);
        assert_eq!(pom.group.as_deref(), Some("com.example"));
        assert_eq!(pom.artifact.as_deref(), Some("demo"));
        assert_eq!(pom.version.as_deref(), Some("2.0"));
        assert_eq!(pom.parent.as_ref().and_then(|p| p.v.as_deref()), Some("1.0"));
        assert_eq!(pom.deps.len(), 3, "{:?}", pom.deps);
        assert_eq!(pom.deps[0].a, "common");
        assert_eq!(pom.deps[0].v.as_deref(), Some("${common.version}"));
        assert_eq!(pom.managed.len(), 1);
        assert_eq!(pom.props.get("common.version").map(String::as_str), Some("3.1.0"));

        let global: HashMap<String, String> = HashMap::new();
        let props = PomProps::new(&global, &pom);
        let managed = HashMap::from([(("com.example".to_owned(), "managed".to_owned()), "9.0".to_owned())]);
        assert_eq!(dep_version(&pom.deps[0], &props, &managed).as_deref(), Some("3.1.0"));
        assert_eq!(dep_version(&pom.deps[1], &props, &managed).as_deref(), Some("9.0"));
        assert!(pom.deps[2].is_test());
    }

    #[test]
    fn jar_paths_follow_the_repository_layout() {
        let dep = PomDep {
            g: "com.example".into(),
            a: "common".into(),
            v: Some("3.1.0".into()),
            ..PomDep::default()
        };
        assert_eq!(
            jar_paths("~/.m2/repository", &dep),
            vec!["~/.m2/repository/com/example/common/3.1.0/common-3.1.0.jar".to_owned()]
        );
        assert_eq!(
            pom_path("~/.m2/repository", &dep),
            "~/.m2/repository/com/example/common/3.1.0/common-3.1.0.pom"
        );
    }

    #[test]
    fn the_import_package_ranks_the_matching_jar_first() {
        let jar = |path: &str, group: &str, artifact: &str| JarRef {
            path: path.to_owned(),
            external: true,
            group: Some(group.to_owned()),
            artifact: Some(artifact.to_owned()),
        };
        let hint_pkg = "com.example.services.common.http";
        let hint = Some(hint_pkg);
        let common = jar("common.jar", "com.example.services", "example-services-common");
        let spring = jar("spring.jar", "org.springframework", "spring-core");
        let jackson = jar("jackson.jar", "com.fasterxml.jackson.core", "jackson-databind");
        let common_score = jar_score(&common, hint, None, None);
        assert!(common_score > jar_score(&spring, hint, None, None), "{common_score}");
        assert!(common_score > jar_score(&jackson, hint, None, None), "{common_score}");
        assert_eq!(common_segments(hint_pkg, "com.example.services"), 3);
        assert_eq!(overlap("example-services-common", hint_pkg), 3);
        assert_eq!(overlap("example-services-online-base", hint_pkg), 2);
        // A remembered jar outranks everything else.
        assert!(
            jar_score(&jackson, hint, Some("jackson.jar"), None) > common_score,
            "the cached jar is tried first"
        );
    }
}
