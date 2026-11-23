//! A class whose name differs from its file name is found by the declaration
//! search fallback, which records the name as "not in the index". This pins
//! down that a second lookup of the same name still works.
//!
//! Route constants are the case that hits it: `@GetMapping(RouteConstants.LIST)`
//! in two controllers of one module resolves the same class name twice. It
//! works because `Consts::value_of` caches the resolved value under
//! `class.name` (`Consts::values`) before a second lookup can reach the index —
//! so the short-circuit on a recorded miss is never consulted with a name that
//! was actually found. That is a subtle ordering dependency; this test is what
//! keeps it from being broken silently.

use std::collections::BTreeMap;
use std::sync::Arc;

use openidea_apimocktle::scan::{self, Options};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Error, HttpRequest, HttpResponse, Location, Position, Range, TextMatch, TextQuery, TextSearch};

const ROOT: &str = "/proj";

#[test]
fn a_constant_class_away_from_its_file_name_resolves_for_every_controller() {
    let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.on_fetch(|_: &HttpRequest| -> Result<HttpResponse, Error> {
        Err(Error::Failed("no network".into()))
    });

    // The class is `RouteConstants`; the file is `Routes.java`.
    let constants = r#"package com.acme;

public final class RouteConstants {
    public static final String LIST = "/list";
    public static final String DETAIL = "/detail";
}
"#;
    let controller = |n: u32| {
        format!(
            r#"package com.acme;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/c{n}")
public class Ctrl{n}Controller {{
    @GetMapping(RouteConstants.LIST)
    public String list() {{ return null; }}

    @GetMapping(RouteConstants.DETAIL)
    public String detail() {{ return null; }}
}}
"#
        )
    };

    let mut files: BTreeMap<String, String> = BTreeMap::new();
    files.insert(format!("{ROOT}/src/Routes.java"), constants.to_owned());
    for n in 0..2 {
        files.insert(format!("{ROOT}/src/Ctrl{n}Controller.java"), controller(n));
    }
    for (path, text) in &files {
        host.add_file(path, text);
    }

    // A search that behaves like Find in Files: the marker query returns the
    // controllers, the declaration query finds whichever file declares the name.
    let indexed: Arc<BTreeMap<String, String>> = Arc::new(files);
    host.on_search(move |query: &TextQuery, _limit| {
        let declaration = declaration_name(&query.pattern);
        let mut matches = Vec::new();
        for (path, text) in indexed.iter() {
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
    let mut got: Vec<(String, String)> = scanned
        .endpoints
        .iter()
        .map(|e| (e.class_name.clone(), e.path.clone()))
        .collect();
    got.sort();
    println!("resolved: {got:?}");

    assert_eq!(scanned.endpoints.len(), 4, "two controllers x two methods: {got:?}");
    for n in 0..2 {
        let class = format!("Ctrl{n}Controller");
        let paths: Vec<&str> = got.iter().filter(|(c, _)| *c == class).map(|(_, p)| p.as_str()).collect();
        assert_eq!(paths.len(), 2, "{class} publishes both routes: {paths:?}");
        assert!(
            paths.contains(&format!("/api/c{n}/list").as_str()),
            "{class} resolved LIST: {paths:?}"
        );
        assert!(
            paths.contains(&format!("/api/c{n}/detail").as_str()),
            "{class} resolved DETAIL: {paths:?}"
        );
    }
}

fn declaration_name(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("(class|interface|record)")?;
    let rest = rest.trim_start().strip_prefix(r"\s+")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}
