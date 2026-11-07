//! The user's uploaded start page background: one picture per user.
//!
//! The row in `backgrounds` holds the metadata and is the truth about which
//! picture is the user's; the file lives at
//! `<data_dir>/backgrounds/<user_id>/<id>.<ext>`. `<id>` is a prefix of the
//! SHA-256 of the content, so the same picture always has the same name and a
//! browser can cache it for good.
//!
//! Order of events, so that a failure never leaves a row without a file:
//!
//! - an upload writes the file (a temporary file, then a rename), commits the
//!   row, and only then deletes everything else in the user's directory (the
//!   previous picture, leftovers of a crash);
//! - a delete commits the row (and the layout change) first, then removes the
//!   directory;
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
}

/// The file's name inside the user's directory.
fn file_name(id: &str, kind: Kind) -> String {
    format!("{id}.{}", kind.ext())
}

/// The place of the backgrounds, and the lock that keeps their changes in order.
#[derive(Debug)]
pub struct Backgrounds {
    root: PathBuf,
    lock: Mutex<()>,
}

impl Backgrounds {
    /// The backgrounds of the Magpie whose data lives in `data_dir`.
    pub fn new(data_dir: &Path) -> Backgrounds {
        Backgrounds {
            root: data_dir.join("backgrounds"),
            lock: Mutex::new(()),
        }
    }

    fn user_dir(&self, user_id: i64) -> PathBuf {
        self.root.join(user_id.to_string())
    }

    /// Keeps `data` (a picture of `kind`) as the user's background and returns
    /// its id. The previous picture is gone once this returns.
    pub async fn store(
        &self,
        core: &Core,
        user_id: i64,
        kind: Kind,
        data: &[u8],
    ) -> Result<String, BackgroundError> {
        let _guard = self.lock.lock().await;
        let id = image_id(data);
        let name = file_name(&id, kind);
        let dir = self.user_dir(user_id);
        write_atomically(&dir, &name, data).await?;

        let (row_id, mime) = (id.clone(), kind.mime());
        let size = i64::try_from(data.len()).unwrap_or(i64::MAX);
        let now = jiff::Timestamp::now().to_string();
        let stored = db::interact(&core.db, move |c| {
            c.execute(
                "INSERT INTO backgrounds (user_id, id, mime, bytes, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(user_id) DO UPDATE SET
                     id = excluded.id, mime = excluded.mime,
                     bytes = excluded.bytes, created_at = excluded.created_at",
                params![user_id, row_id, mime, size, now],
            )
        })
        .await;
        // Whether or not the row is in, what the row names is the one file to
        // keep: after a failure that is the previous picture.
        let keep = match &stored {
            Ok(_) => Some(name),
            Err(_) => self.row_file(core, user_id).await,
        };
        match keep {
            Some(keep) => prune(&dir, &keep).await,
            None => remove_dir(&dir).await,
        }
        stored?;
        Ok(id)
    }

    /// The user's picture when `id` is its id: its kind and bytes. `None` when
    /// the user has no picture, another id, or the file is missing.
    pub async fn read(
        &self,
        core: &Core,
        user_id: i64,
        id: &str,
    ) -> Result<Option<(Kind, Vec<u8>)>, BackgroundError> {
        let row = meta(core, user_id).await?;
        let Some((stored, mime)) = row else {
            return Ok(None);
        };
        let Some(kind) = Kind::from_mime(&mime).filter(|_| stored == id) else {
            return Ok(None);
        };
        let path = self.user_dir(user_id).join(file_name(&stored, kind));
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some((kind, data))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::warn!(user = user_id, "a background row has no file");
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Removes the user's picture. When the user's layout uses it, the layout
    /// goes back to the default preset (and its version moves on, so a tab
    /// with the old copy gets a conflict). Returns the layout's stored version
    /// (0 when none is stored).
    pub async fn remove(&self, core: &Core, user_id: i64) -> Result<u64, BackgroundError> {
        let _guard = self.lock.lock().await;
        let version = db::interact(&core.db, move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "DELETE FROM backgrounds WHERE user_id = ?1",
                params![user_id],
            )?;
            let row: Option<(i64, String)> = tx
                .query_row(
                    "SELECT version, doc FROM layouts WHERE user_id = ?1",
                    params![user_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((version, doc)) = row else {
                tx.commit()?;
                return Ok(0);
            };
            let mut version = u64::try_from(version).unwrap_or_default();
            // A layout that cannot be read is left as it is (the page offers
            // to start over); one on a preset keeps the preset.
            if let Ok(mut layout) = serde_json::from_str::<Layout>(&doc)
                && matches!(layout.background, Background::Image { .. })
            {
                version += 1;
                layout.version = version;
                layout.background = Layout::default().background;
                let doc = serde_json::to_string(&layout)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                tx.execute(
                    "UPDATE layouts SET version = ?2, doc = ?3, updated_at = ?4
                     WHERE user_id = ?1",
                    params![
                        user_id,
                        i64::try_from(version).unwrap_or(i64::MAX),
                        doc,
                        jiff::Timestamp::now().to_string()
                    ],
                )?;
            }
            tx.commit()?;
            Ok(version)
        })
        .await?;
        remove_dir(&self.user_dir(user_id)).await;
        Ok(version)
    }

    /// Removes the files no row accounts for: the directory of a user who has
    /// no picture (the user is gone, or the picture was removed), and in a
    /// directory that has one, every file but the picture's. Entries that are
    /// not a user's number are not ours and stay.
    pub async fn reconcile(&self, core: &Core) -> Result<(), BackgroundError> {
        let _guard = self.lock.lock().await;
        let rows: HashMap<i64, String> = db::interact(&core.db, |c| {
            let mut stmt = c.prepare("SELECT user_id, id, mime FROM backgrounds")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut keep = HashMap::new();
            for row in rows {
                let (user_id, id, mime) = row?;
                if let Some(kind) = Kind::from_mime(&mime) {
                    keep.insert(user_id, file_name(&id, kind));
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

    /// The file the user's row names, if there is a row.
    async fn row_file(&self, core: &Core, user_id: i64) -> Option<String> {
        let (id, mime) = meta(core, user_id).await.ok().flatten()?;
        Some(file_name(&id, Kind::from_mime(&mime)?))
    }
}

/// The user's row: id and mime type.
async fn meta(core: &Core, user_id: i64) -> Result<Option<(String, String)>, DbError> {
    db::interact(&core.db, move |c| {
        c.query_row(
            "SELECT id, mime FROM backgrounds WHERE user_id = ?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
    })
    .await
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

/// Removes everything in `dir` but the file `keep`. Failures are logged: a
/// file that stays is found by the next sweep.
async fn prune(dir: &Path, keep: &str) {
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
        if entry.file_name() == keep {
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
