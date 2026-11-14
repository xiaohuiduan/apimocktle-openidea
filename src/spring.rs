//! Parses Spring MVC, JAX-RS and Feign declarations out of a token stream.
//!
//! OpenIDEA plugins run in a WASM sandbox and cannot use a Java parser, so the
//! controller model is recovered from tokens: class-level and method-level
//! mapping annotations, parameter annotations, and a best-effort return type
//! and DTO field list.

use crate::java::{Kind, Lexed, ident, is, lex, matching};

/// One annotation: its simple name and its arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ann {
    pub name: String,
    pub args: Vec<Arg>,
    /// Token index of the `@`.
    pub at: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arg {
    pub key: String,
    pub strings: Vec<String>,
    pub idents: Vec<String>,
}

impl Ann {
    pub fn strings(&self, key: &str) -> Vec<&str> {
        self.args
            .iter()
            .filter(|a| a.key == key)
            .flat_map(|a| a.strings.iter().map(String::as_str))
            .collect()
    }

    pub fn idents(&self, key: &str) -> Vec<&str> {
        self.args
            .iter()
            .filter(|a| a.key == key)
            .flat_map(|a| a.idents.iter().map(String::as_str))
            .collect()
    }

    pub fn flag(&self, key: &str) -> Option<bool> {
        self.args
            .iter()
            .filter(|a| a.key == key)
            .flat_map(|a| a.idents.iter())
            .next()
            .map(|v| v == "true")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    Path,
    Query,
    Header,
    Body,
    Cookie,
    Form,
}

#[derive(Clone, Debug)]
pub struct RawParam {
    pub name: String,
    pub ty: String,
    pub kind: ParamKind,
    pub required: bool,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct RawEndpoint {
    pub framework: &'static str,
    /// `None` accepts any method (`@RequestMapping` without `method`).
    pub http_method: Option<String>,
    pub path: String,
    pub class_name: String,
    pub method_name: String,
    pub package: String,
    /// Token index of the mapping annotation, for the source location.
    pub at: usize,
    pub summary: String,
    pub description: String,
    pub params: Vec<RawParam>,
    pub body_type: Option<String>,
    pub return_type: String,
}

#[derive(Clone, Debug)]
pub struct Method {
    pub name: String,
    pub anns: Vec<Ann>,
    pub ret: String,
    pub params: Vec<Param>,
    pub doc: String,
    pub at: usize,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub anns: Vec<Ann>,
    pub doc: String,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub ty: String,
    pub anns: Vec<Ann>,
    pub doc: String,
}

#[derive(Clone, Debug, Default)]
pub struct Class {
    pub name: String,
    /// Declared type parameters: `class Result<T>` yields `["T"]`. The schema
    /// generator needs them to substitute the arguments of `Result<X>`.
    pub type_params: Vec<String>,
    pub anns: Vec<Ann>,
    pub methods: Vec<Method>,
    pub fields: Vec<Field>,
    pub doc: String,
}

#[derive(Clone, Debug, Default)]
pub struct FileModel {
    pub package: String,
    pub imports: Vec<String>,
    pub classes: Vec<Class>,
}

const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "final",
    "transient",
    "volatile",
    "abstract",
    "native",
    "synchronized",
    "strictfp",
    "default",
    "sealed",
    "non-sealed",
    "val",
    "var",
];

fn is_modifier(word: &str) -> bool {
    MODIFIERS.contains(&word)
}

/// Parses one source file.
pub fn parse(src: &str) -> FileModel {
    let lexed = lex(src);
    let n = lexed.toks.len();
    let mut model = FileModel::default();
    let mut pending: Vec<Ann> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if is(src, &lexed, i, '@') {
            if let Some((ann, next)) = read_ann(src, &lexed, i) {
                pending.push(ann);
                i = next;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(word) = ident(src, &lexed, i) {
            match word {
                "package" => {
                    let (name, next) = read_dotted(src, &lexed, i + 1);
                    model.package = name;
                    i = next;
                    continue;
                }
                "import" => {
                    let (name, next) = read_dotted(src, &lexed, i + 1);
                    if !name.is_empty() {
                        model.imports.push(name);
                    }
                    i = next;
                    continue;
                }
                "class" | "interface" | "enum" | "record" => {
                    let mut j = i + 1;
                    while j < n && ident(src, &lexed, j).is_none() {
                        j += 1;
                    }
                    let Some(name) = ident(src, &lexed, j).map(str::to_owned) else {
                        i += 1;
                        continue;
                    };
                    let type_params = type_params_of(src, &lexed, j + 1);
                    let doc = doc_before(src, &lexed, lexed.toks[i].start);
                    if word == "enum" {
                        let mut k = j + 1;
                        while k < n && !is(src, &lexed, k, '{') {
                            k += 1;
                        }
                        if is(src, &lexed, k, '{') {
                            i = matching(src, &lexed, k, '{', '}').unwrap_or(n - 1) + 1;
                        } else {
                            i = k + 1;
                        }
                        pending.clear();
                        continue;
                    }
                    // A record or a Kotlin class may carry its fields in a
                    // primary constructor before the body.
                    let mut k = j + 1;
                    let mut ctor_fields: Vec<Field> = Vec::new();
                    while k < n && !is(src, &lexed, k, '{') && !is(src, &lexed, k, ';') && !is(src, &lexed, k, '(') {
                        k += 1;
                    }
                    if is(src, &lexed, k, '(') {
                        let close = matching(src, &lexed, k, '(', ')').unwrap_or(n - 1);
                        ctor_fields = parse_params(src, &lexed, k + 1, close)
                            .into_iter()
                            .map(|p| Field { name: p.name, ty: p.ty, anns: p.anns, doc: p.doc })
                            .collect();
                        k = close + 1;
                        while k < n
                            && !is(src, &lexed, k, '{')
                            && !is(src, &lexed, k, ';')
                            && !is(src, &lexed, k, '}')
                        {
                            if is(src, &lexed, k, '@')
                                || ident(src, &lexed, k).is_some_and(|w| {
                                    matches!(w, "class" | "interface" | "enum" | "record")
                                })
                            {
                                break;
                            }
                            k += 1;
                        }
                        if !is(src, &lexed, k, '{') && !is(src, &lexed, k, ';') && !is(src, &lexed, k, '}') {
                            // The class ended without a body: keep its fields.
                            if word == "class" || word == "record" {
                                model.classes.push(Class {
                                    name,
                                    type_params: type_params.clone(),
                                    anns: std::mem::take(&mut pending),
                                    doc,
                                    fields: ctor_fields,
                                    ..Class::default()
                                });
                            } else {
                                pending.clear();
                            }
                            i = k;
                            continue;
                        }
                    }
                    if is(src, &lexed, k, '{') {
                        let end = matching(src, &lexed, k, '{', '}').unwrap_or(n - 1);
                        if word == "class" || word == "interface" || word == "record" {
                            let mut class = Class {
                                name,
                                type_params: type_params.clone(),
                                anns: std::mem::take(&mut pending),
                                doc,
                                fields: ctor_fields,
                                ..Class::default()
                            };
                            parse_body(src, &lexed, k + 1, end, &mut class);
                            model.classes.push(class);
                        } else {
                            pending.clear();
                        }
                        i = end + 1;
                        continue;
                    }
                    pending.clear();
                    i = k + 1;
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    model
}

fn read_dotted(src: &str, lexed: &Lexed, from: usize) -> (String, usize) {
    let n = lexed.toks.len();
    let mut name = String::new();
    let mut i = from;
    while i < n {
        if is(src, lexed, i, ';') {
            i += 1;
            break;
        }
        if let Some(word) = ident(src, lexed, i) {
            name.push_str(word);
        } else if is(src, lexed, i, '.') {
            name.push('.');
        } else if is(src, lexed, i, '*') {
            name.push('*');
        } else {
            break;
        }
        i += 1;
    }
    (name, i)
}

/// The type parameters declared between a class name and its body:
/// `class Result<T, E>` yields `["T", "E"]`. Only the name of each parameter is
/// kept, which drops a Kotlin `: Any` or a Java `extends Comparable<T>` bound as
/// well as a Kotlin `in`/`out` variance modifier or a leading type annotation.
/// `from` must be the token after the name.
fn type_params_of(src: &str, lexed: &Lexed, from: usize) -> Vec<String> {
    if !is(src, lexed, from, '<') {
        return Vec::new();
    }
    let Some(close) = matching(src, lexed, from, '<', '>') else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut start = from + 1;
    let mut i = from + 1;
    while i <= close {
        // Nested angle brackets belong to a bound (`Comparable<T>`), so only a
        // comma at depth 0 starts the next parameter.
        if i == close || (is(src, lexed, i, ',') && depth == 0) {
            if let Some(name) = param_name(src, lexed, start, i) {
                out.push(name.to_owned());
            }
            start = i + 1;
        } else if is(src, lexed, i, '<') {
            depth += 1;
        } else if is(src, lexed, i, '>') {
            depth -= 1;
        }
        i += 1;
    }
    out
}

/// The name one parameter segment declares. A bound keyword ends it, so the
/// identifier before the bound wins over a Kotlin `in`/`out` variance modifier
/// or a leading annotation (`class ApiResult<out T : Any>` names `T`); without a
/// bound the last identifier is the name.
fn param_name<'a>(src: &'a str, lexed: &Lexed, from: usize, to: usize) -> Option<&'a str> {
    let mut last: Option<&'a str> = None;
    let mut i = from;
    while i < to {
        if is(src, lexed, i, ':') || ident(src, lexed, i) == Some("extends") {
            return last;
        }
        if let Some(word) = ident(src, lexed, i) {
            last = Some(word);
        }
        i += 1;
    }
    last
}

fn parse_body(src: &str, lexed: &Lexed, from: usize, to: usize, class: &mut Class) {
    let mut anns: Vec<Ann> = Vec::new();
    let mut seg: Vec<usize> = Vec::new();
    let mut i = from;
    while i < to {
        if is(src, lexed, i, '@')
            && let Some((ann, next)) = read_ann(src, lexed, i)
        {
            anns.push(ann);
            i = next;
            continue;
        }
        if is(src, lexed, i, '(') {
            let has_assign = seg.iter().any(|&t| is(src, lexed, t, '='));
            let name_tok = seg.iter().rev().copied().find(|&t| ident(src, lexed, t).is_some());
            let is_fun = seg.iter().any(|&t| ident(src, lexed, t) == Some("fun"));
            if !has_assign
                && let (Some(name_tok), Some(name)) = (name_tok, name_tok.and_then(|t| ident(src, lexed, t)))
            {
                let close = matching(src, lexed, i, '(', ')').unwrap_or(to);
                let params = parse_params(src, lexed, i + 1, close);
                // Kotlin writes the return type after the parameter list.
                let (ret, next) = if is_fun {
                    kotlin_return(src, lexed, close + 1, to)
                } else {
                    (type_before(src, lexed, &seg, name_tok), close + 1)
                };
                let doc = doc_before(src, lexed, lexed.toks[anns.first().map_or(name_tok, |a| a.at)].start);
                class.methods.push(Method {
                    name: name.to_owned(),
                    anns: std::mem::take(&mut anns),
                    ret,
                    params,
                    doc,
                    at: name_tok,
                });
                i = next;
                seg.clear();
                continue;
            }
            // A field initializer or a call: skip the parentheses so their
            // commas and equals signs cannot split the statement.
            let close = matching(src, lexed, i, '(', ')').unwrap_or(i);
            i = close + 1;
            continue;
        }
        if is(src, lexed, i, '{') {
            let close = matching(src, lexed, i, '{', '}').unwrap_or(to);
            i = close + 1;
            anns.clear();
            seg.clear();
            continue;
        }
        if is(src, lexed, i, '}') {
            break;
        }
        if is(src, lexed, i, ';') {
            if let Some(field) = parse_field(src, lexed, &seg, &anns) {
                class.fields.push(field);
            }
            anns.clear();
            seg.clear();
            i += 1;
            continue;
        }
        // Kotlin declares properties without a trailing `;`, so a `val`/`var`
        // ends the statement before it.
        if !seg.is_empty()
            && let Some(word) = ident(src, lexed, i)
            && (word == "val" || word == "var")
        {
            if let Some(field) = parse_field(src, lexed, &seg, &anns) {
                class.fields.push(field);
            }
            anns.clear();
            seg.clear();
        }
        seg.push(i);
        i += 1;
    }
    if let Some(field) = parse_field(src, lexed, &seg, &anns) {
        class.fields.push(field);
    }
}

fn parse_field(src: &str, lexed: &Lexed, seg: &[usize], anns: &[Ann]) -> Option<Field> {
    let cut = seg.iter().position(|&t| is(src, lexed, t, '=')).unwrap_or(seg.len());
    let head = &seg[..cut];
    if head.is_empty() {
        return None;
    }
    let doc_at = |token: usize| doc_before(src, lexed, lexed.toks[anns.first().map_or(token, |a| a.at)].start);
    // Kotlin writes `val name: Type`.
    if let Some(colon) = head.iter().position(|&t| is(src, lexed, t, ':')) {
        let name_tok = head[..colon]
            .iter()
            .rev()
            .copied()
            .find(|&t| ident(src, lexed, t).is_some_and(|w| w != "val" && w != "var"))?;
        let name = ident(src, lexed, name_tok)?.to_owned();
        if is_modifier(&name) {
            return None;
        }
        let first = *head.get(colon + 1)?;
        let last = *head.last()?;
        if lexed.toks[last].end <= lexed.toks[first].start {
            return None;
        }
        let ty = src[lexed.toks[first].start..lexed.toks[last].end].trim().to_owned();
        if ty.is_empty() {
            return None;
        }
        return Some(Field { name, ty, anns: anns.to_vec(), doc: doc_at(name_tok) });
    }
    let name_tok = head.iter().rev().copied().find(|&t| ident(src, lexed, t).is_some())?;
    let name = ident(src, lexed, name_tok)?.to_owned();
    if is_modifier(&name) {
        return None;
    }
    let ty = type_before(src, lexed, head, name_tok);
    if ty.is_empty() {
        return None;
    }
    Some(Field { name, ty, anns: anns.to_vec(), doc: doc_at(name_tok) })
}

/// A Kotlin `fun` return type, written after the parameter list as `: Type`.
fn kotlin_return(src: &str, lexed: &Lexed, from: usize, to: usize) -> (String, usize) {
    if from < to
        && is(src, lexed, from, ':')
        && let Some(first) = lexed.toks.get(from + 1)
    {
        let mut end = from + 1;
        while end < to
            && !is(src, lexed, end, '{')
            && !is(src, lexed, end, '=')
            && !is(src, lexed, end, '}')
            && !is(src, lexed, end, ';')
        {
            end += 1;
        }
        if end > from + 1
            && let Some(last) = lexed.toks.get(end - 1)
        {
            let ty = src[first.start..last.end].trim().to_owned();
            return (ty, end);
        }
    }
    (String::new(), from)
}

/// The declared type written before `name_tok`, with modifiers dropped.
fn type_before(src: &str, lexed: &Lexed, seg: &[usize], name_tok: usize) -> String {
    let start = seg
        .iter()
        .copied()
        .find(|&t| ident(src, lexed, t).is_none_or(|w| !is_modifier(w)));
    let Some(start) = start else { return String::new() };
    if lexed.toks[start].start >= lexed.toks[name_tok].start {
        return String::new();
    }
    src[lexed.toks[start].start..lexed.toks[name_tok].start].trim().to_owned()
}

fn parse_params(src: &str, lexed: &Lexed, from: usize, to: usize) -> Vec<Param> {
    let mut out = Vec::new();
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut start = from;
    let mut angle = 0i32;
    let mut paren = 0i32;
    let mut i = from;
    while i < to {
        if is(src, lexed, i, '<') {
            angle += 1;
        } else if is(src, lexed, i, '>') {
            angle = (angle - 1).max(0);
        } else if is(src, lexed, i, '(') || is(src, lexed, i, '{') {
            paren += 1;
        } else if is(src, lexed, i, ')') || is(src, lexed, i, '}') {
            paren = (paren - 1).max(0);
        } else if is(src, lexed, i, ',') && angle == 0 && paren == 0 {
            parts.push((start, i));
            start = i + 1;
        }
        i += 1;
    }
    if start < to {
        parts.push((start, to));
    }
    for (a, b) in parts {
        if let Some(p) = parse_param(src, lexed, a, b) {
            out.push(p);
        }
    }
    out
}

fn parse_param(src: &str, lexed: &Lexed, from: usize, to: usize) -> Option<Param> {
    let mut anns: Vec<Ann> = Vec::new();
    let mut rest: Vec<usize> = Vec::new();
    let mut i = from;
    while i < to {
        if is(src, lexed, i, '@')
            && let Some((ann, next)) = read_ann(src, lexed, i)
        {
            anns.push(ann);
            i = next;
            continue;
        }
        rest.push(i);
        i += 1;
    }
    if let Some((colon, first, last)) = kotlin_param(src, lexed, &rest) {
        let name_tok = rest[..colon]
            .iter()
            .rev()
            .copied()
            .find(|&t| ident(src, lexed, t).is_some_and(|w| !is_modifier(w)))?;
        let name = ident(src, lexed, name_tok)?.to_owned();
        if lexed.toks[last].end <= lexed.toks[first].start {
            return None;
        }
        let ty = src[lexed.toks[first].start..lexed.toks[last].end].trim().to_owned();
        let doc = doc_before(src, lexed, lexed.toks[anns.first().map_or(name_tok, |a| a.at)].start);
        return Some(Param { name, ty, anns, doc });
    }
    let name_tok = rest.iter().rev().copied().find(|&t| ident(src, lexed, t).is_some())?;
    let name = ident(src, lexed, name_tok)?.to_owned();
    let ty = type_before(src, lexed, &rest, name_tok);
    let doc = doc_before(src, lexed, lexed.toks[anns.first().map_or(name_tok, |a| a.at)].start);
    Some(Param { name, ty, anns, doc })
}

/// A Kotlin parameter is written `name: Type`: its colon and the type's first
/// and last token.
fn kotlin_param(src: &str, lexed: &Lexed, rest: &[usize]) -> Option<(usize, usize, usize)> {
    let colon = rest.iter().position(|&t| is(src, lexed, t, ':'))?;
    let first = *rest.get(colon + 1)?;
    let last = *rest.last()?;
    Some((colon, first, last))
}

fn read_ann(src: &str, lexed: &Lexed, at: usize) -> Option<(Ann, usize)> {
    let mut i = at + 1;
    let mut name = ident(src, lexed, i)?;
    let mut end = i + 1;
    while is(src, lexed, end, '.') {
        match ident(src, lexed, end + 1) {
            Some(seg) => {
                name = seg;
                end += 2;
            }
            None => break,
        }
    }
    let mut args = Vec::new();
    if is(src, lexed, end, '(') {
        let close = matching(src, lexed, end, '(', ')')?;
        args = parse_ann_args(src, lexed, end + 1, close);
        end = close + 1;
    }
    i = end;
    let _ = i;
    Some((Ann { name: name.to_owned(), args, at }, end))
}

fn parse_ann_args(src: &str, lexed: &Lexed, from: usize, to: usize) -> Vec<Arg> {
    let mut parts: Vec<Vec<usize>> = vec![Vec::new()];
    let mut depth = 0i32;
    for t in from..to {
        if is(src, lexed, t, '{') || is(src, lexed, t, '(') {
            depth += 1;
        } else if is(src, lexed, t, '}') || is(src, lexed, t, ')') {
            depth = (depth - 1).max(0);
        } else if is(src, lexed, t, ',') && depth == 0 {
            parts.push(Vec::new());
            continue;
        }
        if let Some(last) = parts.last_mut() {
            last.push(t);
        }
    }
    let mut out = Vec::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        let mut key = "value".to_owned();
        let mut idx = 0usize;
        if let Some(word) = ident(src, lexed, part[0])
            && part.len() > 1
            && is(src, lexed, part[1], '=')
        {
            key = word.to_owned();
            idx = 2;
        }
        let mut strings = Vec::new();
        let mut idents = Vec::new();
        for &t in &part[idx..] {
            match lexed.toks[t].kind {
                Kind::Str => strings.push(unescape(lexed.toks[t].text(src))),
                Kind::Ident => idents.push(lexed.toks[t].text(src).to_owned()),
                _ => {}
            }
        }
        out.push(Arg { key, strings, idents });
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// The Javadoc written just above `byte`, if any.
fn doc_before(src: &str, lexed: &Lexed, byte: usize) -> String {
    let found = lexed
        .docs
        .iter()
        .rev()
        .find(|(_, end)| *end <= byte)
        .copied();
    match found {
        Some((start, end)) => doc_text(&src[start..end]),
        None => String::new(),
    }
}

fn doc_text(raw: &str) -> String {
    let inner = raw.trim_start_matches("/**").trim_end_matches("*/");
    let mut lines: Vec<String> = Vec::new();
    for line in inner.lines() {
        let cleaned = line.trim().trim_start_matches('*').trim();
        lines.push(cleaned.to_owned());
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('@'))
        .unwrap_or("")
        .to_owned()
}

/// The one-line summary of a Javadoc block, for field descriptions.
pub fn summary_of(text: &str) -> String {
    first_line(text)
}

// ---------------------------------------------------------------- mapping ---

fn class_prefix(anns: &[Ann]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for ann in anns {
        // `@FeignClient(value = "index12306-pay-service")` names a service for
        // service discovery, not a path; only its `path` attribute prefixes
        // the route.
        if ann.name == "FeignClient" {
            let declared = ann.strings("path");
            if declared.is_empty() {
                paths.push(String::new());
            } else {
                paths.extend(declared.into_iter().map(str::to_owned));
            }
            continue;
        }
        if !matches!(ann.name.as_str(), "RequestMapping" | "Path") {
            continue;
        }
        let value: Vec<&str> = ann.strings("value");
        let path: Vec<&str> = ann.strings("path");
        let chosen: Vec<&str> = if !value.is_empty() {
            value
        } else if !path.is_empty() {
            path
        } else {
            Vec::new()
        };
        if chosen.is_empty() {
            paths.push(String::new());
        } else {
            paths.extend(chosen.into_iter().map(str::to_owned));
        }
    }
    if paths.is_empty() {
        paths.push(String::new());
    }
    paths
}

/// The HTTP verb an annotation declares, if it declares one (`RequestMapping`
/// without `method` yields `Some(None)`).
fn verb_of(ann: &Ann) -> Option<Option<String>> {
    const VERBS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
    if let Some(suffix) = ann.name.strip_suffix("Mapping") {
        let verb = suffix.to_uppercase();
        if VERBS.contains(&verb.as_str()) {
            return Some(Some(verb));
        }
    }
    if VERBS.contains(&ann.name.as_str()) {
        return Some(Some(ann.name.clone()));
    }
    if ann.name == "RequestMapping" {
        let method = ann.idents("method").last().map(|m| m.to_uppercase()).filter(|m| !m.is_empty());
        return Some(method);
    }
    None
}

/// The path an annotation contributes, if it contributes one.
fn path_values(ann: &Ann) -> Option<Vec<String>> {
    const VERBS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
    let mapping = ann.name == "RequestMapping"
        || ann.name == "Path"
        || ann.name.ends_with("Mapping")
        || VERBS.contains(&ann.name.as_str());
    if mapping { Some(mapping_paths(ann)) } else { None }
}

fn mapping_paths(ann: &Ann) -> Vec<String> {
    let value = ann.strings("value");
    let path = ann.strings("path");
    let chosen = if !value.is_empty() {
        value
    } else if !path.is_empty() {
        path
    } else {
        Vec::new()
    };
    if chosen.is_empty() {
        vec![String::new()]
    } else {
        chosen.into_iter().map(str::to_owned).collect()
    }
}

fn join_path(prefix: &str, sub: &str) -> String {
    let mut out = String::new();
    let a = prefix.trim().trim_end_matches('/');
    let b = sub.trim().trim_start_matches('/');
    if a.is_empty() && b.is_empty() {
        return "/".to_owned();
    }
    if !a.is_empty() {
        out.push_str(a);
    }
    if !b.is_empty() {
        if !out.starts_with('/') {
            out.insert(0, '/');
        }
        out.push('/');
        out.push_str(b);
    }
    if out.is_empty() {
        out.push('/');
    }
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    // Collapse repeated slashes outside of `://`-style content (there is none).
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    out
}

fn framework_of(class: &Class) -> Option<&'static str> {
    let has = |names: &[&str]| class.anns.iter().any(|a| names.contains(&a.name.as_str()));
    if has(&["FeignClient"]) {
        return Some("Feign");
    }
    if has(&["RestController", "Controller"]) {
        return Some("Spring");
    }
    if class.methods.iter().any(|m| m.anns.iter().any(|a| verb_of(a).is_some())) {
        if has(&["Path"]) {
            return Some("JAX-RS");
        }
        return Some("Spring");
    }
    None
}

fn param_kind(param: &Param) -> (ParamKind, Option<String>, bool) {
    for ann in &param.anns {
        let name = ann.strings("value")
            .into_iter()
            .chain(ann.strings("name"))
            .next()
            .map(str::to_owned);
        let required = ann.flag("required").unwrap_or(true);
        let kind = match ann.name.as_str() {
            "PathVariable" | "PathParam" => ParamKind::Path,
            "RequestParam" | "QueryParam" => ParamKind::Query,
            "RequestHeader" | "HeaderParam" => ParamKind::Header,
            "RequestBody" => ParamKind::Body,
            "CookieValue" | "CookieParam" => ParamKind::Cookie,
            "RequestPart" | "FormParam" | "ModelAttribute" => ParamKind::Form,
            _ => continue,
        };
        return (kind, name, required);
    }
    // A Spring controller parameter without an annotation is bound from the
    // query string; a JAX-RS one is a body parameter, which the caller cannot
    // tell apart here, so it stays a query parameter.
    (ParamKind::Query, None, true)
}

/// The endpoints one parsed file declares.
pub fn endpoints(model: &FileModel, src: &str) -> Vec<RawEndpoint> {
    let lexed = lex(src);
    let mut out = Vec::new();
    for class in &model.classes {
        let Some(framework) = framework_of(class) else { continue };
        let prefixes = class_prefix(&class.anns);
        let class_summary = first_line(&class.doc);
        for method in &class.methods {
            let Some(http_method) = method.anns.iter().find_map(verb_of) else {
                continue;
            };
            // JAX-RS splits verb and path across two annotations (`@GET` and
            // `@Path`); Spring keeps both in one. Joining every path annotation
            // covers both shapes.
            let mut subs: Vec<String> = vec![String::new()];
            for ann in &method.anns {
                let Some(values) = path_values(ann) else { continue };
                if values.len() == 1 && values[0].is_empty() {
                    continue;
                }
                let mut next = Vec::new();
                for base in &subs {
                    for value in &values {
                        let tail = if value.starts_with('/') { value.clone() } else { format!("/{value}") };
                        next.push(format!("{}{}", base.trim_end_matches('/'), tail));
                    }
                }
                subs = next;
            }
            let mut summary = first_line(&method.doc);
            if summary.is_empty() {
                summary = method.name.clone();
            }
            let description = if method.doc.is_empty() {
                if class_summary.is_empty() {
                    format!("{}#{}", class.name, method.name)
                } else {
                    format!("{class_summary}\n\n{}#{}", class.name, method.name)
                }
            } else {
                method.doc.clone()
            };
            let mut params = Vec::new();
            let mut body_type: Option<String> = None;
            for param in &method.params {
                let (kind, declared, required) = param_kind(param);
                let name = declared.unwrap_or_else(|| param.name.clone());
                let summary = first_line(&param.doc);
                if kind == ParamKind::Body {
                    body_type = Some(param.ty.clone());
                }
                params.push(RawParam { name, ty: param.ty.clone(), kind, required, summary });
            }
            for prefix in &prefixes {
                for sub in &subs {
                    out.push(RawEndpoint {
                        framework,
                        http_method: http_method.clone(),
                        path: join_path(prefix, sub),
                        class_name: class.name.clone(),
                        method_name: method.name.clone(),
                        package: model.package.clone(),
                        at: method.anns.first().map_or(method.at, |a| a.at),
                        summary: summary.clone(),
                        description: description.clone(),
                        params: params.clone(),
                        body_type: body_type.clone(),
                        return_type: method.ret.clone(),
                    });
                }
            }
        }
    }
    let _ = lexed;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPRING: &str = r#"
package com.acme.user;

import org.springframework.web.bind.annotation.*;

/** 用户接口。 */
@RestController
@RequestMapping("/api/users")
public class UserController {

    /** 列表查询。 */
    @GetMapping("/list")
    public Result<List<UserDto>> list(@RequestParam(value = "q", required = false) String q,
                                      @RequestParam(defaultValue = "1") int page) {
        return null;
    }

    /** 详情。 */
    @GetMapping("/{id}")
    public Result<UserDto> detail(@PathVariable("id") Long id,
                                  @RequestHeader(value = "X-Token") String token) {
        return null;
    }

    @RequestMapping(value = "/create", method = RequestMethod.POST)
    public void create(@RequestBody CreateUser form) {
    }

    @PostMapping(value = {"/a", "/b"})
    public void multi() {
    }

    private static final String CONSTANT = "x";

    public void helper() {
        String local = "noise";
    }
}
"#;

    #[test]
    fn reads_spring_routes() {
        let model = parse(SPRING);
        assert_eq!(model.package, "com.acme.user");
        assert_eq!(model.classes.len(), 1);
        let eps = endpoints(&model, SPRING);
        let routes: Vec<(String, String)> = eps
            .iter()
            .map(|e| (e.http_method.clone().unwrap_or_else(|| "ANY".into()), e.path.clone()))
            .collect();
        assert_eq!(
            routes,
            vec![
                ("GET".to_string(), "/api/users/list".to_string()),
                ("GET".to_string(), "/api/users/{id}".to_string()),
                ("POST".to_string(), "/api/users/create".to_string()),
                ("POST".to_string(), "/api/users/a".to_string()),
                ("POST".to_string(), "/api/users/b".to_string()),
            ]
        );
        assert_eq!(eps[0].summary, "列表查询。");
        assert_eq!(eps[0].return_type, "Result<List<UserDto>>");
        assert_eq!(eps[0].class_name, "UserController");
        assert_eq!(eps[1].params[0].kind, ParamKind::Path);
        assert_eq!(eps[1].params[0].name, "id");
        assert_eq!(eps[1].params[1].kind, ParamKind::Header);
        assert!(!eps[0].params[0].required);
        assert_eq!(eps[2].body_type.as_deref(), Some("CreateUser"));
    }

    #[test]
    fn reads_fields_and_skips_locals() {
        let src = r#"
public class UserDto {
    /** 名字。 */
    private String name;
    private final Map<String, List<Long>> tags = new HashMap<>();
    public String getName() { return name; }
}
"#;
        let model = parse(src);
        let class = &model.classes[0];
        let names: Vec<&str> = class.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"name"), "{names:?}");
        assert!(names.contains(&"tags"), "{names:?}");
        assert_eq!(class.fields[1].ty, "Map<String, List<Long>>");
        assert_eq!(class.fields[0].doc, "名字。");
    }

    #[test]
    fn reads_jax_rs() {
        let src = r#"
@Path("/orders")
public class OrderResource {
    @GET
    @Path("/{id}")
    public Order get(@PathParam("id") long id) { return null; }
}
"#;
        let model = parse(src);
        let eps = endpoints(&model, src);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].path, "/orders/{id}");
        assert_eq!(eps[0].http_method.as_deref(), Some("GET"));
        assert_eq!(eps[0].framework, "JAX-RS");
    }

    const KOTLIN: &str = r#"
package com.acme.order

@RestController
@RequestMapping("/api/v1")
class OrderController {

    /** 订单详情。 */
    @GetMapping("/orders/{id}")
    fun order(@PathVariable("id") id: Long, @RequestParam q: String): Result<OrderDto> {
        return null
    }

    @PostMapping("/orders")
    fun create(@RequestBody body: CreateOrder) {
    }

    val unused = "noise"
}
"#;

    #[test]
    fn reads_kotlin_controllers() {
        let model = parse(KOTLIN);
        let eps = endpoints(&model, KOTLIN);
        assert_eq!(eps.len(), 2, "{eps:#?}");
        assert_eq!(eps[0].path, "/api/v1/orders/{id}");
        assert_eq!(eps[0].summary, "订单详情。");
        assert_eq!(eps[0].return_type, "Result<OrderDto>");
        assert_eq!(eps[0].params.len(), 2);
        assert_eq!((eps[0].params[0].name.as_str(), eps[0].params[0].ty.as_str()), ("id", "Long"));
        assert_eq!((eps[0].params[1].name.as_str(), eps[0].params[1].ty.as_str()), ("q", "String"));
        assert_eq!(eps[1].body_type.as_deref(), Some("CreateOrder"));
    }

    #[test]
    fn reads_record_components() {
        let src = r#"
public record UserDto(Long id, String name, @JsonProperty("user_age") int age) {}
"#;
        let model = parse(src);
        assert_eq!(model.classes.len(), 1);
        let class = &model.classes[0];
        assert_eq!(class.name, "UserDto");
        let names: Vec<&str> = class.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "age"]);
        assert_eq!(class.fields[2].ty, "int");
        assert_eq!(class.fields[2].anns[0].name, "JsonProperty");
    }

    #[test]
    fn kotlin_data_class_and_properties() {
        let src = r#"
data class CreateOrder(
    @JsonProperty("user_id") val userId: Long,
    val amount: Double
)

class Holder {
    private val tags: List<String> = emptyList()
    val count: Int
}
"#;
        let model = parse(src);
        let data = &model.classes[0];
        assert_eq!(data.name, "CreateOrder");
        let names: Vec<&str> = data.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["userId", "amount"]);
        assert_eq!(data.fields[0].ty, "Long");
        let holder = &model.classes[1];
        let holder_names: Vec<&str> = holder.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(holder_names.contains(&"tags"), "{holder_names:?}");
        assert!(holder_names.contains(&"count"), "{holder_names:?}");
    }

    #[test]
    fn reads_type_parameters() {
        let src = r#"
public class Result<T> { private T data; }
class Pair<K, V> { private K key; private V value; }
"#;
        let model = parse(src);
        assert_eq!(model.classes[0].type_params, vec!["T".to_owned()]);
        assert_eq!(model.classes[0].fields[0].ty, "T");
        assert_eq!(model.classes[1].type_params, vec!["K".to_owned(), "V".to_owned()]);
        assert_eq!(model.classes[1].fields[0].ty, "K");
        assert_eq!(model.classes[1].fields[1].ty, "V");
    }

    #[test]
    fn kotlin_type_parameter_bounds_are_dropped() {
        let src = r#"
data class Page<T : Any>(val list: List<T>)
"#;
        let model = parse(src);
        let page = &model.classes[0];
        assert_eq!(page.name, "Page");
        assert_eq!(page.type_params, vec!["T".to_owned()], "the bound is not a parameter");
        assert_eq!(page.fields[0].name, "list");
        assert_eq!(page.fields[0].ty, "List<T>");
    }

    #[test]
    fn variance_and_annotations_do_not_hide_the_parameter_name() {
        // `out` and `T` are both identifiers, but only `T` is the parameter: a
        // wrong name here leaves `data: T` unsubstituted, which is worse than
        // the unwrapped response it replaced.
        let src = r#"
class ApiResult<out T : Any>(val data: T)
class Wrapped<@Ann T>(val value: T)
"#;
        let model = parse(src);
        assert_eq!(model.classes[0].type_params, vec!["T".to_owned()]);
        assert_eq!(model.classes[0].fields[0].ty, "T");
        assert_eq!(model.classes[1].type_params, vec!["T".to_owned()]);
        assert_eq!(model.classes[1].fields[0].ty, "T");
    }
}
