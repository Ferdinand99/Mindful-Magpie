//! Magpie's migration set. The pool, the runner and the core tables (users,
//! sessions, API tokens, settings) are vole-kit's.

use rusqlite::Connection;
use vole_kit::db::migrate::{self, MigrationError, MigrationSet};

/// Magpie's own migrations, sorted by name. They run after the kit's core set.
const MIGRATION_FILES: &[(&str, &str)] = &[
    ("0001_init.sql", include_str!("../migrations/0001_init.sql")),
    (
        "0002_layouts.sql",
        include_str!("../migrations/0002_layouts.sql"),
    ),
    (
        "0003_extension_codes.sql",
        include_str!("../migrations/0003_extension_codes.sql"),
    ),
    (
        "0004_backgrounds.sql",
        include_str!("../migrations/0004_backgrounds.sql"),
    ),
    (
        "0005_kept_background.sql",
        include_str!("../migrations/0005_kept_background.sql"),
    ),
    (
        "0006_services.sql",
        include_str!("../migrations/0006_services.sql"),
    ),
    (
        "0007_snapshots.sql",
        include_str!("../migrations/0007_snapshots.sql"),
    ),
    ("0008_acks.sql", include_str!("../migrations/0008_acks.sql")),
    (
        "0009_personal_services.sql",
        include_str!("../migrations/0009_personal_services.sql"),
    ),
    (
        "0010_history.sql",
        include_str!("../migrations/0010_history.sql"),
    ),
];

fn migrations() -> Vec<(String, String)> {
    MIGRATION_FILES
        .iter()
        .map(|(name, sql)| ((*name).to_string(), (*sql).to_string()))
        .collect()
}

fn no_fixups(_: &mut Connection) -> Result<(), MigrationError> {
    Ok(())
}

/// The kit's core set first, then Magpie's (a fresh product: nothing to adopt).
pub static MIGRATIONS: MigrationSet = MigrationSet {
    core: migrate::CORE,
    adopt_core: None,
    migrations,
    fk_off_marker: "-- mm:foreign_keys=off",
    baseline: None,
    after: no_fixups,
};
