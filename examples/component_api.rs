//! Prints the plugin API version a built component declares.
//!
//! The host refuses a component whose imports name an interface it does not
//! provide (`Runtime::check_component` → `component_api` → `check_api`), and it
//! cross-checks `engines.openidea` in the manifest against its own
//! `ENGINE_VERSION`. Both sides therefore have to name the same `1.x`, and this
//! example reads the component's side without needing the `openidea-plugin` CLI
//! — the copy installed on this machine is pinned to the SDK it was built from.
//!
//! ```sh
//! cargo run --release --example component_api -- dist/plugin.wasm
//! ```
//!
//! It reads the raw component binary rather than instantiating it, so it works
//! for any `wasm32-wasip2` build regardless of the SDK version in use.

use std::path::PathBuf;

/// `openidea:plugin/<interface>@<version>` in the component-type import names.
const PREFIX: &str = "openidea:plugin/";

fn main() {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: component_api <component.wasm>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("读不到 {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    // Interface names live in the component-type section as plain strings, so a
    // scan of the binary finds them without decoding the type structure.
    let mut versions: Vec<String> = Vec::new();
    let mut interfaces: Vec<String> = Vec::new();
    let text = String::from_utf8_lossy(&bytes);
    for (at, _) in text.match_indices(PREFIX) {
        let start = at + PREFIX.len();
        let rest = &text[start..];
        let Some(end) = rest.find(|c: char| !is_name_char(c)) else { continue };
        let name = &rest[..end];
        if !name.is_empty() && !interfaces.iter().any(|seen| seen == name) {
            interfaces.push(name.to_owned());
        }
    }
    // Versions appear at the end of each `<name>@<version>` import.
    for (at, _) in text.match_indices('@') {
        let rest = &text[at + 1..];
        let version: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if version.matches('.').count() == 2
            && version.starts_with("1.")
            && !versions.iter().any(|seen| seen == &version)
        {
            versions.push(version);
        }
    }

    interfaces.sort();
    versions.sort();
    println!("组件      {}", path.display());
    println!("接口      {}", interfaces.join(", "));
    match versions.first() {
        Some(version) if versions.len() == 1 => println!("API 版本  {version}"),
        Some(_) => println!("API 版本  {:?}（多个，组件内接口版本不一致）", versions),
        None => {
            println!("API 版本  未找到 openidea:plugin 接口");
            std::process::exit(1);
        }
    }
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/'
}
