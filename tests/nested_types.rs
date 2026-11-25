//! Nested types: a DTO declared inside a controller or another DTO.
//!
//! `class Ctrl { static class Request {} }` is common in real Spring code, and
//! the type index cannot answer it — it maps a file name to a path, and
//! `Request` lives in `Ctrl.java`. The parser has to surface the inner type as a
//! class in its own right, or the endpoint's schema comes out empty while the
//! scan still reports success.

use std::collections::BTreeMap;

use openidea_apimocktle::scan::{self, Options};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Error, HttpRequest, HttpResponse, Location, Position, Range, TextQuery, TextMatch, TextSearch};

const ROOT: &str = "/proj";

fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

fn resolve(files: &[(&str, &str)]) -> Vec<String> {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("no network in this test".into()))
    });
    let mut indexed: BTreeMap<String, String> = BTreeMap::new();
    for (rel, text) in files {
        let path = format!("{ROOT}/{rel}");
        host.add_file(&path, text);
        indexed.insert(path, (*text).to_owned());
    }
    host.on_search(move |q: &TextQuery, _l| {
        let declaration = declaration_name(&q.pattern);
        let mut matches = Vec::new();
        for (path, text) in &indexed {
            if !path.ends_with(".java") {
                continue;
            }
            let hit = match &declaration {
                Some(name) => text.contains(&format!("class {name} ")),
                None => text.contains("@RestController"),
            };
            if hit {
                matches.push(TextMatch {
                    location: Location {
                        path: path.clone(),
                        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    },
                    line: String::new(),
                });
            }
        }
        Ok(TextSearch { matches, limit_hit: false })
    });

    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &Options::default()).expect("a scan");
    let mut names: Vec<String> = scanned.dto.fields.keys().cloned().collect();
    names.sort();
    names
}

#[test]
fn a_dto_declared_inside_its_controller_resolves() {
    // The request body is an inner class, and it mentions another inner class.
    // Both have to reach the DTO index, or the schema for `/api/c/save` is empty.
    let names = resolve(&[(
        "src/Ctrl.java",
        r#"
package com.acme;

@RestController
@RequestMapping("/api/c")
public class Ctrl {
    @PostMapping("/save")
    public Request save(@RequestBody Request form) { return null; }

    public static class Request {
        private String name;
        private Detail detail;
    }

    public static class Detail {
        private String message;
    }
}
"#,
    )]);
    assert_eq!(names, vec!["Detail".to_owned(), "Request".to_owned()]);
}

#[test]
fn a_dto_declared_inside_another_dto_resolves() {
    // `Inner` lives in `Outer.java`, so the file-name index cannot find it by
    // name; the parser has to surface it from Outer's body.
    let names = resolve(&[
        (
            "src/Ctrl.java",
            r#"
package com.acme;

@RestController
@RequestMapping("/api/c")
public class Ctrl {
    @PostMapping("/save")
    public Outer save(@RequestBody Outer form) { return null; }
}
"#,
        ),
        (
            "src/Outer.java",
            r#"
package com.acme;

public class Outer {
    private Inner inner;

    public static class Inner {
        private String message;
    }
}
"#,
        ),
    ]);
    assert_eq!(names, vec!["Inner".to_owned(), "Outer".to_owned()]);
}

#[test]
fn a_field_typed_by_a_qualified_nested_class_resolves() {
    // `Outer.Inner` is how a project refers to a nested type when the simple
    // name is ambiguous; `push_names` keeps the last segment, so `Inner` has to
    // be a resolvable type.
    let names = resolve(&[
        (
            "src/Ctrl.java",
            r#"
package com.acme;

@RestController
@RequestMapping("/api/c")
public class Ctrl {
    @PostMapping("/save")
    public Holder save(@RequestBody Holder form) { return null; }
}
"#,
        ),
        (
            "src/Holder.java",
            r#"
package com.acme;

public class Holder {
    private Outer.Inner nested;
}
"#,
        ),
        (
            "src/Outer.java",
            r#"
package com.acme;

public class Outer {
    public static class Inner {
        private String message;
    }
}
"#,
        ),
    ]);
    assert!(names.contains(&"Holder".to_owned()));
    assert!(
        names.contains(&"Inner".to_owned()),
        "the qualified nested type is a type of its own: {names:?}"
    );
}

#[test]
fn a_nested_type_does_not_become_a_field_of_its_outer() {
    // Surfacing the inner class must not also turn it into a field: that would
    // add a phantom property to every outer schema. `Outer` here has exactly one
    // real field, and that is what the schema carries.
    let names = resolve(&[(
        "src/Ctrl.java",
        r#"
package com.acme;

@RestController
@RequestMapping("/api/c")
public class Ctrl {
    @PostMapping("/save")
    public Outer save(@RequestBody Outer form) { return null; }

    public static class Outer {
        private String real;
        public static class Nested {
            private String message;
        }
    }
}
"#,
    )]);
    // `Outer` is reached because the endpoint uses it. `Nested` is not: nothing
    // refers to it, and the closure only walks what the endpoints reach.
    assert_eq!(names, vec!["Outer".to_owned()], "a nested type is not a field of its outer");
}
