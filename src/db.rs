//! Magpie's migration set. The pool, the runner and the core tables (users,
//! sessions, API tokens, settings) are vole-kit's.

use rusqlite::Connection;
use vole_kit::db::migrate::{self, MigrationError, MigrationSet};

/// Magpie's own migrations, sorted by name. They run after the kit's core set.
const MIGRATION_FILES: &[(&str, &str)] =
    &[("0001_init.sql", include_str!("../migrations/0001_init.sql"))];

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
