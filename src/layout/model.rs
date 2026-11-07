//! The layout document and its validation.
//!
//! The document is plain data. Titles and descriptions are text: markup in
//! them is stored as typed and the renderer shows it as text, so `validate`
//! only checks sizes, shapes and link schemes. It is pure (no database).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation as _;

/// The most groups a user can have.
pub const MAX_GROUPS: usize = 30;
/// The most cards a user can have, over all groups.
pub const MAX_CARDS: usize = 300;
/// The most bookmarks a user can have.
pub const MAX_BOOKMARKS: usize = 100;
/// Longest card or bookmark title, in characters.
pub const MAX_TITLE: usize = 80;
/// Longest group title, in characters.
pub const MAX_GROUP_TITLE: usize = 60;
/// Longest card description, in characters.
pub const MAX_DESCRIPTION: usize = 160;
/// Longest link, in characters.
pub const MAX_URL: usize = 2048;
/// Longest id, in characters.
pub const MAX_ID: usize = 40;
/// Most bytes in the one grapheme of an emoji icon. The longest real emoji
/// sequences are about 30 bytes; the cap stops a grapheme made of thousands
/// of combining marks.
pub const MAX_EMOJI_BYTES: usize = 64;

/// The background presets.
pub const PRESETS: [&str; 4] = ["hedgerow", "natt", "skumring", "hav"];

/// The kit's 13 palette colour names (`--pal-*`).
pub const PALETTE: [&str; 13] = [
    "red", "orange", "yellow", "green", "mint", "teal", "cyan", "blue", "indigo", "purple", "pink",
    "brown", "gray",
];

/// One user's start page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// The version this document was loaded at; a save must carry it.
    pub version: u64,
    pub background: Background,
    pub groups: Vec<Group>,
    pub bookmarks: Vec<Bookmark>,
}

impl Default for Layout {
    /// What a user who never saved sees: nothing on the Hedgerow background.
    fn default() -> Self {
        Layout {
            version: 0,
            background: Background::Preset {
                name: "hedgerow".into(),
            },
            groups: Vec::new(),
            bookmarks: Vec::new(),
        }
    }
}

/// A colour field preset or the user's uploaded image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Background {
    Preset { name: String },
    Image { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub title: String,
    pub cards: Vec<Card>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub icon: Icon,
}

/// A letter tile: the first letter of the title, or one emoji, on a palette
/// colour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Icon {
    pub emoji: Option<String>,
    pub colour: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    pub id: String,
    pub title: String,
    pub url: String,
}

/// One thing wrong with a document: where, and a stable machine code
/// (`too_long`, `required`, `bad_url`, `bad_scheme`, `too_many`, `bad_id`,
/// `duplicate_id`, `bad_colour`, `bad_emoji`, `bad_preset`, `bad_image`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    /// For example `groups[0].cards[2].url`.
    pub path: String,
    pub code: &'static str,
}

/// Whether `value` is `scheme://` followed by a host, not by more slashes.
fn plain_authority(value: &str) -> bool {
    value
        .split_once("://")
        .is_some_and(|(_, rest)| !rest.starts_with(['/', '\\']))
}

struct Checker {
    errors: Vec<FieldError>,
    ids: HashSet<String>,
}

impl Checker {
    fn err(&mut self, path: impl Into<String>, code: &'static str) {
        self.errors.push(FieldError {
            path: path.into(),
            code,
        });
    }

    /// A text field: not empty (when `required`) and at most `max` characters.
    fn text(&mut self, path: String, value: &str, max: usize, required: bool) {
        if required && value.trim().is_empty() {
            self.err(path, "required");
        } else if value.chars().count() > max {
            self.err(path, "too_long");
        }
    }

    /// An id: 1 to 40 of `[A-Za-z0-9_-]`, unique in the document.
    fn id(&mut self, path: String, value: &str) {
        let ok = !value.is_empty()
            && value.len() <= MAX_ID
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !ok {
            self.err(path, "bad_id");
        } else if !self.ids.insert(value.to_string()) {
            self.err(path, "duplicate_id");
        }
    }

    /// A link: present, not too long, a well-formed `http` or `https` URL.
    fn url(&mut self, path: String, value: &str) {
        if value.trim().is_empty() {
            return self.err(path, "required");
        }
        if value.chars().count() > MAX_URL {
            return self.err(path, "too_long");
        }
        // The URL parser strips tabs and line breaks and trims the ends, so
        // what is stored could differ from what a browser follows: refuse it.
        if value != value.trim() || value.chars().any(char::is_control) {
            return self.err(path, "bad_url");
        }
        match url::Url::parse(value) {
            Err(_) => self.err(path, "bad_url"),
            Ok(u) if !matches!(u.scheme(), "http" | "https") => self.err(path, "bad_scheme"),
            Ok(u) if u.host_str().is_none_or(str::is_empty) => self.err(path, "bad_url"),
            // `https:example.com` and `https:///example.com` parse as links
            // too; keep only the plain `scheme://host` shape.
            Ok(_) if !plain_authority(value) => self.err(path, "bad_url"),
            Ok(_) => {}
        }
    }

    fn icon(&mut self, path: &str, icon: &Icon) {
        if !PALETTE.contains(&icon.colour.as_str()) {
            self.err(format!("{path}.colour"), "bad_colour");
        }
        if let Some(emoji) = &icon.emoji
            && (emoji.len() > MAX_EMOJI_BYTES || emoji.graphemes(true).count() != 1)
        {
            self.err(format!("{path}.emoji"), "bad_emoji");
        }
    }
}

/// Checks a layout against the size limits and shapes. Returns every problem
/// found, so a form can mark all of them at once.
///
/// Until backgrounds with images arrive, an image background is refused
/// (`bad_image`): the check needs the database, which this function never
/// touches.
pub fn validate(layout: &Layout) -> Result<(), Vec<FieldError>> {
    let mut c = Checker {
        errors: Vec::new(),
        ids: HashSet::new(),
    };

    match &layout.background {
        Background::Preset { name } => {
            if !PRESETS.contains(&name.as_str()) {
                c.err("background.name", "bad_preset");
            }
        }
        Background::Image { .. } => c.err("background.id", "bad_image"),
    }

    if layout.groups.len() > MAX_GROUPS {
        c.err("groups", "too_many");
    }
    if layout.groups.iter().map(|g| g.cards.len()).sum::<usize>() > MAX_CARDS {
        c.err("cards", "too_many");
    }
    if layout.bookmarks.len() > MAX_BOOKMARKS {
        c.err("bookmarks", "too_many");
    }

    for (gi, group) in layout.groups.iter().enumerate() {
        let gp = format!("groups[{gi}]");
        c.id(format!("{gp}.id"), &group.id);
        c.text(format!("{gp}.title"), &group.title, MAX_GROUP_TITLE, true);
        for (ci, card) in group.cards.iter().enumerate() {
            let cp = format!("{gp}.cards[{ci}]");
            c.id(format!("{cp}.id"), &card.id);
            c.text(format!("{cp}.title"), &card.title, MAX_TITLE, true);
            c.text(
                format!("{cp}.description"),
                &card.description,
                MAX_DESCRIPTION,
                false,
            );
            c.url(format!("{cp}.url"), &card.url);
            c.icon(&format!("{cp}.icon"), &card.icon);
        }
    }
    for (bi, bookmark) in layout.bookmarks.iter().enumerate() {
        let bp = format!("bookmarks[{bi}]");
        c.id(format!("{bp}.id"), &bookmark.id);
        c.text(format!("{bp}.title"), &bookmark.title, MAX_TITLE, true);
        c.url(format!("{bp}.url"), &bookmark.url);
    }

    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}
