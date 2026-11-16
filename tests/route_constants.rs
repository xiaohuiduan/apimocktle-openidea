//! Route paths written as constants.
//!
//! Real controllers often publish `@PostMapping(RouteConstant.ITEM_QUERY)`
//! instead of a literal path, with the constant defined in a shared module.
//! Every method then used to land on `/`, so a controller collapsed into one
//! record in ApiMocktle. The constant is resolved across files here.

use openidea_apimocktle::scan;
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Location, Position, Range, TextMatch, TextSearch};

const ROOT: &str = "/proj";

const CONSTANTS: &str = r#"
package com.acme.common;

public class RouteConstant {
    /** 用户查询条目 */
    public static final String ITEM_QUERY = "1001";
    public static final String ITEM_QUERY_GUEST = "/1002";
    private static final String PREFIX = "/api";
    public static final String NESTED = PREFIX + "/nested";
}
"#;

const CONTROLLER: &str = r#"
package com.acme.item;

import com.acme.common.RouteConstant;

@RestController
public class ItemController {
    @PostMapping(value = RouteConstant.ITEM_QUERY)
    public String queryItem() { return null; }

    @GetMapping(RouteConstant.ITEM_QUERY_GUEST)
    public String queryGuest() { return null; }

    @PostMapping("/literal")
    public String literal() { return null; }
}
"#;

const PREFIXED: &str = r#"
package com.acme.item;

import com.acme.common.RouteConstant;

@RestController
@RequestMapping(RouteConstant.PREFIX)
public class AdminItemController {
    @PostMapping(value = RouteConstant.ITEM_QUERY)
    public String qry() { return null; }
}
"#;

fn host() -> MockHost {
    let host = MockHost::install().with_capabilities(&["project.read"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    let files = [
        ("/proj/common/src/main/java/com/acme/common/RouteConstant.java", CONSTANTS),
        ("/proj/src/main/java/com/acme/item/ItemController.java", CONTROLLER),
        ("/proj/src/main/java/com/acme/item/AdminItemController.java", PREFIXED),
    ];
    for (path, text) in files {
        host.add_file(path, text);
    }

    // The constant class declares no mapping, so the annotation search never
    // returns it: it has to be read on demand.
    let controllers = vec![
        TextMatch {
            location: Location {
                path: "/proj/src/main/java/com/acme/item/ItemController.java".into(),
                range: Range::new(Position::new(0, 0), Position::new(0, 1)),
            },
            line: String::new(),
        },
        TextMatch {
            location: Location {
                path: "/proj/src/main/java/com/acme/item/AdminItemController.java".into(),
                range: Range::new(Position::new(0, 0), Position::new(0, 1)),
            },
            line: String::new(),
        },
    ];
    host.on_search(move |_query, _limit| Ok(TextSearch { matches: controllers.clone(), limit_hit: false }));
    host
}

#[test]
fn a_route_named_by_a_constant_keeps_its_path() {
    let host = host();
    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &scan::Options::default()).expect("a scan");
    let mut keys: Vec<String> = scanned.endpoints.iter().map(|e| e.key()).collect();
    keys.sort();

    assert_eq!(
        keys,
        vec![
            "GET /1002".to_owned(),
            "POST /1001".to_owned(),
            "POST /api/1001".to_owned(),
            "POST /literal".to_owned(),
        ],
        "each route keeps its own path; no collapse onto `/`"
    );
}

#[test]
fn a_route_without_any_path_still_falls_back_to_the_root() {
    // A method whose mapping names something unresolvable must not swallow the
    // others: it stays on `/` while its neighbours keep their own paths.
    let host = MockHost::install().with_capabilities(&["project.read"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.add_file(
        &format!("{ROOT}/src/A.java"),
        r#"
package com.acme;
@RestController
public class A {
    @PostMapping(Missing.UNKNOWN)
    public String a() { return null; }
    @PostMapping("/b")
    public String b() { return null; }
}
"#,
    );
    let paths = vec![TextMatch {
        location: Location {
            path: format!("{ROOT}/src/A.java"),
            range: Range::new(Position::new(0, 0), Position::new(0, 1)),
        },
        line: String::new(),
    }];
    host.on_search(move |_query, _limit| Ok(TextSearch { matches: paths.clone(), limit_hit: false }));

    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &scan::Options::default()).expect("a scan");
    let mut keys: Vec<String> = scanned.endpoints.iter().map(|e| e.key()).collect();
    keys.sort();
    assert_eq!(keys, vec!["POST /".to_owned(), "POST /b".to_owned()]);
}

/// Two modules may each declare `PATH`. The qualifier names the declaring
/// class, so `ARoutes.PATH` and `BRoutes.PATH` must not answer with the same
/// value — that used to collapse both routes onto one path, and deduplication
/// then dropped the second API altogether.
const A_ROUTES: &str = r#"
package com.acme.a;

public class ARoutes {
    public static final String PATH = "/a";
}
"#;

const B_ROUTES: &str = r#"
package com.acme.b;

public class BRoutes {
    public static final String PATH = "/b";
}
"#;

const TWO_QUALIFIERS: &str = r#"
package com.acme.item;

import com.acme.a.ARoutes;
import com.acme.b.BRoutes;

@RestController
public class TwoController {
    @GetMapping(ARoutes.PATH)
    public String a() { return null; }

    @GetMapping(BRoutes.PATH)
    public String b() { return null; }
}
"#;

fn scan_with(files: &[(&str, &str)], searched: &[&str]) -> Vec<String> {
    let host = MockHost::install().with_capabilities(&["project.read"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    for (path, text) in files {
        host.add_file(&format!("{ROOT}/{path}"), text);
    }
    let matches: Vec<TextMatch> = searched
        .iter()
        .map(|path| TextMatch {
            location: Location {
                path: format!("{ROOT}/{path}"),
                range: Range::new(Position::new(0, 0), Position::new(0, 1)),
            },
            line: String::new(),
        })
        .collect();
    host.on_search(move |_query, _limit| Ok(TextSearch { matches: matches.clone(), limit_hit: false }));
    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &scan::Options::default()).expect("a scan");
    let mut keys: Vec<String> = scanned.endpoints.iter().map(|e| e.key()).collect();
    keys.sort();
    keys
}

#[test]
fn repeated_constant_names_resolve_through_their_declaring_class() {
    let keys = scan_with(
        &[
            ("a/ARoutes.java", A_ROUTES),
            ("b/BRoutes.java", B_ROUTES),
            ("item/TwoController.java", TWO_QUALIFIERS),
        ],
        // The constant classes declare no mapping, so the search never returns
        // them; they are read on demand from the qualifier.
        &["item/TwoController.java"],
    );
    assert_eq!(
        keys,
        vec!["GET /a".to_owned(), "GET /b".to_owned()],
        "each qualified constant resolves through its own class"
    );
}

const UNQUALIFIED_SAME_FILE: &str = r#"
package com.acme.item;

import com.acme.a.ARoutes;
import com.acme.b.BRoutes;

@RestController
public class LocalController {
    private static final String PATH = "/local";

    @GetMapping(PATH)
    public String local() { return null; }

    @GetMapping(ARoutes.PATH)
    public String a() { return null; }
}
"#;

#[test]
fn an_unqualified_constant_prefers_the_enclosing_class() {
    let keys = scan_with(
        &[
            ("a/ARoutes.java", A_ROUTES),
            ("b/BRoutes.java", B_ROUTES),
            ("item/LocalController.java", UNQUALIFIED_SAME_FILE),
        ],
        &["item/LocalController.java"],
    );
    assert_eq!(
        keys,
        vec!["GET /a".to_owned(), "GET /local".to_owned()],
        "the controller's own PATH wins over another class' PATH"
    );
}

const STATIC_IMPORTED: &str = r#"
package com.acme.item;

import static com.acme.b.BRoutes.PATH;

@RestController
public class StaticImportController {
    @GetMapping(PATH)
    public String b() { return null; }
}
"#;

#[test]
fn a_static_import_resolves_without_a_qualifier() {
    let keys = scan_with(
        &[("b/BRoutes.java", B_ROUTES), ("item/StaticImportController.java", STATIC_IMPORTED)],
        &["item/StaticImportController.java"],
    );
    assert_eq!(keys, vec!["GET /b".to_owned()], "the static import names the class");
}

const AMBIGUOUS: &str = r#"
package com.acme.item;

@RestController
public class AmbiguousController {
    @GetMapping(PATH)
    public String p() { return null; }

    @GetMapping("/literal")
    public String literal() { return null; }
}
"#;

#[test]
fn an_ambiguous_bare_name_is_not_guessed() {
    // `PATH` is declared by two classes with different values and nothing here
    // says which one is meant: the route falls back to `/` instead of silently
    // picking one, and its neighbour is untouched.
    let keys = scan_with(
        &[
            ("a/ARoutes.java", A_ROUTES),
            ("b/BRoutes.java", B_ROUTES),
            ("item/AmbiguousController.java", AMBIGUOUS),
        ],
        &["item/AmbiguousController.java"],
    );
    assert_eq!(
        keys,
        vec!["GET /".to_owned(), "GET /literal".to_owned()],
        "an unresolvable bare name stays on `/`"
    );
}
