//! The app's texts in its languages. The code writes English (`tr("Copy name")`); each other
//! language is a JSON file in `i18n/` that maps the English text to its own. A text a language
//! lacks shows in English. Counts are worded so one form fits every number ("Waiting: 3").

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;

/// The languages, as Settings lists them after "System": English (the source), then by their own
/// names, Latin script before Cyrillic.
pub const LANGUAGES: [(&str, &str); 8] = [
    ("en", "English"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("sr", "Srpski"),
    ("ru", "Русский"),
];

fn source(lang: &str) -> &'static str {
    match lang {
        "de" => include_str!("../i18n/de.json"),
        "es" => include_str!("../i18n/es.json"),
        "fr" => include_str!("../i18n/fr.json"),
        "it" => include_str!("../i18n/it.json"),
        "pt" => include_str!("../i18n/pt.json"),
        "sr" => include_str!("../i18n/sr.json"),
        "ru" => include_str!("../i18n/ru.json"),
        _ => "{}",
    }
}

thread_local! {
    static CURRENT: Cell<&'static str> = const { Cell::new("en") };
}

/// A language's texts, read once and kept for the app's life (at most eight small tables).
fn table(lang: &'static str) -> &'static HashMap<String, &'static str> {
    static TABLES: OnceLock<std::sync::Mutex<HashMap<&'static str, &'static HashMap<String, &'static str>>>> = OnceLock::new();
    let mut tables = TABLES.get_or_init(Default::default).lock().unwrap();
    tables.entry(lang).or_insert_with(|| {
        let map: HashMap<String, String> = serde_json::from_str(source(lang)).unwrap_or_default();
        Box::leak(Box::new(map.into_iter().map(|(k, v)| (k, &*Box::leak(v.into_boxed_str()))).collect()))
    })
}

/// A language code of `LANGUAGES`, else English.
fn known(lang: &str) -> &'static str {
    LANGUAGES.iter().find(|(code, _)| *code == lang).map_or("en", |(code, _)| code)
}

/// The language the next build draws with; empty = the system's.
pub fn set(lang: &str) {
    let lang = if lang.is_empty() { system() } else { known(lang) };
    CURRENT.with(|c| c.set(lang));
}

/// The system's language when the app has it, else English.
pub fn system() -> &'static str {
    known(sys_locale::get_locale().as_deref().unwrap_or("en").split(['-', '_']).next().unwrap_or("en"))
}

/// A language's own name ("Deutsch").
pub fn native_name(lang: &str) -> &'static str {
    LANGUAGES.iter().find(|(code, _)| *code == lang).map_or("English", |(_, name)| name)
}

/// `english` in the current language.
pub fn tr(english: &'static str) -> &'static str {
    let lang = CURRENT.with(Cell::get);
    if lang == "en" {
        return english;
    }
    table(lang).get(english).copied().unwrap_or(english)
}

/// `english` in the current language with its `{name}` values filled in.
pub fn trf(english: &'static str, values: &[(&str, &str)]) -> String {
    values.iter().fold(tr(english).to_string(), |text, (name, value)| text.replace(&format!("{{{name}}}"), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every text the code passes to `tr` / `trf` is in every language, with the same `{values}`,
    /// and no language has texts the code no longer uses.
    #[test]
    fn every_language_has_every_text() {
        let code = [include_str!("lib.rs"), include_str!("state.rs")].concat();
        let mut used: Vec<String> = Vec::new();
        for start in ["tr(\"", "trf(\""] {
            let mut rest = code.as_str();
            while let Some(i) = rest.find(start) {
                rest = &rest[i + start.len()..];
                let end = rest.find('"').unwrap();
                used.push(rest[..end].replace("\\\"", "\""));
                rest = &rest[end..];
            }
        }
        used.sort();
        used.dedup();
        let values = |t: &str| {
            let mut v: Vec<String> = t.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(n, _)| n.to_string())).collect();
            v.sort();
            v
        };
        for (lang, _) in LANGUAGES.iter().skip(1) {
            let map: HashMap<String, String> = serde_json::from_str(source(lang)).unwrap_or_else(|e| panic!("{lang}.json: {e}"));
            for text in &used {
                let Some(translated) = map.get(text) else { panic!("{lang}.json lacks {text:?}") };
                assert_eq!(values(text), values(translated), "{lang}: the values of {text:?}");
            }
            for key in map.keys() {
                assert!(used.contains(key), "{lang}.json has {key:?}, which the code does not use");
            }
        }
    }

    #[test]
    fn fills_values() {
        assert_eq!(trf("Answers of {name}", &[("name", "api")]), "Answers of api");
    }
}
