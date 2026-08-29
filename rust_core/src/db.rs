use std::env;
use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;

use crate::schema;

/// Default on-disk location for the index database.
/// Sibling of the legacy `~/.fast-finder-cache.json`; we keep it under a
/// dedicated dir so WAL/shm sidecars don't pollute $HOME.
///
/// `FAST_FINDER_DB_PATH` overrides for tests and dev sandboxing — keep this
/// hatch even in release so users can relocate the index without recompiling.
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = env::var("FAST_FINDER_DB_PATH") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(format!("{}/.fast-finder/index.sqlite3", home))
}

pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    schema::apply(&mut conn)?;
    // The index contains a listing of every file the user has — treat it
    // as private. 0600 means owner-only read/write. Best-effort: on
    // permission failure we still return the connection.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            let mut perms = meta.permissions();
            if perms.mode() & 0o777 != 0o600 {
                perms.set_mode(0o600);
                let _ = fs::set_permissions(path, perms);
            }
        }
    }
    Ok(conn)
}

pub fn open_default() -> rusqlite::Result<Connection> {
    open(&default_db_path())
}

/// Read a value from the `settings` key/value table. Missing key → None.
pub fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get(0),
    )
    .ok()
}

/// Write a value into the `settings` table, replacing any existing entry.
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    // WAL: concurrent reads while indexer writes; survives crashes cleanly.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // NORMAL is the right durability/perf point for a derived index.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Larger cache helps the hot path (search) avoid spilling to disk.
    conn.pragma_update(None, "cache_size", -64_000)?; // 64MB
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    // WAL still allows only one writer at a time, and the incremental indexer
    // can collide with a full rescan. Without a timeout the loser gets an
    // immediate SQLITE_BUSY and the update is silently dropped.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite3");
        // Leak the tempdir so the connection outlives this fn in tests that
        // only need the conn handle. For round-trip tests we keep the dir.
        let conn = open(&path).unwrap();
        std::mem::forget(dir);
        conn
    }

    #[test]
    fn migrations_apply_cleanly() {
        let conn = fresh_conn();
        // Every expected table should exist after migration.
        for table in ["files", "file_signals", "blocks", "embeddings", "settings", "files_fts"] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing table: {}", table);
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idem.sqlite3");
        let _ = open(&path).unwrap();
        // Re-opening must not error or duplicate-create.
        let _ = open(&path).unwrap();
    }

    #[test]
    fn fts_triggers_mirror_files() {
        let conn = fresh_conn();
        conn.execute(
            "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, ctime, file_kind, indexed_at)
             VALUES ('/tmp/Report Q3.pdf', 'Report Q3.pdf', '/tmp', 'pdf', 1024, 0, 100, 100, 'PDF Document', 100)",
            [],
        ).unwrap();

        // FTS5 should find it via name token.
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files_fts WHERE files_fts MATCH 'Report'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        // Update should re-mirror.
        conn.execute(
            "UPDATE files SET name = 'Report Q4.pdf', path = '/tmp/Report Q4.pdf' WHERE path = '/tmp/Report Q3.pdf'",
            [],
        ).unwrap();
        let q3: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files_fts WHERE files_fts MATCH 'Q3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let q4: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files_fts WHERE files_fts MATCH 'Q4'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(q3, 0);
        assert_eq!(q4, 1);

        // Delete should clean the FTS row.
        conn.execute("DELETE FROM files WHERE path = '/tmp/Report Q4.pdf'", []).unwrap();
        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files_fts WHERE files_fts MATCH 'Q4'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, 0);
    }

    #[test]
    fn settings_round_trip_and_overwrite() {
        let conn = fresh_conn();
        assert_eq!(get_setting(&conn, "missing"), None);

        set_setting(&conn, "last_event_id", "42").unwrap();
        assert_eq!(get_setting(&conn, "last_event_id").as_deref(), Some("42"));

        // Second write for the same key replaces rather than erroring on the PK.
        set_setting(&conn, "last_event_id", "99").unwrap();
        assert_eq!(get_setting(&conn, "last_event_id").as_deref(), Some("99"));
    }

    #[test]
    fn blocks_round_trip_with_inverse() {
        let conn = fresh_conn();
        conn.execute(
            "INSERT INTO blocks (kind, payload, inverse_payload, status, created_at)
             VALUES ('moveFiles', '{\"sources\":[\"/a\"],\"dest\":\"/b\"}', '{\"sources\":[\"/b/a\"],\"dest\":\"/\"}', 'executed', 1000)",
            [],
        ).unwrap();
        let (kind, inverse): (String, String) = conn
            .query_row(
                "SELECT kind, inverse_payload FROM blocks WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "moveFiles");
        assert!(inverse.contains("\"sources\""));
    }
}
