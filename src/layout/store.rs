//! One layout per user, saved whole with a version number: a save must carry
//! the version it was loaded at, so a second tab with old data gets a
//! conflict instead of silently overwriting the first.

use rusqlite::{OptionalExtension as _, TransactionBehavior, params};
use vole_kit::db::{self, DbError};
use vole_kit::state::Core;

use super::model::{Background, FieldError, Layout, validate};

/// Why a layout could not be read.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Db(#[from] DbError),
    /// The stored document is not a layout (it was changed outside Magpie).
    /// `version` is the stored version, read in the same query as the
    /// document: a save over the broken document must carry it, and a save
    /// that landed in between is then a conflict, never an overwrite.
    #[error("the stored layout is not a valid layout document: {source}")]
    Corrupt {
        version: u64,
        source: serde_json::Error,
    },
}

/// Why a layout was not saved.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The document was loaded at another version than the stored one;
    /// `current` is the stored version.
    #[error("the layout changed since it was loaded (now at version {current})")]
    Conflict { current: u64 },
    /// The document breaks a rule of [`validate`]; nothing was stored.
    #[error("the layout is not valid")]
    Invalid(Vec<FieldError>),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("the layout could not be written as JSON: {0}")]
    Encode(serde_json::Error),
}

/// The user's layout, or the empty default (version 0, Hedgerow background)
/// when none is stored.
pub async fn load(core: &Core, user_id: i64) -> Result<Layout, StoreError> {
    let row: Option<(i64, String)> = db::interact(&core.db, move |c| {
        c.query_row(
            "SELECT version, doc FROM layouts WHERE user_id = ?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
    })
    .await?;
    let Some((version, doc)) = row else {
        return Ok(Layout::default());
    };
    // The column is the truth; the copy inside the document is for humans.
    let version = u64::try_from(version).unwrap_or_default();
    let mut layout: Layout =
        serde_json::from_str(&doc).map_err(|source| StoreError::Corrupt { version, source })?;
    layout.version = version;
    Ok(layout)
}

/// The user's layout for showing: when the stored document cannot be read,
/// the empty default at the stored version (so a save over the broken
/// document is accepted), and `true` for "it was unreadable".
///
/// A picture background that is not the user's current picture (it was
/// replaced by an upload, or removed) is shown as the default preset: nobody
/// who reads the layout (the page, the extension) sees a dead picture. The
/// stored document and the version are left as they are, so what is saved over
/// it carries the version it was loaded at, as always.
pub async fn load_or_default(core: &Core, user_id: i64) -> Result<(Layout, bool), DbError> {
    let (mut layout, unreadable) = load_unchecked(core, user_id).await?;
    if let Background::Image { id } = &layout.background {
        let id = id.clone();
        let own: bool = db::interact(&core.db, move |c| {
            c.query_row(
                "SELECT EXISTS (SELECT 1 FROM backgrounds WHERE user_id = ?1 AND id = ?2)",
                params![user_id, id],
                |r| r.get(0),
            )
        })
        .await?;
        if !own {
            layout.background = Layout::default().background;
        }
    }
    Ok((layout, unreadable))
}

/// [`load_or_default`] before the picture check.
async fn load_unchecked(core: &Core, user_id: i64) -> Result<(Layout, bool), DbError> {
    match load(core, user_id).await {
        Ok(layout) => Ok((layout, false)),
        Err(StoreError::Corrupt { version, source }) => {
            tracing::warn!(user = user_id, "stored layout is unreadable: {source}");
            Ok((
                Layout {
                    version,
                    ..Layout::default()
                },
                true,
            ))
        }
        Err(StoreError::Db(err)) => Err(err),
    }
}

/// How a save ended inside its transaction.
enum Outcome {
    Saved(u64),
    /// The stored version, which is not the one the document carried.
    Conflict(u64),
    /// The document uses an image that is not the user's.
    BadImage,
}

/// Stores `layout` as the user's layout when `layout.version` is the stored
/// version (0 when nothing is stored), and returns the new version. The read,
/// the comparison and the write happen in one transaction.
///
/// An image background must be the picture the user has uploaded: [`validate`]
/// checks the shape of the id, and the transaction checks it against the
/// user's row, so a picture removed or replaced in between is never saved.
/// That is a `bad_image` on `background.id`, before the version is looked at.
pub async fn save(core: &Core, user_id: i64, layout: &Layout) -> Result<u64, SaveError> {
    validate(layout).map_err(SaveError::Invalid)?;
    let expected = layout.version;
    let mut next = layout.clone();
    next.version = expected.saturating_add(1);
    let doc = serde_json::to_string(&next).map_err(SaveError::Encode)?;
    let now = jiff::Timestamp::now().to_string();

    let image = match &next.background {
        Background::Image { id } => Some(id.clone()),
        Background::Preset { .. } => None,
    };
    let outcome = db::interact(&core.db, move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(id) = image {
            let own: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM backgrounds WHERE user_id = ?1 AND id = ?2)",
                params![user_id, id],
                |r| r.get(0),
            )?;
            if !own {
                return Ok(Outcome::BadImage);
            }
        }
        let stored: u64 = tx
            .query_row(
                "SELECT version FROM layouts WHERE user_id = ?1",
                params![user_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map_or(0, |v| u64::try_from(v).unwrap_or_default());
        if stored != expected {
            return Ok(Outcome::Conflict(stored));
        }
        // Version 0 means "nothing stored", so the first stored version is 1.
        let new = i64::try_from(stored + 1).unwrap_or(i64::MAX);
        tx.execute(
            "INSERT INTO layouts (user_id, version, doc, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET
                 version = excluded.version, doc = excluded.doc, updated_at = excluded.updated_at",
            params![user_id, new, doc, now],
        )?;
        tx.commit()?;
        Ok(Outcome::Saved(stored + 1))
    })
    .await?;
    match outcome {
        Outcome::Saved(version) => Ok(version),
        Outcome::Conflict(current) => Err(SaveError::Conflict { current }),
        Outcome::BadImage => Err(SaveError::Invalid(vec![FieldError {
            path: "background.id".into(),
            code: "bad_image",
        }])),
    }
}
