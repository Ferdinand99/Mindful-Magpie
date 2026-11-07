//! What the integration kinds that read an app's API (`vv`, `kk`, `ww`) have
//! in common: the form, the texts in both languages and the shape of the
//! labels of their needs.

use std::collections::HashMap;

use mindful_magpie::integrations::{self, Integration};

const KINDS: [&str; 3] = ["vv", "kk", "ww"];
/// Every kind the build lists: the three above share one form, the others
/// have their own (and their own tests).
const ALL: [&str; 7] = ["http", "vv", "kk", "ww", "proxmox", "unraid", "tofa"];

/// The texts of one `[section]` of a locale file.
fn section(file: &str, wanted: &str) -> HashMap<String, String> {
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

fn locales() -> [(&'static str, &'static str); 2] {
    [
        ("en", include_str!("../locales/en.toml")),
        ("nb", include_str!("../locales/nb.toml")),
    ]
}

fn kind(name: &str) -> &'static dyn Integration {
    integrations::find(name).unwrap_or_else(|| panic!("{name} is registered"))
}

#[test]
fn every_kind_is_registered_once() {
    let names: Vec<_> = integrations::registry().iter().map(|k| k.kind()).collect();
    for name in KINDS {
        assert_eq!(names.iter().filter(|n| **n == name).count(), 1, "{name}");
    }
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len());
}

#[test]
fn the_build_lists_every_kind_once() {
    let mut names: Vec<_> = integrations::registry().iter().map(|k| k.kind()).collect();
    names.sort_unstable();
    let mut all = ALL.to_vec();
    all.sort_unstable();
    assert_eq!(names, all);
    for name in ALL {
        assert!(integrations::find(name).is_some(), "{name}");
    }
}

#[test]
fn the_forms_are_alike() {
    for name in KINDS {
        let fields = kind(name).fields();
        assert_eq!(fields.len(), 1, "{name}");
        let field = &fields[0];
        assert_eq!(field.key, "secret", "{name}");
        assert!(field.secret && field.required, "{name}");
        assert_eq!(field.label_key, format!("field.{name}_secret"));
        assert_eq!(field.help_key, format!("field.{name}_secret_help"));
        assert_eq!(field.placeholder, format!("{name}_…"));
        assert!(kind(name).url_required(), "{name}");
    }
}

#[test]
fn the_form_texts_say_the_same_in_every_kind() {
    for (lang, file) in locales() {
        let fields = section(file, "field");
        let (label, scope, path) = match lang {
            "en" => ("Read token", "\\\"Read only\\\"", "Settings → API tokens"),
            _ => ("Lesetoken", "«Bare lesing»", "Innstillinger → API-tokener"),
        };
        for name in KINDS {
            assert_eq!(fields[&format!("{name}_secret")], label, "{lang} {name}");
            let help = &fields[&format!("{name}_secret_help")];
            assert!(help.contains(scope), "{lang} {name}: {help}");
            assert!(help.contains(path), "{lang} {name}: {help}");
            assert!(help.contains("Authorization: Bearer"), "{lang} {name}");
        }
    }
}

#[test]
fn every_metric_and_kind_label_has_a_text_in_both_languages() {
    for (lang, file) in locales() {
        let (kinds, metrics) = (section(file, "kind"), section(file, "metric"));
        for name in KINDS {
            let k = kind(name);
            assert!(!kinds[name].is_empty(), "{lang} {name}");
            assert_eq!(k.label_key(), format!("kind.{name}"));
            for metric in k.metrics() {
                let key = metric.label_key.strip_prefix("metric.").unwrap();
                assert!(
                    metrics.get(key).is_some_and(|t| !t.is_empty()),
                    "{lang} {name}: {}",
                    metric.label_key
                );
            }
        }
    }
}

#[test]
fn need_labels_read_naturally_for_one_and_for_many() {
    // The noun, a colon, then the count: «Nede: 1» and «Nede: 4» both read
    // well, and the chip already shows the service's name.
    for (lang, file) in locales() {
        let needs = section(file, "need");
        let ours: Vec<_> = needs
            .iter()
            .filter(|(key, _)| KINDS.iter().any(|k| key.starts_with(&format!("{k}_"))))
            .collect();
        assert!(ours.len() >= 5, "{lang}: {ours:?}");
        for (key, text) in ours {
            let noun = text
                .strip_suffix(": {count}")
                .unwrap_or_else(|| panic!("{lang} {key}: {text}"));
            assert!(
                !noun.is_empty() && !noun.contains(['{', '}', ':']),
                "{lang} {key}: {text}"
            );
        }
    }
}

#[test]
fn need_labels_use_the_same_terms_as_the_metrics() {
    // (need label, metric label) that name the same thing.
    for (lang, file) in locales() {
        let (needs, metrics) = (section(file, "need"), section(file, "metric"));
        for (need, metric) in [
            ("vv_down", "vv_down"),
            ("kk_new", "kk_new_issues"),
            ("kk_regressed", "kk_regressed_issues"),
            ("ww_failed", "ww_failed_targets"),
        ] {
            assert_eq!(
                needs[need].strip_suffix(": {count}"),
                Some(metrics[metric].as_str()),
                "{lang}: {need} and {metric}"
            );
        }
    }
}
