//! `ApiEnvelope<Map<String, List<ItemInfoDTO>>>` where `ApiEnvelope`
//! lives in a dependency jar: its source is not in the project, so the DTO
//! graph has no fields for it. The response schema must still describe the
//! payload instead of collapsing to a bare `{"type": "object"}` (which
//! ApiMocktle renders as an empty field table).

use openidea_apimocktle::{scan, yapi};
use openidea_plugin::mock::MockHost;
use openidea_plugin::{Location, Position, Range, TextMatch, TextSearch};
use serde_json::Value;

const ROOT: &str = "/proj";

const CONTROLLER: &str = r#"
package com.acme.item;

import java.util.List;
import java.util.Map;

@RestController
@RequestMapping("/api/items")
public class ItemController {
    /**
     * 新人券信息
     */
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
    /** 券 id */
    private Long itemId;
    /** 券名称 */
    private String itemName;
    private Integer status;
}
"#;

/// The controller plus a DTO, with `ApiEnvelope` and the request DTO missing
/// on purpose: exactly the shape of a project that pulls them from a jar.
fn host() -> MockHost {
    let host = MockHost::install().with_capabilities(&["project.read"]);
    host.add_project(ROOT);
    host.set_project_root(Some(ROOT));
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/ItemController.java"), CONTROLLER);
    host.add_file(&format!("{ROOT}/src/main/java/com/acme/item/dto/ItemInfoDTO.java"), ITEM_DTO);

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

#[test]
fn a_wrapper_from_a_jar_still_describes_its_argument() {
    let host = host();
    let project = host.invocation().project().expect("a project");
    let scanned = scan::scan(&project, &scan::Options { depth: 3, ..scan::Options::default() }).expect("a scan");

    let endpoint = scanned
        .endpoints
        .iter()
        .find(|e| e.method_name == "queryItem")
        .expect("the endpoint");
    assert_eq!(endpoint.return_type, "ApiEnvelope<Map<String, List<ItemInfoDTO>>>");

    let doc = yapi::api_doc(endpoint, &scanned.dto, 3);
    let res: Value = serde_json::from_str(doc["res_body"].as_str().expect("a schema string")).expect("json");

    // `ApiEnvelope` has no source here, so the only thing left to describe is
    // the type argument: `Map<String, List<ItemInfoDTO>>`.
    let properties = res["properties"].as_object().expect("a map is rendered with a sample key");
    let sample = properties
        .values()
        .next()
        .expect("the sample key carries the value schema");
    assert_eq!(sample["type"], "array", "the map value is a list: {res}");
    let item = &sample["items"]["properties"];
    assert_eq!(item["itemId"]["type"], "number", "{res}");
    assert_eq!(item["itemName"]["type"], "string", "{res}");
    assert_eq!(item["status"]["type"], "number", "{res}");
}
