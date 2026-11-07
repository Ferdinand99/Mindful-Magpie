//! The start page layout: the document a user builds (groups of cards,
//! bookmarks, a background), its validation and its per-user storage.

pub mod background;
pub mod model;
pub mod store;

pub use background::{BackgroundError, Backgrounds, Kind};
pub use model::{
    Background, Bookmark, Card, FieldError, Group, Icon, Layout, PALETTE, PRESETS, ServiceRef,
    Size, validate,
};
pub use store::{SaveError, StoreError, load, load_or_default, save};
