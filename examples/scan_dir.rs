//! Scans a directory of Java/Kotlin sources and prints the routes the plugin
//! would publish. Useful for checking the parser against a real project:
//!
//! ```sh
//! cargo run --example scan_dir -- C:\path\to\project
//! ```

use std::path::{Path, PathBuf};

use openidea_apimocktle::{spring, spring::ParamKind};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "build" | "node_modules" | ".git" | "out" | ".idea"
            ) {
                continue;
            }
            collect(&path, out);
        } else if name.ends_with(".java") || name.ends_with(".kt") {
            out.push(path);
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    let root = PathBuf::from(root);
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();

    let mut total = 0usize;
    let mut with_body = 0usize;
    let mut frameworks = std::collections::BTreeMap::new();
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else { continue };
        let model = spring::parse(&src);
        let endpoints = spring::endpoints(&model, &src);
        if endpoints.is_empty() {
            continue;
        }
        let rel = file.strip_prefix(&root).unwrap_or(file);
        println!("\n== {}", rel.display());
        for endpoint in &endpoints {
            total += 1;
            if endpoint.body_type.is_some() {
                with_body += 1;
            }
            *frameworks.entry(endpoint.framework).or_insert(0usize) += 1;
            let params: Vec<String> = endpoint
                .params
                .iter()
                .map(|p| {
                    let kind = match p.kind {
                        ParamKind::Path => "path",
                        ParamKind::Query => "query",
                        ParamKind::Header => "header",
                        ParamKind::Body => "body",
                        ParamKind::Cookie => "cookie",
                        ParamKind::Form => "form",
                    };
                    format!("{}:{}", p.name, kind)
                })
                .collect();
            println!(
                "  {:6} {:<48} {}#{}  ({})",
                endpoint.http_method.clone().unwrap_or_else(|| "ANY".into()),
                endpoint.path,
                endpoint.class_name,
                endpoint.method_name,
                params.join(", ")
            );
            if !endpoint.return_type.is_empty() {
                println!("         -> {}", endpoint.return_type);
            }
        }
    }
    println!("\n扫描 {} 个源文件，得到 {total} 个接口（{with_body} 个带请求体）", files.len());
    println!("框架分布：{frameworks:?}");
}
