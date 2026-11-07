//! One layout per user, saved whole with a version number: a save must carry
//! the version it was loaded at, so a second tab with old data gets a
//! conflict instead of silently overwriting the first.

use std::collections::HashMap;

use rusqlite::{OptionalExtension as _, TransactionBehavior, params};
use vole_kit::db::{self, DbError};
use vole_kit::state::Core;

use super::model::{Background, FieldError, Layout, ServiceRef, validate};
use crate::integrations;
use crate::services::{Viewer, store as services};

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
/// A picture background that is not one of the user's pictures (the current one,
/// or the kept one the stored layout used before the latest upload; it was
/// removed, or replaced by an upload while the layout used another) is shown as
/// the default preset: nobody
/// who reads the layout (the page, the extension) sees a dead picture. The
/// stored document and the version are left as they are, so what is saved over
/// it carries the version it was loaded at, as always.
pub async fn load_or_default(core: &Core, user_id: i64) -> Result<(Layout, bool), DbError> {
    let (mut layout, unreadable) = load_unchecked(core, user_id).await?;
    if let Background::Image { id } = &layout.background {
        let id = id.clone();
        let own: bool = db::interact(&core.db, move |c| {
            c.query_row(
                "SELECT EXISTS (SELECT 1 FROM backgrounds
                                WHERE user_id = ?1 AND (id = ?2 OR kept_id = ?2))",
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
    /// A card is bound to a service, or to a metric, that the user may not use.
    BadService(Vec<FieldError>),
}

/// One card's binding with where it is in the document.
struct Bound {
    path: String,
    card: String,
    service: ServiceRef,
}

/// The bound cards of `layout`, in document order.
fn bound_cards(layout: &Layout) -> Vec<Bound> {
    let mut out = Vec::new();
    for (gi, group) in layout.groups.iter().enumerate() {
        for (ci, card) in group.cards.iter().enumerate() {
            if let Some(service) = &card.service {
                out.push(Bound {
                    path: format!("groups[{gi}].cards[{ci}]"),
                    card: card.id.clone(),
                    service: service.clone(),
                });
            }
        }
    }
    out
}

/// What is wrong with the bindings in `bound`, for `viewer`, inside the
/// transaction `c`.
///
/// A binding the stored layout already has on the same card, unchanged, is
/// let through: the service may have been deleted or hidden since (the card
/// then shows "this service no longer exists"), and that must not stop the
/// person from saving anything else on the page. Every other binding must
/// name a service the viewer can see and metrics the service's kind has.
/// A hidden service and one that does not exist give the same answer.
fn binding_problems(
    c: &rusqlite::Connection,
    viewer: Viewer,
    bound: &[Bound],
    stored: Option<&Layout>,
) -> rusqlite::Result<Vec<FieldError>> {
    let kept: HashMap<&str, &ServiceRef> = stored
        .into_iter()
        .flat_map(|l| &l.groups)
        .flat_map(|g| &g.cards)
        .filter_map(|card| Some((card.id.as_str(), card.service.as_ref()?)))
        .collect();
    let mut kinds: HashMap<i64, Option<String>> = HashMap::new();
    let mut problems = Vec::new();
    for b in bound {
        if kept.get(b.card.as_str()) == Some(&&b.service) {
            continue;
        }
        let kind = match kinds.get(&b.service.id) {
            Some(kind) => kind.clone(),
            None => {
                let kind = services::visible_kind(c, viewer, b.service.id)?;
                kinds.insert(b.service.id, kind.clone());
                kind
            }
        };
        let Some(kind) = kind else {
            problems.push(FieldError {
                path: format!("{}.service", b.path),
                code: "bad_service",
            });
            continue;
        };
        let known = integrations::find(&kind).map(|k| k.metrics());
        let every_metric_exists = b
            .service
            .metrics
            .iter()
            .all(|m| known.is_some_and(|defs| defs.iter().any(|d| d.id == m)));
        if !every_metric_exists {
            problems.push(FieldError {
                path: format!("{}.service.metrics", b.path),
                code: "bad_metric",
            });
        }
    }
    Ok(problems)
}

/// Stores `layout` as the user's layout when `layout.version` is the stored
/// version (0 when nothing is stored), and returns the new version. The read,
/// the comparison and the write happen in one transaction.
///
/// An image background must be a picture the user has uploaded (the current or
/// the kept one): [`validate`] checks the shape of the id, and the transaction
/// checks it against the user's row, so a picture removed or replaced in
/// between is never saved.
/// That is a `bad_image` on `background.id`, before the version is looked at.
///
/// A card bound to a service must name one the user can see, and metrics its
/// kind has (`bad_service` on `…service`, `bad_metric` on `…service.metrics`;
/// see [`binding_problems`]). That is checked in the same transaction, after
/// the version, against the stored layout.
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
    let bound = bound_cards(&next);
    let outcome = db::interact(&core.db, move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(id) = image {
            let own: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM backgrounds
                                WHERE user_id = ?1 AND (id = ?2 OR kept_id = ?2))",
                params![user_id, id],
                |r| r.get(0),
            )?;
            if !own {
                return Ok(Outcome::BadImage);
            }
        }
        let row: Option<(i64, String)> = tx
            .query_row(
                "SELECT version, doc FROM layouts WHERE user_id = ?1",
                params![user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let stored = row
            .as_ref()
            .map_or(0, |(v, _)| u64::try_from(*v).unwrap_or_default());
        if stored != expected {
            return Ok(Outcome::Conflict(stored));
        }
        if !bound.is_empty() {
            let admin: bool = tx
                .query_row(
                    "SELECT role = 'admin' FROM users WHERE id = ?1",
                    params![user_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            // A stored document that cannot be read keeps nothing.
            let before = row.and_then(|(_, doc)| serde_json::from_str::<Layout>(&doc).ok());
            let problems =
                binding_problems(&tx, Viewer::new(user_id, admin), &bound, before.as_ref())?;
            if !problems.is_empty() {
                return Ok(Outcome::BadService(problems));
            }
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
        Outcome::BadService(problems) => Err(SaveError::Invalid(problems)),
    }
}
