use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde_json::Value;

/// Status values stored in the `blocks.status` column.
pub const STATUS_EXECUTED: &str = "executed";
pub const STATUS_FAILED: &str = "failed";

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Record a block. Returns the row id, or an error if the insert fails.
/// Callers should treat recording as best-effort — losing the block must not
/// fail the underlying FS operation.
pub fn record(
    conn: &Connection,
    kind: &str,
    payload: &Value,
    inverse_payload: Option<&Value>,
    status: &str,
    user_query: Option<&str>,
    error: Option<&str>,
) -> rusqlite::Result<i64> {
    let now = now_ts();
    conn.execute(
        "INSERT INTO blocks (kind, payload, inverse_payload, status, user_query, error, created_at, executed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            kind,
            payload.to_string(),
            inverse_payload.map(|v| v.to_string()),
            status,
            user_query,
            error,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Best-effort recorder: opens the default DB, writes the block, logs on
/// failure but never panics or propagates. Use this inside FS-op functions
/// where the user-facing operation must succeed even if block recording fails.
pub fn record_best_effort(
    kind: &str,
    payload: &Value,
    inverse_payload: Option<&Value>,
    status: &str,
) {
    match crate::db::open_default() {
        Ok(conn) => {
            if let Err(e) = record(&conn, kind, payload, inverse_payload, status, None, None) {
                eprintln!("⚠️  block recording failed ({}): {}", kind, e);
            }
        }
        Err(e) => {
            eprintln!("⚠️  block DB open failed ({}): {}", kind, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn fresh() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocks.sqlite3");
        let conn = db::open(&path).unwrap();
        std::mem::forget(dir);
        conn
    }

    #[test]
    fn records_with_inverse() {
        let conn = fresh();
        let payload = serde_json::json!({"moves": [{"from": "/a", "to": "/b/a"}]});
        let inverse = serde_json::json!({"moves": [{"from": "/b/a", "to": "/a"}]});
        let id = record(
            &conn,
            "moveFiles",
            &payload,
            Some(&inverse),
            STATUS_EXECUTED,
            Some("move a to b"),
            None,
        )
        .unwrap();
        assert!(id > 0);

        let (kind, status, query, inv): (String, String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT kind, status, user_query, inverse_payload FROM blocks WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "moveFiles");
        assert_eq!(status, "executed");
        assert_eq!(query.as_deref(), Some("move a to b"));
        assert!(inv.unwrap().contains("\"from\":\"/b/a\""));
    }

    #[test]
    fn records_failure_with_error_and_no_inverse() {
        let conn = fresh();
        let payload = serde_json::json!({"sources": ["/missing"]});
        let id = record(
            &conn,
            "trashFiles",
            &payload,
            None,
            STATUS_FAILED,
            None,
            Some("no such file"),
        )
        .unwrap();

        let (status, inv, err): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, inverse_payload, error FROM blocks WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert!(inv.is_none());
        assert_eq!(err.as_deref(), Some("no such file"));
    }
}
