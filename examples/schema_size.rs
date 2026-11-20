//! Measures the JSON Schema size ApiMocktle stores per interface at different
//! DTO expansion depths.
//!
//! ```sh
//! cargo run --example schema_size -- C:\path\to\project
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use openidea_apimocktle::model::{Endpoint, FieldSpec};
use openidea_apimocktle::scan::DtoIndex;
use openidea_apimocktle::{spring, yapi};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(name.as_str(), "target" | "build" | "node_modules" | ".git" | "out" | ".idea") {
                continue;
            }
            collect(&path, out);
        } else if name.ends_with(".java") || name.ends_with(".kt") {
            out.push(path);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".to_owned()));
    let mut files = Vec::new();
    collect(&root, &mut files);

    let mut index: HashMap<String, (Vec<String>, Vec<spring::Field>)> = HashMap::new();
    let mut endpoints: Vec<Endpoint> = Vec::new();
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else { continue };
        let parsed = spring::parse(&src);
        for class in &parsed.classes {
            index
                .entry(class.name.clone())
                .or_insert_with(|| (class.type_params.clone(), class.fields.clone()));
        }
        for raw in spring::endpoints(&parsed, &src) {
            endpoints.push(Endpoint {
                method: raw.http_method,
                path: raw.path,
                framework: raw.framework.to_owned(),
                class_name: raw.class_name,
                method_name: raw.method_name,
                summary: raw.summary,
                description: raw.description,
                body_type: raw.body_type,
                return_type: raw.return_type,
                ..Endpoint::default()
            });
        }
    }

    let mut dto = DtoIndex::default();
    for (name, (params, fields)) in &index {
        let specs = fields
            .iter()
            .map(|f| FieldSpec {
                name: f.name.clone(),
                ty: f.ty.clone(),
                description: String::new(),
                children: Vec::new(),
            })
            .collect();
        dto.params.insert(name.clone(), params.clone());
        dto.fields.insert(name.clone(), specs);
    }

    println!("{} 个源文件，{} 个接口，{} 个可解析类型\n", files.len(), endpoints.len(), dto.fields.len());
    for depth in 1..=4u32 {
        let mut total = 0usize;
        let mut biggest = (0usize, String::new());
        for endpoint in &endpoints {
            let doc = yapi::api_doc(endpoint, &dto, depth, openidea_apimocktle::i18n::Lang::En);
            let bytes = doc["req_body_other"].as_str().map_or(0, str::len)
                + doc["res_body"].as_str().map_or(0, str::len);
            total += bytes;
            if bytes > biggest.0 {
                biggest = (bytes, format!("{} {}", endpoint.yapi_method(), endpoint.path));
            }
        }
        println!(
            "深度 {depth}: 合计 {:>7} KiB，最大单条 {:>6} B（{}）",
            total / 1024,
            biggest.0,
            biggest.1
        );
    }
}
