//! Dependency-jar resolution, end to end.
//!
//! `ApiEnvelope<T>` lives in a jar the project does not contain — exactly the
//! case that used to export a bare `{"type": "object"}`. The test seeds a mock
//! Maven repository (a POM chain plus the fixture jar) and checks that the
//! wrapper's inherited and own fields reach the schema, with `result` typed by
//! the endpoint's type argument.

use openidea_apimocktle::deps::Config as DepsConfig;
use openidea_apimocktle::{scan, yapi};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Location, Position, Range, TextMatch, TextSearch, host};
use serde_json::Value;

const ROOT: &str = "/proj";
const REPO: &str = "/repo";

const CONTROLLER: &str = r#"
package com.acme.item;

import java.util.List;
import java.util.Map;

@RestController
@RequestMapping("/api/items")
public class ItemController {
    @PostMapping("/queryItem")
    public ApiEnvelope<Map<String, List<ItemInfoDTO>>> queryItem(
            @Validated @RequestBody PageRequest dto
    ) {
        return null;
    }
}
"#;

const ITEM_DTO: &str = r#"
package com.acme.item.dto;

public class ItemInfoDTO {
    private Long itemId;
    private String itemName;
}
"#;

/// The project depends on `api`, which depends on `common`, which is the jar
/// that actually declares `ApiEnvelope` — the transitive case.
const PROJECT_POM: &str = r#"
<project>
    <modelVersion>4.0.0</modelVersion>
    <groupId>com.acme</groupId>
    <artifactId>item-service</artifactId>
    <version>1.0</version>
    <dependencies>
        <dependency>
            <groupId>com.example</groupId>
            <artifactId>api</artifactId>
            <version>1.0</version>
        </dependency>
    </dependencies>
</project>
"#;

const API_POM: &str = r#"
<project>
    <groupId>com.example</groupId>
    <artifactId>api</artifactId>
    <version>1.0</version>
    <dependencies>
        <dependency>
            <groupId>com.example</groupId>
            <artifactId>common</artifactId>
            <version>1.0</version>
        </dependency>
    </dependencies>
</project>
"#;

const COMMON_POM: &str = r#"
<project>
    <groupId>com.example</groupId>
    <artifactId>common</artifactId>
    <version>1.0</version>
</project>
"#;

fn jar() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/deps/api-envelope.jar")).expect("the fixture jar")
}

fn host(caps: &[&str]) -> MockHost {
    let host = MockHost::install().with_capabilities(caps);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.add_file(&format!("{ROOT}/pom.xml"), PROJECT_POM);
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/ItemController.java"), CONTROLLER);
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/dto/ItemInfoDTO.java"), ITEM_DTO);

    // POMs are text, so they seed the repository layout; the jar itself needs
    // an fs.write grant to land in the mock's byte store.
    host.add_external_file(&format!("{REPO}/com/example/api/1.0/api-1.0.pom"), API_POM);
    host.add_external_file(&format!("{REPO}/com/example/common/1.0/common-1.0.pom"), COMMON_POM);

    let paths = [
        format!("{ROOT}/src/main/java/com/acme/item/ItemController.java"),
        format!("{ROOT}/src/main/java/com/acme/item/dto/ItemInfoDTO.java"),
    ];
    host.on_search(move |_query, _limit| {
        Ok(TextSearch {
            matches: paths
                .iter()
                .map(|path| TextMatch {
                    location: Location {
                        path: path.clone(),
                        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    },
                    line: String::new(),
                })
                .collect(),
            limit_hit: false,
        })
    });
    host
}

fn options() -> scan::Options {
    scan::Options {
        depth: 3,
        deps: DepsConfig { enabled: true, repo: REPO.to_owned() },
        ..scan::Options::default()
    }
}

fn document() -> Value {
    let host = host(&["project.read", "storage", "fs.read:/repo", "fs.write:/repo"]);
    host::fs::write_bytes(&format!("{REPO}/com/example/common/1.0/common-1.0.jar"), &jar()).expect("seed the jar");

    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &options()).expect("a scan");
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.method_name == "queryItem")
        .expect("the endpoint");
    yapi::api_doc(endpoint, &scanned.dto, 3, openidea_apimocktle::i18n::Lang::En)
}

#[test]
fn a_wrapper_in_a_transitive_dependency_keeps_its_fields() {
    let doc = document();
    let res: Value = serde_json::from_str(doc["res_body"].as_str().expect("a schema string")).expect("json");
    assert_eq!(res["type"], "object", "{res}");
    let properties = res["properties"].as_object().expect("wrapper properties");

    // Inherited from BaseReply, which lives in the same jar.
    assert_eq!(properties["code"]["type"], "number", "{res}");
    assert_eq!(properties["message"]["type"], "string", "{res}");
    // Own fields.
    assert_eq!(properties["errorNo"]["type"], "number", "{res}");
    assert_eq!(properties["errorInfo"]["type"], "string", "{res}");
    // `T` was substituted with `Map<String, List<ItemInfoDTO>>`.
    let result = &properties["result"];
    assert_eq!(result["type"], "object", "{res}");
    assert_eq!(result["description"], "Map");
    let list = &result["properties"]["string"];
    assert_eq!(list["type"], "array", "{res}");
    assert_eq!(list["items"]["properties"]["itemId"]["type"], "number", "{res}");
    assert_eq!(list["items"]["properties"]["itemName"]["type"], "string", "{res}");

    // A second class from the same jar, as a request body: the jar is indexed
    // once and still answers the next lookup.
    let req: Value = serde_json::from_str(doc["req_body_other"].as_str().expect("a schema string")).expect("json");
    assert_eq!(req["properties"]["pageNo"]["type"], "number", "{req}");
    assert_eq!(req["properties"]["pageSize"]["type"], "number", "{req}");
}

#[test]
fn without_the_fs_grant_the_jar_is_skipped_quietly() {
    let host = host(&["project.read", "storage"]);
    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &options()).expect("a scan");
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.method_name == "queryItem")
        .expect("the endpoint");
    let doc = yapi::api_doc(endpoint, &scanned.dto, 3, openidea_apimocktle::i18n::Lang::En);
    let res: Value = serde_json::from_str(doc["res_body"].as_str().expect("a schema string")).expect("json");

    // Without the grant the jar is invisible, so the wrapper stays unknown and
    // the fallback unwraps the single type argument instead.
    assert_eq!(res["description"], "Map", "{res}");
    assert!(res["properties"]["errorNo"].is_null(), "{res}");
}

/// The same end-to-end path for a wrapper whose class also implements an
/// interface: javac then writes the interface into the class Signature after
/// the superclass, which used to cost the whole signature — the type parameters
/// and the fields typed by them.
const IFACE_CONTROLLER: &str = r#"
package com.acme.item;

@RestController
@RequestMapping("/api/items")
public class ItemController {
    @GetMapping("/jailed")
    public JailedEnvelope<ItemInfoDTO> jailed() {
        return null;
    }
}
"#;

fn interface_document() -> Value {
    let host = MockHost::install().with_capabilities(&[
        "project.read",
        "storage",
        "fs.read:/repo",
        "fs.write:/repo",
    ]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.add_file(&format!("{ROOT}/pom.xml"), PROJECT_POM);
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/ItemController.java"), IFACE_CONTROLLER);
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/dto/ItemInfoDTO.java"), ITEM_DTO);
    host.add_external_file(&format!("{REPO}/com/example/api/1.0/api-1.0.pom"), API_POM);
    host.add_external_file(&format!("{REPO}/com/example/common/1.0/common-1.0.pom"), COMMON_POM);
    host::fs::write_bytes(&format!("{REPO}/com/example/common/1.0/common-1.0.jar"), &jar()).expect("seed the jar");

    let paths = [
        format!("{ROOT}/src/main/java/com/acme/item/ItemController.java"),
        format!("{ROOT}/src/main/java/com/acme/item/dto/ItemInfoDTO.java"),
    ];
    host.on_search(move |_query, _limit| {
        Ok(TextSearch {
            matches: paths
                .iter()
                .map(|path| TextMatch {
                    location: Location {
                        path: path.clone(),
                        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    },
                    line: String::new(),
                })
                .collect(),
            limit_hit: false,
        })
    });

    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &options()).expect("a scan");
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.method_name == "jailed")
        .expect("the endpoint");
    yapi::api_doc(endpoint, &scanned.dto, 3, openidea_apimocktle::i18n::Lang::En)
}

#[test]
fn a_wrapper_that_implements_an_interface_keeps_its_payload() {
    let doc = interface_document();
    let res: Value = serde_json::from_str(doc["res_body"].as_str().expect("a schema string")).expect("json");
    let properties = res["properties"].as_object().expect("wrapper properties");

    // Inherited from BaseReply, which still has to be read.
    assert_eq!(properties["code"]["type"], "number", "the generic superclass is kept: {res}");
    assert_eq!(properties["message"]["type"], "string", "{res}");
    // `T` substituted with the endpoint's argument: the payload fields are the
    // whole point of having the signature at all.
    assert_eq!(
        properties["payload"]["properties"]["itemId"]["type"], "number",
        "the payload keeps its fields: {res}"
    );
    assert_eq!(properties["payload"]["properties"]["itemName"]["type"], "string", "{res}");
}

// ---------------------------------------------------------------- build.read ---
//
// API 1.10 lets the plugin ask the IDE for the dependency graph it already
// resolved. That replaces walking every POM — up to `MAX_POMS` host reads — and
// takes the conflict resolution from the side that actually did it. These tests
// pin the three outcomes: the graph is used when granted, a losing conflict is
// not turned into a jar, and the POM walk still answers when it is not.

use openidea_plugin::{DependencyNode, DependencyStatus};

/// The graph the IDE would return: a module root, the `api` jar, and the
/// transitive `common` jar that actually declares `ApiEnvelope`.
fn graph() -> Vec<DependencyNode> {
    vec![
        DependencyNode {
            group: "com.acme".into(),
            artifact: "item-service".into(),
            version: "1.0".into(),
            scope: String::new(),
            status: DependencyStatus::Resolved,
            requested_version: None,
            parent: None,
        },
        DependencyNode {
            group: "com.example".into(),
            artifact: "api".into(),
            version: "1.0".into(),
            scope: "compile".into(),
            status: DependencyStatus::Resolved,
            requested_version: None,
            parent: Some(0),
        },
        DependencyNode {
            group: "com.example".into(),
            artifact: "common".into(),
            version: "1.0".into(),
            scope: "compile".into(),
            status: DependencyStatus::Resolved,
            requested_version: None,
            parent: Some(1),
        },
    ]
}

/// A project whose POM is broken on purpose: if the graph is used, nothing
/// reads it. That makes "did it walk the POMs?" observable instead of inferred.
const UNREADABLE_POM: &str = "<project><this is not a pom";

fn graph_host(caps: &[&str], nodes: Option<Vec<DependencyNode>>) -> MockHost {
    graph_host_with_pom(caps, nodes, UNREADABLE_POM)
}

fn graph_host_with_pom(caps: &[&str], nodes: Option<Vec<DependencyNode>>, pom: &str) -> MockHost {
    let host = MockHost::install().with_capabilities(caps);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.add_file(&format!("{ROOT}/pom.xml"), pom);
    // The repository POM chain the fallback path walks. It is seeded even when
    // the graph answers, because a fallback that never gets exercised is a
    // fallback nobody knows is broken.
    host.add_external_file(&format!("{REPO}/com/example/api/1.0/api-1.0.pom"), API_POM);
    host.add_external_file(&format!("{REPO}/com/example/common/1.0/common-1.0.pom"), COMMON_POM);
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/ItemController.java"), CONTROLLER);
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/dto/ItemInfoDTO.java"), ITEM_DTO);
    // The mock infers repository directories from the files it holds, so the
    // jar's own directory has to exist before `fs.write` can land in it.
    host.add_external_file(&format!("{REPO}/com/example/common/1.0/README"), "");
    host::fs::write_bytes(&format!("{REPO}/com/example/common/1.0/common-1.0.jar"), &jar()).expect("seed the jar");
    if let Some(nodes) = nodes {
        host.set_build_dependencies(nodes);
    }
    let paths = [
        format!("{ROOT}/src/main/java/com/acme/item/ItemController.java"),
        format!("{ROOT}/src/main/java/com/acme/item/dto/ItemInfoDTO.java"),
    ];
    host.on_search(move |_query, _limit| {
        Ok(TextSearch {
            matches: paths
                .iter()
                .map(|path| TextMatch {
                    location: Location {
                        path: path.clone(),
                        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    },
                    line: String::new(),
                })
                .collect(),
            limit_hit: false,
        })
    });
    host
}

fn res_body(host: &MockHost) -> Value {
    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &options()).expect("a scan");
    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.method_name == "queryItem")
        .expect("the endpoint");
    let doc = yapi::api_doc(endpoint, &scanned.dto, 3, openidea_apimocktle::i18n::Lang::En);
    serde_json::from_str(doc["res_body"].as_str().expect("a schema string")).expect("json")
}

#[test]
fn the_ide_graph_replaces_the_pom_walk() {
    // The POM in this project is unparseable, so fields can only come from the
    // graph pointing at a jar. If anything still walked the POM, this wrapper
    // would stay unknown.
    let host = graph_host(&["project.read", "storage", "fs.read:/repo", "fs.write:/repo", "build.read"], Some(graph()));
    let res = res_body(&host);

    let properties = res["properties"].as_object().expect("wrapper properties");
    assert_eq!(properties["errorNo"]["type"], "number", "the jar was found through the graph: {res}");
    assert_eq!(properties["result"]["type"], "object", "{res}");
}

#[test]
fn a_losing_conflict_does_not_become_a_jar() {
    // `common` 2.0 lost to 1.0. A jar path built from the loser's version points
    // at an artifact the build never used, so a class found there would be the
    // wrong class — worse than not resolving it at all.
    let mut nodes = graph();
    nodes[2].requested_version = Some("2.0".into());
    nodes[2].version = "1.0".into();
    nodes[2].status = DependencyStatus::OmittedForConflict;

    let host = graph_host(&["project.read", "storage", "fs.read:/repo", "fs.write:/repo", "build.read"], Some(nodes));
    let res = res_body(&host);

    assert!(res["properties"]["errorNo"].is_null(), "the conflicting node must not supply a jar: {res}");
}

#[test]
fn without_the_grant_the_pom_walk_still_answers() {
    // No `build.read`: the capability is optional, so the scan must fall back to
    // reading the POM — the pre-1.10 behaviour — and still resolve the wrapper.
    // The POM here must be a real one, or this would assert that a broken
    // fallback still works.
    let host = graph_host_with_pom(
        &["project.read", "storage", "fs.read:/repo", "fs.write:/repo"],
        Some(graph()),
        PROJECT_POM,
    );
    let res = res_body(&host);

    let properties = res["properties"].as_object().expect("wrapper properties");
    assert_eq!(properties["errorNo"]["type"], "number", "the POM path still resolves: {res}");
}

#[test]
fn an_empty_graph_falls_back_rather_than_dropping_every_jar() {
    // The IDE answered, but with nothing resolved yet (no Reload). Using that
    // answer would mean zero jars and a bare `object` schema, so it has to be
    // treated as "no answer" — and the fallback needs a usable POM to prove it.
    let host = graph_host_with_pom(
        &["project.read", "storage", "fs.read:/repo", "fs.write:/repo", "build.read"],
        Some(Vec::new()),
        PROJECT_POM,
    );
    let res = res_body(&host);

    let properties = res["properties"].as_object().expect("wrapper properties");
    assert_eq!(properties["errorNo"]["type"], "number", "an empty graph is not an answer: {res}");
}
