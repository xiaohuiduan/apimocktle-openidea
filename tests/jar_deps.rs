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
    yapi::api_doc(endpoint, &scanned.dto, 3)
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
    let doc = yapi::api_doc(endpoint, &scanned.dto, 3);
    let res: Value = serde_json::from_str(doc["res_body"].as_str().expect("a schema string")).expect("json");

    // Without the grant the jar is invisible, so the wrapper stays unknown and
    // the 0.2.4 fallback unwraps the single type argument instead.
    assert_eq!(res["description"], "Map", "{res}");
    assert!(res["properties"]["errorNo"].is_null(), "{res}");
}
