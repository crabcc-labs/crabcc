//! Auto-creating helpers for the `wings` / `rooms` rows referenced by
//! every drawer insert, plus the once-per-process `sqlite-vec` extension
//! registration.

use super::encoding::now_secs;
use anyhow::Result;
use rusqlite::{params, Connection};

/// Register the bundled `sqlite-vec` C extension as a SQLite auto-extension
/// so every subsequent `Connection::open` picks it up. Once-only per process —
/// `sqlite3_auto_extension` is cumulative; calling it twice would install the
/// same entry point twice. `Once::call_once` guarantees the body runs at most
/// once, so this helper is safe to call from every `Backend::open`.
///
/// v2.5.1 (#17) — extension registration; the `drawers_vec` virtual table is
/// created in `Backend::open` (gated `IF NOT EXISTS`).
#[cfg(feature = "memory-vec")]
pub(super) fn register_sqlite_vec_once() {
    use std::sync::Once;
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        // Safety: `sqlite_vec::sqlite3_vec_init` is the C entry point of the
        // bundled sqlite-vec extension. Its real C signature matches the
        // `sqlite3_auto_extension` contract; the Rust binding declares it
        // zero-arg, so we transmute through `*const ()` to the explicit
        // SQLite extension entry-point type. Same pattern as the upstream
        // sqlite-vec Rust binding's own test, with the explicit type
        // annotation clippy's `missing_transmute_annotations` lint requires.
        type SqliteExtInit = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                SqliteExtInit,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

pub(super) fn ensure_wing(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO wings(name, kind, created_at) VALUES (?1, 'project', ?2)",
        params![name, now_secs()],
    )?;
    let id: i64 = conn.query_row("SELECT id FROM wings WHERE name = ?1", params![name], |r| {
        r.get(0)
    })?;
    Ok(id)
}

pub(super) fn ensure_room(conn: &Connection, wing_id: i64, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO rooms(wing_id, name) VALUES (?1, ?2)",
        params![wing_id, name],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM rooms WHERE wing_id = ?1 AND name = ?2",
        params![wing_id, name],
        |r| r.get(0),
    )?;
    Ok(id)
}
