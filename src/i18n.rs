//! Переводы интерфейса. Ключ — исходная русская строка, значение — перевод.
//! Русский и английский вшиты в программу; файлы `<код>.json` в папке языков можно править
//! и добавлять свои.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde_json::{Map, Value};

const EMBEDDED: &[(&str, &str)] = &[
    ("ru", include_str!("../lang/ru.json")),
    ("en", include_str!("../lang/en.json")),
];

struct Active {
    code: String,
    strings: HashMap<String, &'static str>,
}

static ACTIVE: RwLock<Option<Active>> = RwLock::new(None);
static ENGLISH: OnceLock<HashMap<String, &'static str>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageInfo {
    pub code: String,
    pub name: String,
}

/// Text in the active language. Strings missing from a translation fall back to English,
/// then to the Russian original.
pub fn tr(key: &'static str) -> &'static str {
    let guard = ACTIVE.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(active) = guard.as_ref() else {
        return key;
    };
    if let Some(text) = active.strings.get(key) {
        return text;
    }
    if active.code == "ru" {
        return key;
    }
    english().get(key).copied().unwrap_or(key)
}

/// Replaces `{name}` in a translated template; `{{` and `}}` stand for literal braces.
pub fn fill(template: &str, args: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(index) = rest.find(['{', '}']) {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if tail.starts_with('{') {
            if let Some(close) = tail.find('}') {
                let name = &tail[1..close];
                if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
                    out.push_str(value);
                    rest = &tail[close + 1..];
                    continue;
                }
            }
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// `trf!("Записей: {count}", count = files.len())`
#[macro_export]
macro_rules! trf {
    ($key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::fill(
            $crate::i18n::tr($key),
            &[$((stringify!($name), ($value).to_string())),*],
        )
    };
}

fn english() -> &'static HashMap<String, &'static str> {
    ENGLISH.get_or_init(|| {
        let text = embedded("en").unwrap_or("{}");
        strings_of(&parse(text).unwrap_or(Value::Null))
    })
}

/// Windows Notepad saves JSON with a byte order mark, which `serde_json` refuses.
fn parse(text: &str) -> serde_json::Result<Value> {
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
}

fn embedded(code: &str) -> Option<&'static str> {
    EMBEDDED
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, text)| *text)
}

fn strings_of(file: &Value) -> HashMap<String, &'static str> {
    file.get("strings")
        .and_then(Value::as_object)
        .map(|strings| {
            strings
                .iter()
                .filter_map(|(key, value)| {
                    let text = value.as_str()?;
                    (!text.is_empty()).then(|| (key.clone(), &*Box::leak(text.to_string().into_boxed_str())))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn language_path(dir: &Path, code: &str) -> PathBuf {
    dir.join(format!("{code}.json"))
}

/// Writes the built-in languages on first start; later adds strings a new version brought,
/// keeping everything the user already changed.
pub fn ensure_files(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (code, text) in EMBEDDED {
        let path = language_path(dir, code);
        let Ok(current) = std::fs::read_to_string(&path) else {
            std::fs::write(&path, text)?;
            continue;
        };
        let (Ok(mut file), Ok(bundled)) = (
            parse(&current),
            parse(text),
        ) else {
            continue;
        };
        let Some(bundled) = bundled.get("strings").and_then(Value::as_object).cloned() else {
            continue;
        };
        let Some(root) = file.as_object_mut() else {
            continue;
        };
        let strings = root
            .entry("strings")
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(strings) = strings.as_object_mut() else {
            continue;
        };
        let mut added = false;
        for (key, value) in bundled {
            if !strings.contains_key(&key) {
                strings.insert(key, value);
                added = true;
            }
        }
        if added {
            if let Ok(pretty) = serde_json::to_string_pretty(&file) {
                std::fs::write(&path, pretty + "\n")?;
            }
        }
    }
    Ok(())
}

/// Every `*.json` in `dir` that looks like a language file, plus the built-in ones.
pub fn available(dir: &Path) -> Vec<LanguageInfo> {
    let mut found: Vec<LanguageInfo> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(code) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = parse(&text) else {
                continue;
            };
            if file.get("strings").and_then(Value::as_object).is_none() {
                continue;
            }
            let name = file
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(code)
                .to_string();
            found.push(LanguageInfo {
                code: code.to_string(),
                name,
            });
        }
    }
    for (code, text) in EMBEDDED {
        if found.iter().any(|item| item.code == *code) {
            continue;
        }
        let name = parse(text)
            .ok()
            .and_then(|file| file.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| code.to_string());
        found.push(LanguageInfo {
            code: code.to_string(),
            name,
        });
    }
    found.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    found
}

/// Switches the interface to `code`, read from `dir` or, for built-in languages, from the program.
pub fn activate(dir: &Path, code: &str) -> Result<(), String> {
    let path = language_path(dir, code);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => embedded(code)
            .ok_or_else(|| format!("нет файла {}", path.display()))?
            .to_string(),
    };
    let file: Value = parse(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    let strings = strings_of(&file);
    *ACTIVE.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Active {
        code: code.to_string(),
        strings,
    });
    Ok(())
}

/// `ru` for Russian-speaking systems (and CIS languages where Russian is common), `en` otherwise.
pub fn system_language() -> &'static str {
    const RUSSIAN_LIKE: &[&str] = &["ru", "be", "kk", "ky", "uz", "tg"];
    let code = system_code();
    if RUSSIAN_LIKE.contains(&code.as_str()) {
        "ru"
    } else {
        "en"
    }
}

#[cfg(windows)]
fn system_code() -> String {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    let primary = unsafe { GetUserDefaultUILanguage() } & 0x3ff;
    match primary {
        0x19 => "ru",
        0x23 => "be",
        0x3f => "kk",
        0x40 => "ky",
        0x43 => "uz",
        0x28 => "tg",
        _ => "en",
    }
    .to_string()
}

#[cfg(not(windows))]
fn system_code() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty() && value != "C" && value != "POSIX")
        .map(|value| value.chars().take_while(|ch| ch.is_ascii_alphabetic()).collect::<String>().to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_names_and_keeps_escaped_braces() {
        let args = [("count", "3".to_string()), ("name", "Двор".to_string())];
        assert_eq!(fill("{name}: {count} {{x}} {missing}", &args), "Двор: 3 {x} {missing}");
    }

    #[test]
    fn built_in_files_parse_and_english_covers_russian() {
        let ru: Value = parse(embedded("ru").unwrap()).unwrap();
        let en: Value = parse(embedded("en").unwrap()).unwrap();
        let ru = ru["strings"].as_object().unwrap();
        let en = en["strings"].as_object().unwrap();
        let missing: Vec<&String> = ru.keys().filter(|key| !en.contains_key(*key)).collect();
        assert!(missing.is_empty(), "нет перевода: {missing:?}");
        for (key, value) in en {
            let wanted: Vec<&str> = placeholders(key);
            let got: Vec<&str> = placeholders(value.as_str().unwrap());
            assert_eq!(wanted, got, "подстановки не совпадают: {key}");
        }
    }

    #[test]
    fn every_string_in_the_code_is_in_the_language_files() {
        let ru: Value = parse(embedded("ru").unwrap()).unwrap();
        let ru = ru["strings"].as_object().unwrap();
        let en: Value = parse(embedded("en").unwrap()).unwrap();
        let extra: Vec<&String> = en["strings"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| !ru.contains_key(*key))
            .collect();
        assert!(extra.is_empty(), "в en.json лишние строки: {extra:?}");
        let sources = [
            include_str!("bin/xm-view.rs"),
            include_str!("bin/config.rs"),
            include_str!("error.rs"),
            include_str!("hikvision.rs"),
            include_str!("session.rs"),
            include_str!("packet.rs"),
            include_str!("frame.rs"),
        ];
        let mut missing = Vec::new();
        for source in sources {
            let code = source.split("#[cfg(test)]").next().unwrap();
            for key in literal_keys(code) {
                if !ru.contains_key(&key) {
                    missing.push(key);
                }
            }
        }
        assert!(missing.is_empty(), "добавьте в lang/ru.json и lang/en.json: {missing:?}");
    }

    #[test]
    fn external_language_files_match_the_program() {
        let ru: Value = parse(embedded("ru").unwrap()).unwrap();
        let ru = ru["strings"].as_object().unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("lang");
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let code = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default().to_string();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") || embedded(&code).is_some() {
                continue;
            }
            let file = parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            assert!(
                file["name"].as_str().is_some_and(|name| !name.trim().is_empty()),
                "{code}: нет названия языка"
            );
            let strings = file["strings"].as_object().unwrap_or_else(|| panic!("{code}: нет strings"));
            let unknown: Vec<&String> = strings.keys().filter(|key| !ru.contains_key(*key)).collect();
            assert!(unknown.is_empty(), "{code}: таких строк нет в программе: {unknown:?}");
            for (key, value) in strings {
                let text = value.as_str().unwrap_or_else(|| panic!("{code}: не строка: {key}"));
                assert_eq!(placeholders(key), placeholders(text), "{code}: подстановки не совпадают: {key}");
            }
        }
    }

    fn literal_keys(code: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for marker in ["tr(\"", "trf!(\""] {
            let mut rest = code;
            while let Some(index) = rest.find(marker) {
                let before = rest[..index].chars().next_back();
                rest = &rest[index + marker.len()..];
                if before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
                    continue;
                }
                let mut key = String::new();
                let mut chars = rest.chars();
                while let Some(ch) = chars.next() {
                    match ch {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => key.push('\n'),
                            Some(other) => key.push(other),
                            None => break,
                        },
                        other => key.push(other),
                    }
                }
                keys.push(key);
            }
        }
        keys
    }

    fn placeholders(text: &str) -> Vec<&str> {
        let mut names: Vec<&str> = text
            .split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .filter(|name| !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
            .collect();
        names.sort_unstable();
        names
    }
}
