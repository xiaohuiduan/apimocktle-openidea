//! The endpoint model and the JSON Schema generation YApi expects for request
//! and response bodies.

use serde_json::{Map, Value, json};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamSpec {
    pub name: String,
    pub ty: String,
    pub required: bool,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldSpec {
    pub name: String,
    pub ty: String,
    pub description: String,
    /// Resolved nested fields, when the type is a known class.
    pub children: Vec<FieldSpec>,
}

/// A class the resolver found. The declared type parameters travel with the
/// fields because a wrapper's `data: T` only becomes a real type once the
/// arguments of `Result<X>` are substituted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedClass {
    pub params: Vec<String>,
    pub fields: Vec<FieldSpec>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Endpoint {
    /// `None` accepts any method.
    pub method: Option<String>,
    pub path: String,
    pub framework: String,
    pub class_name: String,
    pub method_name: String,
    pub package: String,
    /// Absolute path inside the project.
    pub file: String,
    /// Zero-based line and UTF-16 column of the mapping annotation.
    pub line: u32,
    pub col: u32,
    pub summary: String,
    pub description: String,
    pub path_params: Vec<ParamSpec>,
    pub query_params: Vec<ParamSpec>,
    pub header_params: Vec<ParamSpec>,
    /// Multipart / form parts (`@RequestPart`, `@FormParam`, `@ModelAttribute`).
    /// YApi reads these from `req_body_form`, not from the body schema.
    pub form_params: Vec<ParamSpec>,
    pub body_type: Option<String>,
    pub return_type: String,
}

impl Endpoint {
    /// `Controller#method GET /path`, the label used in lists and results.
    pub fn label(&self) -> String {
        let method = self.method.clone().unwrap_or_else(|| "ANY".into());
        format!("{method} {}", self.path)
    }

    pub fn handler(&self) -> String {
        format!("{}#{}", self.class_name, self.method_name)
    }

    /// The YApi `method` field: a concrete verb, never empty.
    pub fn yapi_method(&self) -> String {
        self.method.clone().unwrap_or_else(|| "GET".into())
    }

    pub fn key(&self) -> String {
        format!("{} {}", self.yapi_method(), self.path)
    }
}

/// Splits `List<Map<String, Long>>` into its base name and type arguments.
pub fn split_type(ty: &str) -> (String, Vec<String>) {
    let ty = ty.trim();
    let ty = ty.strip_prefix("? extends ").unwrap_or(ty).trim();
    let Some(open) = ty.find('<') else {
        return (ty.trim_end_matches("[]").trim().to_owned(), Vec::new());
    };
    if !ty.ends_with('>') {
        return (ty.to_owned(), Vec::new());
    }
    let base = ty[..open].trim().to_owned();
    let inner = &ty[open + 1..ty.len() - 1];
    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in inner.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                args.push(inner[start..i].trim().to_owned());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = inner[start..].trim();
    if !tail.is_empty() {
        args.push(tail.to_owned());
    }
    (base, args)
}

/// The simple name of a possibly qualified or generic type.
pub fn simple_name(ty: &str) -> String {
    let (base, _) = split_type(ty);
    let base = base.trim().trim_end_matches("[]");
    base.rsplit('.').next().unwrap_or(base).to_owned()
}

/// Wrappers unwrapped before a body is described.
const WRAPPERS: &[&str] = &[
    "Result",
    "R",
    "Res",
    "Resp",
    "Response",
    "ResponseEntity",
    "ApiResponse",
    "ApiResult",
    "CommonResult",
    "BaseResponse",
    "Optional",
    "List",
    "Set",
    "Collection",
    "Iterable",
    "ArrayList",
    "LinkedList",
    "Page",
    "PageResult",
    "PageInfo",
    "IPage",
];

pub(crate) fn is_array_like(base: &str) -> bool {
    matches!(
        base,
        "List" | "Set" | "Collection" | "Iterable" | "ArrayList" | "LinkedList" | "Queue" | "Deque" | "Stream"
    )
}

fn is_object_like(base: &str) -> bool {
    matches!(base, "Map" | "HashMap" | "LinkedHashMap" | "TreeMap" | "JSONObject" | "JsonObject")
}

/// The sample property a `Map<K, V>` is described under. ApiMocktle only reads
/// `properties` from a schema, the same way YApi does, so a map written as
/// `additionalProperties` renders as an empty object; a concrete key named
/// after K keeps the value schema visible. The upstream IntelliJ plugin does
/// the same in its `JsonSchemaBuilder.buildMapSchema`.
fn map_key_name(key_ty: &str) -> String {
    match simple_name(key_ty).as_str() {
        "String" | "CharSequence" | "char" | "Character" | "UUID" | "Object" => "string".to_owned(),
        "int" | "Integer" | "long" | "Long" | "short" | "Short" | "byte" | "Byte" => "0".to_owned(),
        _ => "key".to_owned(),
    }
}

fn scalar_schema(base: &str) -> Option<Value> {
    let ty = match base {
        "String" | "CharSequence" | "StringBuilder" | "char" | "Character" | "UUID" | "LocalDate"
        | "LocalDateTime" | "Date" | "Instant" | "BigDecimal" | "BigInteger" | "Object" => "string",
        "byte" | "short" | "int" | "long" | "float" | "double" => "number",
        "Byte" | "Short" | "Integer" | "Long" | "Float" | "Double" => "number",
        "boolean" | "Boolean" | "AtomicBoolean" => "boolean",
        "void" | "Void" => return None,
        _ => return None,
    };
    Some(json!({ "type": ty }))
}

/// Resolves a class name to its declared parameters and fields. Returning
/// `None` means the class has no source in the project: a single-argument
/// generic is then unwrapped to its argument (a dependency jar's wrapper), and
/// anything else is described as a plain object.
/// The third argument is the scope the name is written in: the qualified
/// identity of the type whose fields are being walked (`com.acme::A::Request`),
/// or the endpoint's own scope at the top level. A field mention carries only a
/// simple name, and the type it means is the one in that scope — without this,
/// `AController.Request.detail: Detail` would be looked up from `AController`
/// and could find `BController`'s `Detail` instead.
pub type Resolver<'a> = dyn Fn(&str, u32, &str) -> Option<ResolvedClass> + 'a;

/// The qualified identity of `name` as declared inside `scope`.
///
/// `com.acme::AController` + `Request` is `com.acme::AController::Request`; that is
/// the scope its own fields resolve in, so `Detail` inside it means
/// `com.acme::AController::Request::Detail` rather than some other `Detail`.
fn qualified_in(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_owned()
    } else {
        format!("{scope}::{name}")
    }
}

/// The JSON Schema for one type, as YApi stores it.
pub fn type_schema(ty: &str, depth: u32, resolve: &Resolver<'_>, scope: &str) -> Value {
    let ty = ty.trim();
    if ty.is_empty() {
        return json!({ "type": "string" });
    }
    if ty.ends_with("[]") {
        let inner = ty.trim_end_matches("[]");
        return json!({ "type": "array", "items": type_schema(inner, depth, resolve, scope) });
    }
    let (base, args) = split_type(ty);
    let base = base.rsplit('.').next().unwrap_or(&base).to_owned();
    if is_object_like(&base) {
        let value = args.get(1).map(|v| type_schema(v, depth, resolve, scope)).unwrap_or_else(|| json!({ "type": "string" }));
        let key = args.first().map(|k| map_key_name(k)).unwrap_or_else(|| "string".to_owned());
        let mut properties = Map::new();
        properties.insert(key, value);
        return json!({ "type": "object", "description": "Map", "properties": properties });
    }
    if is_array_like(&base) {
        let item = args.first().map(|v| type_schema(v, depth, resolve, scope)).unwrap_or_else(|| json!({ "type": "string" }));
        return json!({ "type": "array", "items": item });
    }
    if let Some(scalar) = scalar_schema(&base) {
        return scalar;
    }
    if WRAPPERS.contains(&base.as_str()) {
        // A wrapper declared in the scanned project keeps its own shape: the
        // field table on the server only reads the top-level `properties`, and
        // `data` needs the type arguments substituted to stay a real type. The
        // `depth > 0` guard keeps a self-referential wrapper from recursing
        // forever once the depth budget is spent.
        if depth > 0
            && let Some(resolved) = resolve(&base, depth, scope).filter(|rc| !rc.fields.is_empty())
        {
            let mapped = substitute(&resolved.fields, &resolved.params, &args);
            return object_schema_in(&mapped, depth, resolve, &qualified_in(scope, &base));
        }
        // A wrapper that lives in a dependency (`ResponseEntity`, `Optional`)
        // cannot be resolved, so it is unwrapped exactly as before.
        if let Some(first) = args.first() {
            return type_schema(first, depth, resolve, scope);
        }
    }
    if depth == 0 {
        return json!({ "type": "object" });
    }
    // A generic class that is not a known wrapper (`Wrapper<T>`, `Envelope<T>`)
    // still describes itself, but its parameters must be substituted or every
    // `T` field collapses into a plain object.
    match resolve(&base, depth, scope) {
        Some(resolved) if !resolved.fields.is_empty() => {
            let mapped = substitute(&resolved.fields, &resolved.params, &args);
            object_schema_in(&mapped, depth, resolve, &qualified_in(scope, &base))
        }
        // A wrapper declared in a dependency jar (`ApiEnvelope<T>`,
        // `PageResult<T>`, …) has no source here, so its own fields cannot be
        // read. The only thing left to describe is the type argument: unwrap
        // it exactly like `ResponseEntity`, instead of degrading the whole
        // body to a bare `{"type": "object"}` with no fields at all.
        _ if args.len() == 1 => type_schema(&args[0], depth, resolve, scope),
        _ => json!({ "type": "object" }),
    }
}

/// Replaces declared type parameters with the actual arguments:
/// `data: T` becomes `data: List<UserDto>` for `Result<List<UserDto>>`.
fn substitute(fields: &[FieldSpec], params: &[String], args: &[String]) -> Vec<FieldSpec> {
    fields
        .iter()
        .map(|field| FieldSpec {
            name: field.name.clone(),
            ty: substitute_type(&field.ty, params, args),
            description: field.description.clone(),
            children: substitute(&field.children, params, args),
        })
        .collect()
}

/// Substitutes whole identifiers only, so `T` is replaced while `Test`, `Time`
/// and `TaskDto` are left alone. A missing argument leaves the parameter as is.
pub(crate) fn substitute_type(ty: &str, params: &[String], args: &[String]) -> String {
    let mut out = String::with_capacity(ty.len());
    let mut word = String::new();
    for c in ty.chars() {
        if is_ident_char(c) {
            word.push(c);
            continue;
        }
        out.push_str(mapped_word(&word, params, args));
        word.clear();
        out.push(c);
    }
    out.push_str(mapped_word(&word, params, args));
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn mapped_word<'a>(word: &'a str, params: &[String], args: &'a [String]) -> &'a str {    if word.is_empty() {
        return word;
    }
    params
        .iter()
        .position(|param| param == word)
        .and_then(|index| args.get(index))
        .map_or(word, String::as_str)
}

/// `object_schema` for fields declared in `scope`.
fn object_schema_in(fields: &[FieldSpec], depth: u32, resolve: &Resolver<'_>, scope: &str) -> Value {
    let mut properties = Map::new();
    for field in fields {
        let mut schema = if field.children.is_empty() {
            type_schema(&field.ty, depth.saturating_sub(1), resolve, scope)
        } else {
            object_schema_in(&field.children, depth.saturating_sub(1), resolve, scope)
        };
        if !field.description.is_empty()
            && let Some(obj) = schema.as_object_mut()
        {
            obj.insert("description".into(), Value::String(field.description.clone()));
        }
        properties.insert(field.name.clone(), schema);
    }
    json!({ "type": "object", "properties": Value::Object(properties) })
}

/// The `res_body` / `req_body_other` JSON Schema of an endpoint.
pub fn body_schema(ty: &str, depth: u32, resolve: &Resolver<'_>, scope: &str) -> String {
    type_schema(ty, depth, resolve, scope).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_dto() -> ResolvedClass {
        ResolvedClass {
            params: Vec::new(),
            fields: vec![
                FieldSpec { name: "name".into(), ty: "String".into(), description: "名字".into(), children: vec![] },
                FieldSpec { name: "age".into(), ty: "int".into(), description: String::new(), children: vec![] },
                FieldSpec { name: "active".into(), ty: "boolean".into(), description: String::new(), children: vec![] },
            ],
        }
    }

    fn wrapper() -> ResolvedClass {
        ResolvedClass {
            params: vec!["T".into()],
            fields: vec![
                FieldSpec { name: "code".into(), ty: "int".into(), description: String::new(), children: vec![] },
                FieldSpec { name: "msg".into(), ty: "String".into(), description: String::new(), children: vec![] },
                FieldSpec { name: "data".into(), ty: "T".into(), description: String::new(), children: vec![] },
            ],
        }
    }

    fn only_dtos(name: &str, _depth: u32, _scope: &str) -> Option<ResolvedClass> {
        match name {
            "UserDto" => Some(user_dto()),
            _ => None,
        }
    }

    #[test]
    fn splits_generics() {
        assert_eq!(split_type("Result<List<User>>").0, "Result");
        assert_eq!(split_type("Result<List<User>>").1, vec!["List<User>"]);
        assert_eq!(split_type("Map<String, List<Long>>").1, vec!["String", "List<Long>"]);
        assert_eq!(split_type("int").1.len(), 0);
        assert_eq!(simple_name("com.acme.UserDto"), "UserDto");
    }

    #[test]
    fn schema_unwraps_result_and_lists() {
        let resolve: &Resolver = &only_dtos;
        let schema: Value = serde_json::from_str(&body_schema("Result<List<UserDto>>", 2, resolve, "")).unwrap();
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "object");
        assert_eq!(schema["items"]["properties"]["name"]["type"], "string");
        assert_eq!(schema["items"]["properties"]["name"]["description"], "名字");
        assert_eq!(schema["items"]["properties"]["age"]["type"], "number");
    }

    #[test]
    fn keeps_a_resolved_wrapper_and_substitutes_a_list_argument() {
        let resolve: &Resolver = &|name: &str, _depth: u32, _scope: &str| match name {
            "Result" => Some(wrapper()),
            "UserDto" => Some(user_dto()),
            _ => None,
        };
        let schema: Value = serde_json::from_str(&body_schema("Result<List<UserDto>>", 2, resolve, "")).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["code"]["type"], "number");
        assert_eq!(schema["properties"]["msg"]["type"], "string");
        assert_eq!(schema["properties"]["data"]["type"], "array");
        let items = &schema["properties"]["data"]["items"]["properties"];
        assert_eq!(items["name"]["type"], "string");
        assert_eq!(items["age"]["type"], "number");
        assert_eq!(items["active"]["type"], "boolean");
    }

    #[test]
    fn keeps_a_resolved_wrapper_and_substitutes_a_single_argument() {
        let resolve: &Resolver = &|name: &str, _depth: u32, _scope: &str| match name {
            "Result" => Some(wrapper()),
            "UserDto" => Some(user_dto()),
            _ => None,
        };
        let schema: Value = serde_json::from_str(&body_schema("Result<UserDto>", 2, resolve, "")).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["data"]["type"], "object");
        assert_eq!(schema["properties"]["data"]["properties"]["name"]["type"], "string");
        assert_eq!(schema["properties"]["data"]["properties"]["active"]["type"], "boolean");
    }

    #[test]
    fn an_unresolvable_wrapper_is_unwrapped() {
        // `Result` lives in a dependency here, exactly like `ResponseEntity`:
        // the old unwrapping has to stay in place.
        let resolve: &Resolver = &only_dtos;
        let schema: Value = serde_json::from_str(&body_schema("Result<List<UserDto>>", 2, resolve, "")).unwrap();
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "object");

        // A wrapper with no fields carries nothing the field table could show.
        let empty: &Resolver = &|name: &str, _depth: u32, _scope: &str| match name {
            "Result" => Some(ResolvedClass::default()),
            "UserDto" => Some(user_dto()),
            _ => None,
        };
        let schema: Value = serde_json::from_str(&body_schema("Result<List<UserDto>>", 2, empty, "")).unwrap();
        assert_eq!(schema["type"], "array");

        // `ResponseEntity` is a dependency type too, so it is still unwrapped.
        let response: Value =
            serde_json::from_str(&body_schema("ResponseEntity<List<UserDto>>", 2, resolve, "")).unwrap();
        assert_eq!(response["type"], "array");
        assert_eq!(response["items"]["type"], "object");

        // `Optional` is named in the README as unwrapped as well.
        let optional: Value = serde_json::from_str(&body_schema("Optional<UserDto>", 2, resolve, "")).unwrap();
        assert_eq!(optional["type"], "object");
        assert_eq!(optional["properties"]["name"]["type"], "string");
    }

    #[test]
    fn a_wrapper_from_a_jar_is_unwrapped_to_its_argument() {
        // `ApiEnvelope` is not in WRAPPERS and has no source in the project:
        // a dependency jar cannot be read, so the envelope's own fields are out
        // of reach and its single type argument is what still can be described.
        let resolve: &Resolver = &only_dtos;
        let schema: Value =
            serde_json::from_str(&body_schema("ApiEnvelope<Map<String, List<UserDto>>>", 2, resolve, "")).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["description"], "Map");
        assert_eq!(schema["properties"]["string"]["type"], "array");
        assert_eq!(schema["properties"]["string"]["items"]["properties"]["name"]["type"], "string");

        // Two arguments stay opaque: `Pair<A, B>` has no single payload to
        // stand in for the unknown class.
        let pair: Value = serde_json::from_str(&body_schema("Pair<UserDto, String>", 2, resolve, "")).unwrap();
        assert_eq!(pair["type"], "object");
        assert!(pair["properties"].is_null(), "{pair}");

        // A wrapper the project declares itself keeps its own shape, as before.
        let known: &Resolver = &|name: &str, _depth: u32, _scope: &str| match name {
            "ApiEnvelope" => Some(wrapper()),
            "UserDto" => Some(user_dto()),
            _ => None,
        };
        let kept: Value = serde_json::from_str(&body_schema("ApiEnvelope<UserDto>", 2, known, "")).unwrap();
        assert_eq!(kept["properties"]["data"]["properties"]["name"]["type"], "string");
    }

    #[test]
    fn a_spent_depth_budget_unwraps_the_wrapper() {
        // At depth 0 the wrapper is unwrapped byte for byte as it was before
        // wrappers were kept; the guard is also what stops a self-referential
        // wrapper from re-entering `object_schema` until the stack runs out.
        let resolve: &Resolver = &|name: &str, _depth: u32, _scope: &str| match name {
            "Result" => Some(ResolvedClass {
                params: vec!["T".into()],
                // A field that refers back to the wrapper: without the guard
                // this is where the recursion would never end.
                fields: vec![
                    FieldSpec {
                        name: "next".into(),
                        ty: "Result<T>".into(),
                        description: String::new(),
                        children: vec![],
                    },
                    FieldSpec { name: "data".into(), ty: "T".into(), description: String::new(), children: vec![] },
                ],
            }),
            "UserDto" => Some(user_dto()),
            _ => None,
        };
        let schema: Value = serde_json::from_str(&body_schema("Result<List<UserDto>>", 0, resolve, "")).unwrap();
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "object");
    }

    #[test]
    fn a_custom_generic_class_substitutes_its_argument() {
        // `Envelope` is not in WRAPPERS, so it takes the plain-class path; its
        // parameter still has to be substituted or `value` loses its shape.
        let resolve: &Resolver = &|name: &str, _depth: u32, _scope: &str| match name {
            "Envelope" => Some(ResolvedClass {
                params: vec!["T".into()],
                fields: vec![FieldSpec { name: "value".into(), ty: "T".into(), description: String::new(), children: vec![] }],
            }),
            "UserDto" => Some(user_dto()),
            _ => None,
        };
        let schema: Value = serde_json::from_str(&body_schema("Envelope<UserDto>", 2, resolve, "")).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["value"]["type"], "object");
        assert_eq!(schema["properties"]["value"]["properties"]["name"]["type"], "string");
    }

    #[test]
    fn substitute_only_replaces_whole_identifiers() {
        let fields = vec![
            FieldSpec { name: "a".into(), ty: "Test".into(), description: String::new(), children: vec![] },
            FieldSpec { name: "b".into(), ty: "Time".into(), description: String::new(), children: vec![] },
            FieldSpec { name: "c".into(), ty: "TaskDto".into(), description: String::new(), children: vec![] },
            FieldSpec { name: "d".into(), ty: "T".into(), description: String::new(), children: vec![] },
            FieldSpec { name: "e".into(), ty: "Map<String, T>".into(), description: String::new(), children: vec![] },
            FieldSpec {
                name: "f".into(),
                ty: "List<T>".into(),
                description: String::new(),
                children: vec![FieldSpec { name: "inner".into(), ty: "T".into(), description: String::new(), children: vec![] }],
            },
        ];
        let out = substitute(&fields, &["T".to_owned()], &["List<UserDto>".to_owned()]);
        assert_eq!(out[0].ty, "Test");
        assert_eq!(out[1].ty, "Time");
        assert_eq!(out[2].ty, "TaskDto");
        assert_eq!(out[3].ty, "List<UserDto>");
        assert_eq!(out[4].ty, "Map<String, List<UserDto>>");
        assert_eq!(out[5].ty, "List<List<UserDto>>");
        assert_eq!(out[5].children[0].ty, "List<UserDto>", "children are substituted too");
    }

    #[test]
    fn missing_arguments_leave_parameters_alone() {
        let fields = vec![
            FieldSpec { name: "key".into(), ty: "K".into(), description: String::new(), children: vec![] },
            FieldSpec { name: "value".into(), ty: "V".into(), description: String::new(), children: vec![] },
        ];
        let out = substitute(&fields, &["K".to_owned(), "V".to_owned()], &["String".to_owned()]);
        assert_eq!(out[0].ty, "String");
        assert_eq!(out[1].ty, "V");
    }

    #[test]
    fn schema_handles_maps_and_arrays() {
        let resolve: &Resolver = &|_: &str, _depth: u32, _scope: &str| None;
        // ApiMocktle only renders `properties` (a map written as
        // `additionalProperties` shows as `{}`), so a Map is described under
        // one sample key named after K.
        let map: Value = serde_json::from_str(&body_schema("Map<String, Long>", 2, resolve, "")).unwrap();
        assert_eq!(map["type"], "object");
        assert_eq!(map["description"], "Map");
        assert_eq!(map["properties"]["string"]["type"], "number");
        assert!(map.get("additionalProperties").is_none());

        let numeric: Value = serde_json::from_str(&body_schema("Map<Long, List<UserDto>>", 2, resolve, "")).unwrap();
        assert_eq!(numeric["properties"]["0"]["type"], "array");

        let arr: Value = serde_json::from_str(&body_schema("String[]", 2, resolve, "")).unwrap();
        assert_eq!(arr["items"]["type"], "string");
    }
}
