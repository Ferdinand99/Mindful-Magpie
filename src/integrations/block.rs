//! Detail blocks: what a card can show next to its numbers.
//!
//! A [`Snapshot`](super::Snapshot) carries `details`, a short list of [`Block`]s
//! a kind fills from data it fetched anyway. The vocabulary is small and typed
//! so that the renderer (`render.js`, the web page and the extension alike)
//! draws each kind of block the same way, with no markup from outside:
//!
//! - [`Block::List`]: lines of text, each with an optional second line, status
//!   dot, right-aligned value and link.
//! - [`Block::Bars`]: labelled bars filled to a percentage, with a text.
//! - [`Block::Strip`]: a row of status cells (for example the last day).
//! - [`Block::Facts`]: label and value rows.
//!
//! The JSON is tagged with `"kind"`, for example
//! `{"kind":"list","title_key":"x","items":[{"text":"a","status":"warn"}]}`.
//!
//! **Bounds.** Everything that comes from outside (names, titles, messages) is
//! text in a block, and a block is cut down by [`Block::bounded`] before it is
//! stored: a text is one line of at most [`TEXT_CHARS`] characters without
//! control or format characters ([`api::clean_text`](super::api::clean_text)),
//! a link is an `http` or `https` address or nothing, a bar's percentage is a
//! number from 0 to 100, a list or bar block has at most [`MAX_ITEMS`] entries,
//! a strip [`MAX_CELLS`] cells, a facts block [`MAX_FACTS`] rows, and a
//! snapshot at most [`MAX_BLOCKS`] blocks ([`Snapshot::bound_details`](super::Snapshot::bound_details)).
//!
//! **Titles and labels are keys, never text.** `title_key` and `label_key`
//! name a string in the locale files under `[detail]` (`detail.<key>`), shaped
//! `[a-z0-9_.]{1,64}`. The page carries those strings in its label map, and a
//! key it does not know shows nothing. A key of another shape is blanked here.

use serde::{Deserialize, Serialize};

use super::Status;
use super::api::clean_text;
use crate::layout::model::{is_format, link_problem};

/// Most characters of any text in a block.
pub const TEXT_CHARS: usize = 80;
/// Most entries of a list or bars block.
pub const MAX_ITEMS: usize = 8;
/// Most cells of a strip.
pub const MAX_CELLS: usize = 48;
/// Most rows of a facts block.
pub const MAX_FACTS: usize = 8;
/// Most blocks of one snapshot.
pub const MAX_BLOCKS: usize = 6;
/// Longest `title_key` or `label_key`.
pub const MAX_KEY: usize = 64;

/// One block of a card's details (see the module).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    List {
        title_key: String,
        items: Vec<ListItem>,
    },
    Bars {
        title_key: String,
        bars: Vec<Bar>,
    },
    Strip {
        title_key: String,
        cells: Vec<Status>,
    },
    Facts {
        title_key: String,
        rows: Vec<Fact>,
    },
}

/// One line of a list: the text, a smaller second line, a status dot, a value
/// at the right and a link (an `http` or `https` address).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ListItem {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

impl ListItem {
    /// A line of just `text`.
    pub fn new(text: impl Into<String>) -> Self {
        ListItem {
            text: text.into(),
            ..ListItem::default()
        }
    }
}

/// One bar: a label, how full it is (0 to 100) and the text at its end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub label: String,
    pub pct: f64,
    pub text: String,
}

/// One row of facts: the label (a locale key, or text from outside when the
/// label is a name) and the value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Fact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub value: String,
}

/// Whether `key` has the shape of a detail key: 1 to 64 of `[a-z0-9_.]`.
pub fn key_shape(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

/// Whether the locale has a text for the detail key (`detail.<key>`).
pub fn key_known(key: &str) -> bool {
    key_shape(key)
        && vole_kit::i18n::lookup(vole_kit::i18n::Lang::EN, &format!("detail.{key}")).is_some()
}

/// `key` when it has the shape of one, else nothing.
fn clean_key(key: String) -> String {
    if key_shape(&key) { key } else { String::new() }
}

/// A line of text from outside: cleaned and cut; `None` when nothing is left.
fn line(text: &str) -> Option<String> {
    let text = clean_text(text, TEXT_CHARS);
    (!text.is_empty()).then_some(text)
}

/// A link a browser may follow: a well-formed `http` or `https` address.
fn link(href: &str) -> Option<String> {
    let href = href.trim();
    (link_problem(href).is_none() && !href.chars().any(is_format)).then(|| href.to_string())
}

/// A bar's fill: a number from 0 to 100 (anything else is 0).
fn percent(pct: f64) -> f64 {
    if pct.is_finite() {
        pct.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

impl Block {
    /// The block cut down to the limits of the module, or `None` when nothing
    /// of it is left to show (no entry survives). Texts are cleaned and cut, a
    /// link that is not `http` or `https` is dropped (its line stays), a
    /// percentage is clamped, and a key of the wrong shape is blanked.
    pub fn bounded(self) -> Option<Block> {
        match self {
            Block::List { title_key, items } => {
                let items: Vec<ListItem> = items
                    .into_iter()
                    .filter_map(|item| {
                        Some(ListItem {
                            text: line(&item.text)?,
                            sub: item.sub.as_deref().and_then(line),
                            status: item.status,
                            right: item.right.as_deref().and_then(line),
                            href: item.href.as_deref().and_then(link),
                        })
                    })
                    .take(MAX_ITEMS)
                    .collect();
                (!items.is_empty()).then(|| Block::List {
                    title_key: clean_key(title_key),
                    items,
                })
            }
            Block::Bars { title_key, bars } => {
                let bars: Vec<Bar> = bars
                    .into_iter()
                    .filter_map(|bar| {
                        Some(Bar {
                            label: line(&bar.label)?,
                            pct: percent(bar.pct),
                            text: line(&bar.text).unwrap_or_default(),
                        })
                    })
                    .take(MAX_ITEMS)
                    .collect();
                (!bars.is_empty()).then(|| Block::Bars {
                    title_key: clean_key(title_key),
                    bars,
                })
            }
            Block::Strip {
                title_key,
                mut cells,
            } => {
                cells.truncate(MAX_CELLS);
                (!cells.is_empty()).then(|| Block::Strip {
                    title_key: clean_key(title_key),
                    cells,
                })
            }
            Block::Facts { title_key, rows } => {
                let rows: Vec<Fact> = rows
                    .into_iter()
                    .filter_map(|row| {
                        let label_key = row.label_key.map(clean_key).filter(|k| !k.is_empty());
                        let label = row.label.as_deref().and_then(line);
                        let value = line(&row.value)?;
                        Some(Fact {
                            label_key,
                            label,
                            value,
                        })
                    })
                    .take(MAX_FACTS)
                    .collect();
                (!rows.is_empty()).then(|| Block::Facts {
                    title_key: clean_key(title_key),
                    rows,
                })
            }
        }
    }

    /// The locale keys the block names (its title and its rows' labels), for
    /// a test that every key a kind writes has a text.
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Block::List { title_key, .. }
            | Block::Bars { title_key, .. }
            | Block::Strip { title_key, .. } => vec![title_key],
            Block::Facts { title_key, rows } => std::iter::once(title_key.as_str())
                .chain(rows.iter().filter_map(|r| r.label_key.as_deref()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: Vec<ListItem>) -> Block {
        Block::List {
            title_key: "x.title".into(),
            items,
        }
    }

    #[test]
    fn the_json_shape_is_tagged_by_kind() {
        let block = list(vec![ListItem {
            text: "a".into(),
            sub: Some("b".into()),
            status: Some(Status::Warn),
            right: Some("c".into()),
            href: Some("https://x.example/".into()),
        }]);
        assert_eq!(
            serde_json::to_value(&block).unwrap(),
            serde_json::json!({
                "kind": "list", "title_key": "x.title",
                "items": [{ "text": "a", "sub": "b", "status": "warn", "right": "c", "href": "https://x.example/" }]
            })
        );
        let bars = Block::Bars {
            title_key: "k".into(),
            bars: vec![Bar {
                label: "CPU".into(),
                pct: 40.0,
                text: "40 %".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(&bars).unwrap(),
            serde_json::json!({ "kind": "bars", "title_key": "k",
                "bars": [{ "label": "CPU", "pct": 40.0, "text": "40 %" }] })
        );
        let strip = Block::Strip {
            title_key: "k".into(),
            cells: vec![Status::Ok, Status::Bad, Status::Unknown],
        };
        assert_eq!(
            serde_json::to_value(&strip).unwrap(),
            serde_json::json!({ "kind": "strip", "title_key": "k", "cells": ["ok", "bad", "unknown"] })
        );
        let facts = Block::Facts {
            title_key: "k".into(),
            rows: vec![Fact {
                label_key: Some("a.b".into()),
                label: None,
                value: "1".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(&facts).unwrap(),
            serde_json::json!({ "kind": "facts", "title_key": "k",
                "rows": [{ "label_key": "a.b", "value": "1" }] })
        );
        // And back; optional parts may be absent.
        let back: Block = serde_json::from_value(
            serde_json::json!({ "kind": "list", "title_key": "k", "items": [{ "text": "t" }] }),
        )
        .unwrap();
        assert_eq!(
            back,
            Block::List {
                title_key: "k".into(),
                items: vec![ListItem::new("t")]
            }
        );
    }

    #[test]
    fn hostile_text_is_cleaned_and_cut() {
        let hostile = format!(
            "<script>alert(1)</script>\u{202E}evil\u{0}\n{}",
            "x".repeat(500)
        );
        let Some(Block::List { items, .. }) = list(vec![ListItem {
            text: hostile.clone(),
            sub: Some(hostile.clone()),
            status: None,
            right: Some(hostile.clone()),
            href: None,
        }])
        .bounded() else {
            panic!("a list")
        };
        for text in [
            Some(&items[0].text),
            items[0].sub.as_ref(),
            items[0].right.as_ref(),
        ] {
            let text = text.unwrap();
            assert!(text.chars().count() <= TEXT_CHARS, "{text}");
            assert!(!text.contains(['\u{202E}', '\u{0}', '\n']), "{text}");
        }
        // The markup is kept as text (the renderer shows it as text); a
        // nothing-but-control line is dropped.
        assert!(items[0].text.starts_with("<script>"));
        assert_eq!(list(vec![ListItem::new("\u{202E}\u{0}\n")]).bounded(), None);
    }

    #[test]
    fn only_http_and_https_links_stay() {
        for (href, keeps) in [
            ("https://a.example/x?y=1", true),
            ("http://10.0.0.1:8080/", true),
            ("javascript:alert(1)", false),
            ("JaVaScRiPt:alert(1)", false),
            ("data:text/html,<b>x</b>", false),
            ("ftp://a.example/", false),
            ("//a.example/", false),
            ("/relative", false),
            ("https://", false),
            ("https:///a", false),
            ("https://a.example/\u{202E}", false),
            ("", false),
        ] {
            let mut item = ListItem::new("t");
            item.href = Some(href.into());
            let Some(Block::List { items, .. }) = list(vec![item]).bounded() else {
                panic!("the line stays even when its link goes")
            };
            assert_eq!(items[0].href.is_some(), keeps, "{href:?}");
            assert_eq!(items[0].text, "t");
        }
    }

    #[test]
    fn a_percentage_is_a_number_from_0_to_100() {
        let bar = |pct| Bar {
            label: "l".into(),
            pct,
            text: "t".into(),
        };
        let Some(Block::Bars { bars, .. }) = (Block::Bars {
            title_key: "k".into(),
            bars: vec![
                bar(f64::NAN),
                bar(f64::INFINITY),
                bar(f64::NEG_INFINITY),
                bar(1e300),
                bar(-5.0),
                bar(37.5),
            ],
        })
        .bounded() else {
            panic!("bars")
        };
        let pcts: Vec<f64> = bars.iter().map(|b| b.pct).collect();
        assert_eq!(pcts, vec![0.0, 0.0, 0.0, 100.0, 0.0, 37.5]);
        // What is stored is valid JSON that reads back.
        let json = serde_json::to_string(&bars).unwrap();
        assert!(serde_json::from_str::<Vec<Bar>>(&json).is_ok(), "{json}");
    }

    #[test]
    fn the_counts_are_capped() {
        let many = |n: usize| (0..n).map(|i| ListItem::new(format!("i{i}"))).collect();
        let Some(Block::List { items, .. }) = list(many(500)).bounded() else {
            panic!("a list")
        };
        assert_eq!(items.len(), MAX_ITEMS);
        assert_eq!(items[0].text, "i0");
        let Some(Block::Bars { bars, .. }) = (Block::Bars {
            title_key: "k".into(),
            bars: (0..99)
                .map(|i| Bar {
                    label: format!("b{i}"),
                    pct: 1.0,
                    text: String::new(),
                })
                .collect(),
        })
        .bounded() else {
            panic!("bars")
        };
        assert_eq!(bars.len(), MAX_ITEMS);
        let Some(Block::Strip { cells, .. }) = (Block::Strip {
            title_key: "k".into(),
            cells: vec![Status::Ok; 1000],
        })
        .bounded() else {
            panic!("a strip")
        };
        assert_eq!(cells.len(), MAX_CELLS);
        let Some(Block::Facts { rows, .. }) = (Block::Facts {
            title_key: "k".into(),
            rows: (0..50)
                .map(|i| Fact {
                    label_key: None,
                    label: Some(format!("l{i}")),
                    value: "v".into(),
                })
                .collect(),
        })
        .bounded() else {
            panic!("facts")
        };
        assert_eq!(rows.len(), MAX_FACTS);
    }

    #[test]
    fn a_block_with_nothing_left_is_dropped() {
        assert_eq!(list(vec![]).bounded(), None);
        assert_eq!(
            Block::Strip {
                title_key: "k".into(),
                cells: vec![]
            }
            .bounded(),
            None
        );
        assert_eq!(
            Block::Facts {
                title_key: "k".into(),
                rows: vec![Fact {
                    label_key: None,
                    label: None,
                    value: "\u{0}".into()
                }]
            }
            .bounded(),
            None
        );
    }

    #[test]
    fn a_key_of_the_wrong_shape_is_blanked() {
        for bad in [
            "",
            "Upper",
            "has space",
            "dash-ed",
            "<script>",
            "ü",
            &"a".repeat(65),
        ] {
            let Some(Block::List { title_key, .. }) = (Block::List {
                title_key: bad.into(),
                items: vec![ListItem::new("t")],
            })
            .bounded() else {
                panic!("the block stays")
            };
            assert_eq!(title_key, "", "{bad:?}");
        }
        for good in ["a", "a.b_c9", &"a".repeat(64)] {
            let Some(Block::List { title_key, .. }) = (Block::List {
                title_key: good.into(),
                items: vec![ListItem::new("t")],
            })
            .bounded() else {
                panic!("a list")
            };
            assert_eq!(title_key, good);
        }
        // A fact's label key of a bad shape falls back to its label.
        let Some(Block::Facts { rows, .. }) = (Block::Facts {
            title_key: "k".into(),
            rows: vec![Fact {
                label_key: Some("BAD KEY".into()),
                label: Some("Name".into()),
                value: "1".into(),
            }],
        })
        .bounded() else {
            panic!("facts")
        };
        assert_eq!(rows[0].label_key, None);
        assert_eq!(rows[0].label.as_deref(), Some("Name"));
    }
}
