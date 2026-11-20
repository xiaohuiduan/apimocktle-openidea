//! The package's translation catalog, checked the way the host reads it.
//!
//! The host translates a package's declaration text from `locales/<tag>.json`
//! (English source text → translation) and picks the catalog by language tag:
//! the exact tag first, then the language, then a variant of that language.
//! These tests replay that rule over the catalog this package ships, so a
//! missing or unreachable catalog fails here rather than in a Chinese IDE that
//! suddenly shows English.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The IDE's interface languages and the BCP 47 tag it reports for each
/// (`src/platform/i18n.ts`, `LOCALES`).
const IDE_TAGS: &[(&str, &str)] = &[("en", "en"), ("zh", "zh-CN"), ("ko", "ko"), ("fr", "fr")];

fn locales_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("locales")
}

/// Every `<tag>.json` the package ships, as (tag, catalog).
fn catalogs() -> Vec<(String, BTreeMap<String, String>)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(locales_dir()) else { return found };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let tag = path.file_stem().and_then(|s| s.to_str()).expect("a file name").to_owned();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let catalog: BTreeMap<String, String> = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{}: not a JSON object of strings ({e})", path.display()));
        found.push((tag, catalog));
    }
    found
}

/// `pickCatalogTag` from `src/features/plugins/pluginText.ts`.
fn pick_catalog_tag<'a>(tags: &'a [String], locale: &str) -> Option<&'a String> {
    let full = IDE_TAGS
        .iter()
        .find(|(key, _)| *key == locale)
        .map(|(_, tag)| (*tag).to_owned())
        .unwrap_or_else(|| locale.to_owned())
        .to_lowercase();
    let language = full.split('-').next().unwrap_or_default();
    let lower: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
    let exact = lower.iter().position(|t| t == &full);
    let at = exact.or_else(|| lower.iter().position(|t| t == language));
    if let Some(index) = at {
        return Some(&tags[index]);
    }
    tags.iter().find(|t| t.to_lowercase().split('-').next() == Some(language))
}

#[test]
fn a_chinese_ide_finds_the_catalog_and_an_english_one_keeps_the_source_text() {
    let tags: Vec<String> = catalogs().into_iter().map(|(tag, _)| tag).collect();
    assert!(!tags.is_empty(), "the package ships at least one catalog: {}", locales_dir().display());

    // A Chinese interface — however the tag is spelled — lands on the catalog.
    for tag in ["zh", "zh-CN", "zh-TW"] {
        assert_eq!(pick_catalog_tag(&tags, tag).map(String::as_str), Some("zh-CN"), "{tag}");
    }
    // English and languages without a catalog keep the English source text: the
    // host translates nothing and the text reads as written.
    for tag in ["en", "fr", "ko"] {
        assert_eq!(pick_catalog_tag(&tags, tag), None, "{tag}");
    }
}

#[test]
fn the_catalog_filename_is_a_language_tag_the_host_accepts() {
    // `ui_points.rs::locale_tag_ok`: a 2–3 letter language, then 1–8 character
    // alphanumeric subtags. A malformed name refuses the whole install.
    for (tag, _) in catalogs() {
        let mut parts = tag.split('-');
        let language = parts.next().unwrap_or_default();
        assert!(
            (2..=3).contains(&language.len()) && language.chars().all(|c| c.is_ascii_alphabetic()),
            "{tag}: language subtag"
        );
        for part in parts {
            assert!(
                (1..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric()),
                "{tag}: subtag {part:?}"
            );
        }
    }
}

/// The quoted `title` / `category` values of the manifest, in file order.
///
/// Parsed by line instead of with a TOML crate: the manifest's contributed text
/// is all single-line strings, and a dev-dependency for three assertions is not
/// worth what it would add to the build.
fn manifest_text_values(source: &str) -> Vec<String> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let (key, rest) = line.split_once('=')?;
            matches!(key.trim(), "title" | "category")
                .then(|| rest.trim().strip_prefix('"')?.strip_suffix('"').map(str::to_owned))
                .flatten()
        })
        .collect()
}

#[test]
fn the_manifest_and_the_settings_are_translated_in_chinese() {
    let catalog = catalogs()
        .into_iter()
        .find(|(tag, _)| tag == "zh-CN")
        .expect("a Chinese catalog")
        .1;
    let translate = |text: &str| catalog.get(text).cloned().unwrap_or_else(|| text.to_owned());
    let chinese = |text: &str| text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Command titles and categories reach the user through the host's catalog.
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).expect("a manifest");
    let values = manifest_text_values(&manifest);
    assert!(values.len() >= 6, "commands and their category: {values:?}");
    for source in &values {
        // A product name is its own translation and needs no catalog entry.
        if source == "ApiMocktle" {
            continue;
        }
        let shown = translate(source);
        assert!(chinese(&shown), "the Chinese interface shows {source:?} in Chinese, got {shown:?}");
    }

    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("settings.schema.json")).unwrap())
            .expect("a settings schema");
    for (key, property) in schema["properties"].as_object().expect("properties") {
        for field in ["title", "description"] {
            let Some(source) = property.get(field).and_then(serde_json::Value::as_str) else { continue };
            let shown = translate(source);
            assert!(
                chinese(&shown),
                "{key}.{field}: the Chinese interface shows this in Chinese, got {shown:?}"
            );
        }
    }
}

/// `description` is the one user-visible string the host renders verbatim: it is
/// not in the host's translatable keys (`pluginText.ts`), and the details page
/// draws it as a single `<p>` with no `pre-wrap`, so HTML collapses every newline
/// into a space — the manifest cannot produce a line break. Both languages are
/// therefore separated by a visible marker in one paragraph, English first because
/// the marketplace list row truncates to one line.
#[test]
fn the_overview_carries_both_languages_in_one_paragraph() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).expect("a manifest");
    let description = manifest
        .lines()
        .find_map(|line| line.strip_prefix("description = "))
        .expect("a description line")
        .trim()
        .trim_matches('"');

    assert!(!description.contains('\n'), "it is rendered as one <p>");
    // A newline would collapse to a space anyway, so the languages are split by a
    // marker the user can see.
    let (english, chinese) = description
        .split_once('▍')
        .expect("the Chinese half is introduced by a visible marker");
    assert!(
        english.len() > 200 && chinese.len() > 200,
        "both halves are substantive: {} / {}",
        english.len(),
        chinese.len()
    );
    assert!(
        !english.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "English leads: the list row truncates to its first ~40 characters"
    );
    let chinese_body = chinese.strip_prefix("中文：").expect("the marker names the language");
    assert!(
        chinese_body.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "the Chinese half is Chinese"
    );
}
