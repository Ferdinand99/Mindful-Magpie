//! One layout per user, saved whole with a version number: a save must carry
//! the version it was loaded at, so a second tab with old data gets a
//! conflict instead of silently overwriting the first.

use rusqlite::{OptionalExtension as _, TransactionBehavior, params};
use vole_kit::db::{self, DbError};
use vole_kit::state::Core;

use super::model::{FieldError, Layout, validate};

/// Why a layout could not be read.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Db(#[from] DbError),
    /// The stored document is not a layout (it was changed outside Magpie).
    #[error("the stored layout is not a valid layout document: {0}")]
    Corrupt(serde_json::Error),
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
    let mut layout: Layout = serde_json::from_str(&doc).map_err(StoreError::Corrupt)?;
    // The column is the truth; the copy inside the document is for humans.
    layout.version = u64::try_from(version).unwrap_or_default();
    Ok(layout)
}

/// Stores `layout` as the user's layout when `layout.version` is the stored
/// version (0 when nothing is stored), and returns the new version. The read,
/// the comparison and the write happen in one transaction.
pub async fn save(core: &Core, user_id: i64, layout: &Layout) -> Result<u64, SaveError> {
    validate(layout).map_err(SaveError::Invalid)?;
    let expected = layout.version;
    let mut next = layout.clone();
    next.version = expected.saturating_add(1);
    let doc = serde_json::to_string(&next).map_err(SaveError::Encode)?;
    let now = jiff::Timestamp::now().to_string();

    let outcome: Result<u64, u64> = db::interact(&core.db, move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored: u64 = tx
            .query_row(
                "SELECT version FROM layouts WHERE user_id = ?1",
                params![user_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map_or(0, |v| u64::try_from(v).unwrap_or_default());
        if stored != expected {
            return Ok(Err(stored));
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
        Ok(Ok(stored + 1))
    })
    .await?;
    outcome.map_err(|current| SaveError::Conflict { current })
}
