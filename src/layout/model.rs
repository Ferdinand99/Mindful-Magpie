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

/// The most numbers one card can name from its service. A card shows any
/// number of its kind's metrics, one up to all of them, each at most once; this
/// is only a ceiling above the largest catalogue, so that a list of any length
/// is refused by its length and not walked (a test keeps it above every kind's
/// catalogue). The save checks the names against the kind's own.
pub const MAX_METRICS: usize = 32;

/// Longest id of an uploaded background image, in characters (the renderer
/// accepts the same shape, `render.js`).
pub const MAX_IMAGE_ID: usize = 64;

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
    /// The service whose numbers the card shows. Layouts saved before
    /// services existed have none, and a card without one is written without
    /// the key, so such a layout reads and saves back unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceRef>,
    /// How much room a card with a service takes. Absent means the default of
    /// the service's kind (`Integration::default_size`), so a layout saved
    /// before sizes existed reads and saves back unchanged. A card without a
    /// service has no size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
}

/// A card's size: one grid column, or two on a wide screen (one below about
/// 700 px).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    Normal,
    Wide,
}

/// A card's binding to a service: which one, and 1 or more of its metrics (at
/// most all the kind has, none twice), in the order the card shows them. The
/// ids are the kind's (`MetricDef::id`); [`validate`] checks their shape, count
/// and that none is repeated, and the save checks the service exists, is
/// visible to the user and has these metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRef {
    pub id: i64,
    pub metrics: Vec<String>,
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
/// `duplicate_id`, `bad_colour`, `bad_emoji`, `bad_preset`, `bad_image`,
/// `bad_service`, `bad_metric`, `bad_size`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    /// For example `groups[0].cards[2].url`.
    pub path: String,
    pub code: &'static str,
}

/// Whether `value` is `scheme://` followed by a host: the `//` must come right
/// after the scheme's colon (not from a later `://`, say in the query), and
/// more slashes must not follow it.
fn plain_authority(value: &str) -> bool {
    value
        .split_once(':')
        .and_then(|(_, rest)| rest.strip_prefix("//"))
        .is_some_and(|host| !host.starts_with(['/', '\\']))
}

/// What is wrong with a link that a browser is to follow (a card's, a
/// bookmark's, a service's), as the error code: `required`, `too_long`,
/// `bad_url` or `bad_scheme`. `None` for a well-formed `http` or `https` URL.
/// Nothing is fetched from it, so no address is refused for what it points at.
pub fn link_problem(value: &str) -> Option<&'static str> {
    if value.trim().is_empty() {
        return Some("required");
    }
    if value.chars().count() > MAX_URL {
        return Some("too_long");
    }
    // The URL parser strips tabs and line breaks and trims the ends, so what
    // is stored could differ from what a browser follows: refuse it.
    if value != value.trim() || value.chars().any(char::is_control) {
        return Some("bad_url");
    }
    match url::Url::parse(value) {
        Err(_) => Some("bad_url"),
        Ok(u) if !matches!(u.scheme(), "http" | "https") => Some("bad_scheme"),
        Ok(u) if u.host_str().is_none_or(str::is_empty) => Some("bad_url"),
        // `https:example.com` and `https:///example.com` parse as links too;
        // keep only the plain `scheme://host` shape.
        Ok(_) if !plain_authority(value) => Some("bad_url"),
        Ok(_) => None,
    }
}

/// Characters of Unicode category Cf (format): invisible, and a few reverse
/// the text or glue it together.
pub fn is_format(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}'
        | '\u{890}'..='\u{891}' | '\u{8e2}' | '\u{180e}' | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}'
        | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}'
        | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

/// The invisible parts real emoji are built from: the zero width joiner and
/// the variation selectors.
fn is_emoji_glue(c: char) -> bool {
    matches!(c, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}')
}

/// A tag character (U+E0020 to U+E007F).
fn is_tag(c: char) -> bool {
    matches!(c, '\u{e0020}'..='\u{e007f}')
}

/// Whether `emoji` is a subdivision flag: the black flag, tag characters and
/// the cancel tag (U+E007F) that ends the sequence. Tag characters are
/// invisible, so they are accepted only in this shape.
fn is_subdivision_flag(emoji: &str) -> bool {
    emoji
        .strip_prefix('\u{1f3f4}')
        .and_then(|rest| rest.strip_suffix('\u{e007f}'))
        .is_some_and(|tags| !tags.is_empty() && tags.chars().all(is_tag))
}

/// Whether `emoji` has no whitespace, control or format characters (except
/// the glue that emoji sequences need, and the tags of a subdivision flag) and
/// at least one character that shows.
fn is_visible_symbol(emoji: &str) -> bool {
    let flag = is_subdivision_flag(emoji);
    emoji.chars().all(|c| {
        !c.is_whitespace()
            && !c.is_control()
            && (!is_format(c) || is_emoji_glue(c) || (flag && is_tag(c)))
    }) && emoji.chars().any(|c| !is_emoji_glue(c) && !is_tag(c))
}

/// Whether `value` is 1 to 40 of `[A-Za-z0-9_-]`.
pub(crate) fn id_shape(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The most problems [`validate`] returns.
const MAX_ERRORS: usize = 100;

struct Checker {
    errors: Vec<FieldError>,
    ids: HashSet<String>,
}

impl Checker {
    fn err(&mut self, path: impl Into<String>, code: &'static str) {
        if self.errors.len() >= MAX_ERRORS {
            return;
        }
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
        if !id_shape(value) {
            self.err(path, "bad_id");
        } else if !self.ids.insert(value.to_string()) {
            self.err(path, "duplicate_id");
        }
    }

    /// A link: present, not too long, a well-formed `http` or `https` URL.
    fn url(&mut self, path: String, value: &str) {
        if let Some(code) = link_problem(value) {
            self.err(path, code);
        }
    }

    /// A card's service binding, as far as it can be told without the
    /// database: an id that can be one, and 1 or more metric ids of the shape of
    /// an id, none twice. One problem at most is reported: `bad_service` on
    /// `…service`, or one code on `…service.metrics`.
    fn service(&mut self, path: &str, service: &ServiceRef) {
        if service.id < 1 {
            return self.err(format!("{path}.service"), "bad_service");
        }
        let metrics = format!("{path}.service.metrics");
        if service.metrics.is_empty() {
            return self.err(metrics, "required");
        }
        // Over the limit: say so without walking a list of any length.
        if service.metrics.len() > MAX_METRICS {
            return self.err(metrics, "too_many");
        }
        if !service.metrics.iter().all(|m| id_shape(m)) {
            return self.err(metrics, "bad_id");
        }
        let distinct: HashSet<&str> = service.metrics.iter().map(String::as_str).collect();
        if distinct.len() != service.metrics.len() {
            self.err(metrics, "duplicate_id");
        }
    }

    fn icon(&mut self, path: &str, icon: &Icon) {
        if !PALETTE.contains(&icon.colour.as_str()) {
            self.err(format!("{path}.colour"), "bad_colour");
        }
        if let Some(emoji) = &icon.emoji
            && (emoji.len() > MAX_EMOJI_BYTES
                || emoji.graphemes(true).count() != 1
                || !is_visible_symbol(emoji))
        {
            self.err(format!("{path}.emoji"), "bad_emoji");
        }
    }
}

/// Checks a layout against the size limits and shapes. Returns the problems
/// found (at most 100), so a form can mark them at once. A document over a
/// count limit (`too_many`) is not checked further.
///
/// An image background must name an id of the right shape (`bad_image`).
/// Whether it is the user's own uploaded image needs the database, which this
/// function never touches: the save checks that (see `store::save`).
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
        Background::Image { id } => {
            let ok = !id.is_empty()
                && id.len() <= MAX_IMAGE_ID
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            if !ok {
                c.err("background.id", "bad_image");
            }
        }
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
    // Over a limit: say so and stop, rather than walk a huge document.
    if c.errors.iter().any(|e| e.code == "too_many") {
        return Err(c.errors);
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
            // A card with a service may have no link of its own: the page
            // links it to the service's. A card without one needs a link.
            if !(card.url.is_empty() && card.service.is_some()) {
                c.url(format!("{cp}.url"), &card.url);
            }
            c.icon(&format!("{cp}.icon"), &card.icon);
            if let Some(service) = &card.service {
                c.service(&cp, service);
            } else if card.size.is_some() {
                // A size is the size of a service's card.
                c.err(format!("{cp}.size"), "bad_size");
            }
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
