//! `/api/memory/recent` — most-recently-created memory drawers for the
//! live feed's "new entries" column.
//!
//! Uses raw SQL against the memory db (read-only flags) rather than
//! `Palace::list_drawers` because we don't want the schema-bootstrap
//! side effects of `Palace::open` on every poll.

use crate::query::url_decode;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
pub(crate) struct MemoryRecentSnapshot {
    present: bool,
    cursor: i64,
    drawers: Vec<DrawerOut>,
}

#[derive(Serialize)]
struct DrawerOut {
    id: i64,
    wing: String,
    room: Option<String>,
    source_id: String,
    body_preview: String,
    created_at: i64,
}

pub(crate) fn memory_recent(root: &Path, query: &str) -> Result<MemoryRecentSnapshot> {
    let mut since: i64 = 0;
    let mut limit: usize = 20;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = url_decode(v);
        match k {
            "since" => since = v.parse().unwrap_or_default(),
            "limit" => limit = v.parse::<usize>().unwrap_or(20).clamp(1, 200),
            _ => {}
        }
    }
    // Same path resolution as `bootstrap_snapshot` — see #479. The
    // legacy `.crabcc/memory.db` is also checked as a fallback for
    // installs that haven't run the migrating `Palace::open` yet.
    let memory_path = crabcc_memory::resolve_db_path(root)
        .unwrap_or_else(|_| root.join(".crabcc").join("memory.db"));
    if !memory_path.exists() {
        return Ok(MemoryRecentSnapshot {
            present: false,
            cursor: since,
            drawers: vec![],
        });
    }
    let conn = rusqlite::Connection::open_with_flags(
        &memory_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    // The drawer body can be huge; preview only the first ~240 chars
    // for the live feed. Clients that want the full body call
    // `crabcc memory get <id>` (a separate, more expensive path).
    // The drawers schema uses FKs to `wings` + `rooms` (not flat columns),
    // so we LEFT JOIN to surface human-readable names. body_enc != 0
    // means FSST-compressed; we skip those rows in the preview because
    // decoding requires the codec from `~/.crabcc/fsst.symbols` and we
    // don't want the live feed to depend on optional sidecars. The
    // count line above already includes them, so the preview just
    // shows fewer rows than `count` when compression is on — that's
    // fine for a live dashboard.
    let mut stmt = conn.prepare(
        "SELECT d.id, w.name, r.name, d.source_id, substr(d.body, 1, 240), d.created_at \
         FROM drawers d \
         LEFT JOIN wings w ON w.id = d.wing_id \
         LEFT JOIN rooms r ON r.id = d.room_id \
         WHERE d.created_at > ?1 AND d.body_enc = 0 \
         ORDER BY d.created_at DESC \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![since, limit as i64], |r| {
        Ok(DrawerOut {
            id: r.get(0)?,
            wing: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "?".into()),
            room: r.get::<_, Option<String>>(2)?,
            source_id: r.get(3)?,
            body_preview: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    let mut drawers: Vec<DrawerOut> = rows.filter_map(|r| r.ok()).collect();
    let cursor = drawers.iter().map(|d| d.created_at).max().unwrap_or(since);
    // Reverse so the JSON is oldest-first within the page; the
    // frontend prepends each event to its list which gives the user
    // the natural "newest at top" ordering after concatenation.
    drawers.reverse();
    Ok(MemoryRecentSnapshot {
        present: true,
        cursor,
        drawers,
    })
}
