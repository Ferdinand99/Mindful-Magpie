//! The user's uploaded start page background: one picture per user.
//!
//! The row in `backgrounds` holds the metadata and is the truth about which
//! pictures are the user's: the current one and, while the stored layout still
//! uses the one before it, that one (`kept_*`). The file lives at
//! `<data_dir>/backgrounds/<user_id>/<id>.<ext>`. `<id>` is a prefix of the
//! SHA-256 of the content, so the same picture always has the same name and a
//! browser can cache it for good.
//!
//! Order of events, so that a failure never leaves a row without a file:
//!
//! - an upload writes the file (a temporary file, then a rename), commits the
//!   row, and only then deletes everything else in the user's directory
//!   (leftovers of a crash, a picture nothing uses). The picture the stored
//!   layout uses is not touched: it becomes the kept picture, so that a Cancel
//!   after an upload leaves the user's page as it was; the layout that is saved
//!   later decides which of the two stays (the client asks for the other to go);
//! - a delete commits the row first, then removes the file; it never
//!   touches the layout (a layout that still names a removed picture is shown
//!   on the default preset, see `store::load_or_default`);
//! - [`Backgrounds::reconcile`] removes what no row accounts for. It runs at
//!   start-up and after an admin deletes a user: the row goes with the user by
//!   `ON DELETE CASCADE`, the vole-kit has no hook that tells us about the
//!   file.
//!
//! One lock serialises upload, delete and sweep, so that a sweep never removes
//! the file of an upload that has not committed yet.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use rusqlite::{OptionalExtension as _, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;
use vole_kit::db::{self, DbError};
use vole_kit::state::Core;

use super::model::{Background, Layout};

/// The largest picture, in bytes.
pub const MAX_BYTES: usize = 5 * 1024 * 1024;

/// Hex characters of the SHA-256 that make the id (128 bits).
const ID_LEN: usize = 32;

/// The picture formats Magpie keeps, told apart by their first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    WebP,
}

impl Kind {
    /// What `data` is by its magic bytes (never by a name or a declared type),
    /// or `None` when it is none of the three.
    pub fn sniff(data: &[u8]) -> Option<Kind> {
        if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
            Some(Kind::Png)
        } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Kind::Jpeg)
        } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            Some(Kind::WebP)
        } else {
            None
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
            Kind::WebP => "image/webp",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::WebP => "webp",
        }
    }

    fn from_mime(mime: &str) -> Option<Kind> {
        [Kind::Png, Kind::Jpeg, Kind::WebP]
            .into_iter()
            .find(|k| k.mime() == mime)
    }
}

/// The id of a picture: the first 128 bits of its SHA-256, in hex.
pub fn image_id(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut id = String::with_capacity(ID_LEN);
    for byte in digest.iter().take(ID_LEN / 2) {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// What went wrong outside the caller's control.
#[derive(Debug, thiserror::Error)]
pub enum BackgroundError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("background file: {0}")]
    Io(#[from] io::Error),
    #[error("the background task did not finish: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// The file's name inside the user's directory.
fn file_name(id: &str, kind: Kind) -> String {
    format!("{id}.{}", kind.ext())
}

/// The place of the backgrounds, and the lock that keeps their changes in order.
#[derive(Debug)]
pub struct Backgrounds {
    root: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Backgrounds {
    /// The backgrounds of the Magpie whose data lives in `data_dir`.
    pub fn new(data_dir: &Path) -> Backgrounds {
        Backgrounds {
            root: data_dir.join("backgrounds"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn user_dir(&self, user_id: i64) -> PathBuf {
        self.root.join(user_id.to_string())
    }

    /// Keeps `data` (a picture of `kind`) as the user's background and returns
    /// its id. The previous picture is gone once this returns.
    ///
    /// The work runs in a task of its own that holds the lock: a client that
    /// goes away (which drops this future) cannot stop it between the write
    /// and the commit, where a sweep could otherwise remove the new file.
    pub async fn store(
        &self,
        core: &Core,
        user_id: i64,
        kind: Kind,
        data: Bytes,
    ) -> Result<String, BackgroundError> {
        let guard = Arc::clone(&self.lock).lock_owned().await;
        let (core, dir) = (core.clone(), self.user_dir(user_id));
        let task = tokio::spawn(async move {
            let _guard = guard;
            store_locked(&core, &dir, user_id, kind, &data).await
        });
        task.await?
    }

    /// The user's picture when `id` is the id of the current or the kept
    /// picture: its kind and bytes. `None` when the user has no such picture or
    /// the file is missing.
    pub async fn read(
        &self,
        core: &Core,
        user_id: i64,
        id: &str,
    ) -> Result<Option<(Kind, Vec<u8>)>, BackgroundError> {
        let Some(row) = meta(core, user_id).await? else {
            return Ok(None);
        };
        let Some(picture) = row.pictures().into_iter().find(|p| p.id == id) else {
            return Ok(None);
        };
        let Some(kind) = Kind::from_mime(&picture.mime) else {
            return Ok(None);
        };
        let path = self.user_dir(user_id).join(file_name(&picture.id, kind));
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some((kind, data))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::warn!(user = user_id, "a background row has no file");
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Removes the user's picture `id`, when it is the current or the kept
    /// picture and the stored layout does not use it. Anything else is a no-op
    /// (the picture was replaced, or a layout saved since points at it), so a
    /// stale tab can never take away a picture another tab has put to use.
    /// Removing the current picture while one is kept makes the kept one the
    /// current. The layout and its version are never changed. Returns whether a
    /// picture was removed.
    ///
    /// Like [`Backgrounds::store`], it finishes in a task of its own.
    pub async fn remove(
        &self,
        core: &Core,
        user_id: i64,
        id: &str,
    ) -> Result<bool, BackgroundError> {
        let guard = Arc::clone(&self.lock).lock_owned().await;
        let (core, dir, id) = (core.clone(), self.user_dir(user_id), id.to_string());
        let task = tokio::spawn(async move {
            let _guard = guard;
            remove_locked(&core, &dir, user_id, id).await
        });
        task.await?
    }

    /// Removes the files no row accounts for: the directory of a user who has
    /// no picture (the user is gone, or the picture was removed), and in a
    /// directory that has one, every file but the picture's. Entries that are
    /// not a user's number are not ours and stay.
    pub async fn reconcile(&self, core: &Core) -> Result<(), BackgroundError> {
        let _guard = self.lock.lock().await;
        let rows: HashMap<i64, Vec<String>> = db::interact(&core.db, |c| {
            let mut stmt = c.prepare("SELECT user_id FROM backgrounds")?;
            let users = stmt
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut keep = HashMap::new();
            for user_id in users {
                if let Some(row) = row_in(c, user_id)? {
                    keep.insert(user_id, row.file_names());
                }
            }
            Ok(keep)
        })
        .await?;

        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let Some(user_id) = name
                .to_str()
                .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<i64>().ok())
            else {
                continue;
            };
            if !entry.file_type().await.is_ok_and(|t| t.is_dir()) {
                continue;
            }
            match rows.get(&user_id) {
                Some(keep) => prune(&entry.path(), keep).await,
                None => remove_dir(&entry.path()).await,
            }
        }
        Ok(())
    }
}

/// One picture of a user: its id, type and size.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Picture {
    id: String,
    mime: String,
    bytes: i64,
}

impl Picture {
    fn file(&self) -> Option<String> {
        Some(file_name(&self.id, Kind::from_mime(&self.mime)?))
    }
}

/// The user's `backgrounds` row: the current picture, and the kept one (the
/// picture the stored layout used before the latest upload).
#[derive(Debug, Clone)]
struct Row {
    current: Picture,
    kept: Option<Picture>,
}

impl Row {
    fn pictures(&self) -> Vec<Picture> {
        let mut all = vec![self.current.clone()];
        all.extend(self.kept.clone());
        all
    }

    /// The names of the files the row accounts for.
    fn file_names(&self) -> Vec<String> {
        self.pictures().iter().filter_map(Picture::file).collect()
    }
}

/// Reads the user's row.
fn row_in(c: &rusqlite::Connection, user_id: i64) -> rusqlite::Result<Option<Row>> {
    c.query_row(
        "SELECT id, mime, bytes, kept_id, kept_mime, kept_bytes
         FROM backgrounds WHERE user_id = ?1",
        params![user_id],
        |r| {
            let kept = match (
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ) {
                (Some(id), Some(mime)) => Some(Picture {
                    id,
                    mime,
                    bytes: r.get::<_, Option<i64>>(5)?.unwrap_or_default(),
                }),
                _ => None,
            };
            Ok(Row {
                current: Picture {
                    id: r.get(0)?,
                    mime: r.get(1)?,
                    bytes: r.get(2)?,
                },
                kept,
            })
        },
    )
    .optional()
}

/// The stored layout document of the user, if any.
fn doc_in(c: &rusqlite::Connection, user_id: i64) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT doc FROM layouts WHERE user_id = ?1",
        params![user_id],
        |r| r.get(0),
    )
    .optional()
}

/// Whether the stored layout `doc` uses the picture `id`. A document that
/// cannot be read may still name the picture, so it counts as use when the id
/// appears in it.
fn layout_uses(doc: Option<&str>, id: &str) -> bool {
    doc.is_some_and(|doc| match serde_json::from_str::<Layout>(doc) {
        Ok(layout) => {
            matches!(&layout.background, Background::Image { id: used } if used == id)
        }
        Err(_) => doc.contains(id),
    })
}

/// The body of [`Backgrounds::store`], with the lock held.
async fn store_locked(
    core: &Core,
    dir: &Path,
    user_id: i64,
    kind: Kind,
    data: &[u8],
) -> Result<String, BackgroundError> {
    let id = image_id(data);
    let name = file_name(&id, kind);
    write_atomically(dir, &name, data).await?;

    let new = Picture {
        id: id.clone(),
        mime: kind.mime().to_string(),
        bytes: i64::try_from(data.len()).unwrap_or(i64::MAX),
    };
    let now = jiff::Timestamp::now().to_string();
    let stored = db::interact(&core.db, move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let doc = doc_in(&tx, user_id)?;
        // The picture the stored layout uses stays (as the kept one) until a
        // saved layout stops using it; anything else makes way for the new one.
        let kept = row_in(&tx, user_id)?.and_then(|row| {
            row.pictures()
                .into_iter()
                .find(|p| p.id != new.id && layout_uses(doc.as_deref(), &p.id))
        });
        tx.execute(
            "INSERT INTO backgrounds
                 (user_id, id, mime, bytes, created_at, kept_id, kept_mime, kept_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(user_id) DO UPDATE SET
                 id = excluded.id, mime = excluded.mime,
                 bytes = excluded.bytes, created_at = excluded.created_at,
                 kept_id = excluded.kept_id, kept_mime = excluded.kept_mime,
                 kept_bytes = excluded.kept_bytes",
            params![
                user_id,
                new.id,
                new.mime,
                new.bytes,
                now,
                kept.as_ref().map(|p| p.id.clone()),
                kept.as_ref().map(|p| p.mime.clone()),
                kept.as_ref().map(|p| p.bytes),
            ],
        )?;
        tx.commit()?;
        Ok(Row { current: new, kept }.file_names())
    })
    .await;
    // What the row names is what to keep. After a failed upsert that is what
    // was there before, or nothing when there was nothing. When the row cannot
    // be read either (a busy or broken database), nothing is known about what
    // to keep, so nothing is removed: the next sweep decides.
    let keep = match &stored {
        Ok(files) => Ok(files.clone()),
        Err(_) => row_files(core, user_id).await,
    };
    match keep {
        Ok(files) if files.is_empty() => remove_dir(dir).await,
        Ok(files) => prune(dir, &files).await,
        Err(err) => {
            tracing::warn!(
                user = user_id,
                "leaving the background files to the sweep: {err}"
            );
        }
    }
    stored?;
    Ok(id)
}

/// The body of [`Backgrounds::remove`], with the lock held. The row, the stored
/// layout and the delete are one transaction, so that a layout saved at the
/// same moment either sees the picture gone (and is refused as `bad_image`) or
/// is seen here (and keeps its picture).
async fn remove_locked(
    core: &Core,
    dir: &Path,
    user_id: i64,
    id: String,
) -> Result<bool, BackgroundError> {
    let outcome = db::interact(&core.db, move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(row) = row_in(&tx, user_id)? else {
            return Ok(None);
        };
        if !row.pictures().iter().any(|p| p.id == id) {
            return Ok(None);
        }
        if layout_uses(doc_in(&tx, user_id)?.as_deref(), &id) {
            return Ok(None);
        }
        let left = if row.current.id == id {
            // The kept picture, if any, becomes the current one.
            match &row.kept {
                Some(kept) => {
                    tx.execute(
                        "UPDATE backgrounds SET id = ?2, mime = ?3, bytes = ?4,
                             kept_id = NULL, kept_mime = NULL, kept_bytes = NULL
                         WHERE user_id = ?1",
                        params![user_id, kept.id, kept.mime, kept.bytes],
                    )?;
                    Some(Row {
                        current: kept.clone(),
                        kept: None,
                    })
                }
                None => {
                    tx.execute(
                        "DELETE FROM backgrounds WHERE user_id = ?1",
                        params![user_id],
                    )?;
                    None
                }
            }
        } else {
            tx.execute(
                "UPDATE backgrounds SET kept_id = NULL, kept_mime = NULL, kept_bytes = NULL
                 WHERE user_id = ?1",
                params![user_id],
            )?;
            Some(Row {
                current: row.current.clone(),
                kept: None,
            })
        };
        tx.commit()?;
        Ok(Some(left.map(|r| r.file_names()).unwrap_or_default()))
    })
    .await?;
    let Some(files) = outcome else {
        return Ok(false);
    };
    if files.is_empty() {
        remove_dir(dir).await;
    } else {
        prune(dir, &files).await;
    }
    Ok(true)
}

/// The files the user's row accounts for: an empty list when there is no row,
/// an error when the row could not be read.
async fn row_files(core: &Core, user_id: i64) -> Result<Vec<String>, DbError> {
    Ok(meta(core, user_id)
        .await?
        .map(|row| row.file_names())
        .unwrap_or_default())
}

/// The user's row.
async fn meta(core: &Core, user_id: i64) -> Result<Option<Row>, DbError> {
    db::interact(&core.db, move |c| row_in(c, user_id)).await
}

/// Writes `data` to `dir/name` so that the name never holds half a file: a
/// temporary file in the same directory, flushed to disk, then renamed over.
async fn write_atomically(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let temporary = dir.join(format!(".{name}.tmp"));
    let written = async {
        let mut file = tokio::fs::File::create(&temporary).await?;
        file.write_all(data).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, dir.join(name)).await
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    written
}

/// Removes everything in `dir` but the files `keep`. Failures are logged: a
/// file that stays is found by the next sweep.
async fn prune(dir: &Path, keep: &[String]) {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), "can't list a background directory: {err}");
            }
            return;
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if keep.iter().any(|name| entry.file_name() == name.as_str()) {
            continue;
        }
        let path = entry.path();
        let removed = match entry.file_type().await {
            Ok(t) if t.is_dir() => tokio::fs::remove_dir_all(&path).await,
            _ => tokio::fs::remove_file(&path).await,
        };
        if let Err(err) = removed {
            tracing::warn!(path = %path.display(), "can't remove a background file: {err}");
        }
    }
}

/// Removes `dir` and what is in it; logged when it fails, quiet when it is
/// not there.
async fn remove_dir(dir: &Path) {
    if let Err(err) = tokio::fs::remove_dir_all(dir).await
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), "can't remove a background directory: {err}");
    }
}
