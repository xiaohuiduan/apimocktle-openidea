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
