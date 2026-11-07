//! Reading the locale files in tests: the texts of one `[section]`.

use std::collections::HashMap;

/// The two locale files, by language.
pub const LOCALES: [(&str, &str); 2] = [
    ("en", include_str!("../../locales/en.toml")),
    ("nb", include_str!("../../locales/nb.toml")),
];

/// The texts of one `[section]` of a locale file.
pub fn section(file: &str, wanted: &str) -> HashMap<String, String> {
    let mut current = "";
    let mut texts = HashMap::new();
    for line in file.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
        } else if current == wanted
            && let Some((key, value)) = line.split_once(" = ")
        {
            texts.insert(key.to_string(), value.trim_matches('"').to_string());
        }
    }
    texts
}

/// The text called `name` in `[section]`, as written in the file.
pub fn text(file: &str, section_name: &str, name: &str) -> Option<String> {
    section(file, section_name).remove(name)
}

/// Whether `[section]` has a non-empty text called `name`.
pub fn has_text(file: &str, section_name: &str, name: &str) -> bool {
    text(file, section_name, name).is_some_and(|t| !t.is_empty())
}

/// Whether the dotted `key` (`need.vv_down`) has a text in both languages.
pub fn in_both(key: &str) -> Result<(), String> {
    let (section_name, name) = key.split_once('.').ok_or_else(|| key.to_string())?;
    for (lang, file) in LOCALES {
        if !has_text(file, section_name, name) {
            return Err(format!("{lang}: {key}"));
        }
    }
    Ok(())
}
