use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use ignore::WalkBuilder;
use rusqlite::params;
use serde::{Deserialize, Serialize};

mod blocks;
mod db;
mod safety;
mod schema;

uniffi::setup_scaffolding!();

#[derive(uniffi::Record, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub file_name: String,
    pub file_path: String,
    pub file_size: u64,
    pub is_folder: bool,
    pub score: i64,
    pub date_value: i64,
    pub date_kind: String,
    pub file_kind: String,
    #[serde(default)]
    pub pretty_date: String,  // Pre-formatted relative date
}

// Format relative date in Rust (faster than Swift UI thread)
fn format_relative_date(timestamp: i64) -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    
    let diff = now - timestamp;
    
    if diff < 60 { return "Just now".to_string(); }
    if diff < 3600 { return format!("{}m ago", diff / 60); }
    if diff < 86400 { return format!("{}h ago", diff / 3600); }
    if diff < 604800 { return format!("{}d ago", diff / 86400); }
    if diff < 2592000 { return format!("{}w ago", diff / 604800); }
    if diff < 31536000 { return format!("{}mo ago", diff / 2592000); }
    format!("{}y ago", diff / 31536000)
}

/// Row → SearchResult mapper used by every read path.
fn map_row(row: &rusqlite::Row) -> rusqlite::Result<SearchResult> {
    let date_value: i64 = row.get("date_value")?;
    Ok(SearchResult {
        file_name: row.get("name")?,
        file_path: row.get("path")?,
        file_size: row.get::<_, i64>("size")? as u64,
        is_folder: row.get::<_, i64>("is_dir")? != 0,
        score: row.get::<_, Option<i64>>("score")?.unwrap_or(date_value),
        date_value,
        date_kind: row.get("date_kind")?,
        file_kind: row.get("file_kind")?,
        pretty_date: format_relative_date(date_value),
    })
}

// Helper to get file kind from extension
fn get_file_kind(path: &std::path::Path, is_folder: bool) -> String {
    if is_folder {
        return "Folder".to_string();
    }
    
    match path.extension().and_then(|e| e.to_str()) {
        Some("pdf") => "PDF Document",
        Some("doc") | Some("docx") => "Word Document",
        Some("xls") | Some("xlsx") => "Excel Spreadsheet",
        Some("ppt") | Some("pptx") => "Presentation",
        Some("txt") => "Plain Text",
        Some("md") => "Markdown",
        Some("html") | Some("htm") => "HTML Document",
        Some("css") => "CSS Stylesheet",
        Some("js") => "JavaScript",
        Some("ts") => "TypeScript",
        Some("json") => "JSON",
        Some("py") => "Python Script",
        Some("rs") => "Rust Source",
        Some("swift") => "Swift Source",
        Some("java") => "Java Source",
        Some("go") => "Go Source",
        Some("c") | Some("h") => "C Source",
        Some("cpp") | Some("hpp") => "C++ Source",
        Some("jpg") | Some("jpeg") => "JPEG Image",
        Some("png") => "PNG Image",
        Some("gif") => "GIF Image",
        Some("heic") => "HEIC Image",
        Some("svg") => "SVG Image",
        Some("mp4") => "MP4 Video",
        Some("mov") => "QuickTime Movie",
        Some("mp3") => "MP3 Audio",
        Some("wav") => "WAV Audio",
        Some("zip") => "ZIP Archive",
        Some("dmg") => "Disk Image",
        Some("app") => "Application",
        Some(ext) => return format!("{} File", ext.to_uppercase()),
        None => "Document",
    }.to_string()
}

// Only use mtime and ctime (atime is unreliable on macOS)
fn get_best_date(metadata: &std::fs::Metadata) -> (i64, &'static str) {
    let (mtime, ctime) = extract_times(metadata);
    if ctime > mtime {
        (ctime, "Created")
    } else {
        (mtime, "Modified")
    }
}

/// Raw (mtime, ctime) extraction. Storing both lets the ranking layer reason
/// about "created in the last week" vs "modified yesterday" — losing one
/// to a tiebreak (as the old code did) would degrade future ranking signals.
fn extract_times(metadata: &std::fs::Metadata) -> (i64, i64) {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let ctime = metadata
        .created()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (mtime, ctime)
}

// ============== INDEXING POLICY ==============
//
// The full rescan and the incremental updater MUST agree on what belongs in
// the index. If they diverge, `index_paths` will happily insert rows that the
// next `rebuild_index` prunes (or vice versa) and the index starts flickering.
// Everything that decides "is this path in scope" lives in this section.

/// Directory depth below a scan root that we index, matching the walker's
/// `max_depth`. Root itself is depth 0, its children depth 1.
const MAX_SCAN_DEPTH: usize = 5;

/// Folders the indexer walks.
fn scan_roots() -> Vec<String> {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    vec![
        format!("{}/Documents", home),
        format!("{}/Downloads", home),
        format!("{}/Desktop", home),
    ]
}

fn allowed_extensions() -> &'static HashSet<&'static str> {
    static EXTS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    EXTS.get_or_init(|| {
        [
            "pdf", "doc", "docx", "txt", "rtf", "md", "pages", "odt",
            "xls", "xlsx", "csv", "numbers",
            "ppt", "pptx", "key",
            "jpg", "jpeg", "png", "gif", "heic", "webp", "svg", "psd", "ai",
            "mp4", "mov", "avi", "mkv", "webm",
            "mp3", "wav", "aac", "flac", "m4a",
            "py", "js", "ts", "rs", "swift", "java", "go", "html", "css", "json",
            "zip", "tar", "gz", "rar", "7z", "dmg",
        ]
        .into_iter()
        .collect()
    })
}

/// Extension policy, mirroring the filter inside the parallel walker:
///   - anything with an extension must be on the allow-list (this also drops
///     oddly-named directories like `my.backup`, same as the walker)
///   - a file with no extension is skipped; a directory with none is kept
fn extension_allows(path: &Path, is_dir: bool) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => allowed_extensions().contains(ext.to_lowercase().as_str()),
        None => is_dir,
    }
}

/// The scan root containing `path`, if any. Returned so callers can compute
/// depth and scope prune statements to a single root.
fn scan_root_for(path: &Path) -> Option<String> {
    scan_roots()
        .into_iter()
        .find(|root| path.starts_with(root))
}

/// Full eligibility check for a single path: inside a scan root, within the
/// depth limit, not hidden, and extension-allowed.
///
/// Note: the full rescan additionally honours `.gitignore` via `WalkBuilder`.
/// We deliberately don't reimplement that for single-file events — the cost of
/// a stray indexed row is far lower than the cost of getting ignore-file
/// semantics subtly wrong in two places. Directory events re-walk through
/// `WalkBuilder`, so subtrees stay exact.
fn is_indexable(path: &Path, is_dir: bool) -> bool {
    let Some(root) = scan_root_for(path) else {
        return false;
    };
    let Ok(rel) = path.strip_prefix(&root) else {
        return false;
    };
    let mut depth = 0usize;
    for component in rel.components() {
        depth += 1;
        let name = component.as_os_str().to_string_lossy();
        if name.starts_with('.') {
            return false;
        }
    }
    if depth > MAX_SCAN_DEPTH {
        return false;
    }
    extension_allows(path, is_dir)
}

/// Build the FFI record plus the raw (mtime, ctime) we persist alongside it.
fn record_for(path: &Path, metadata: &fs::Metadata) -> (SearchResult, i64, i64) {
    let is_folder = metadata.is_dir();
    let (mtime, ctime) = extract_times(metadata);
    let (date_value, date_kind) = get_best_date(metadata);
    (
        SearchResult {
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            file_path: path.to_string_lossy().into_owned(),
            file_size: metadata.len(),
            is_folder,
            score: date_value,
            date_value,
            date_kind: date_kind.to_string(),
            file_kind: get_file_kind(path, is_folder),
            pretty_date: format_relative_date(date_value),
        },
        mtime,
        ctime,
    )
}

/// Matches every row strictly beneath the directory bound to `?1`.
///
/// Deliberately not `path LIKE ?1 || '/%'`: `%` and `_` are ordinary,
/// common characters in filenames but SQL wildcards in LIKE, so a folder
/// named `tax_2025` would also match `taxX2025` and a prune would delete
/// index rows for unrelated files. A range over the `path` unique index is
/// exact — every descendant sorts between `dir/` and `dir0`, because '0' is
/// the next code point after '/'.
const DESCENDANTS_OF: &str = "path > ?1 || '/' AND path < ?1 || '0'";

/// UPSERT on `path` so file ids stay stable across reindexes — `blocks` and
/// `embeddings` FK into `files(id)` and must not be orphaned by a rescan.
const UPSERT_FILE_SQL: &str = "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, ctime, file_kind, indexed_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
     ON CONFLICT(path) DO UPDATE SET
         name       = excluded.name,
         parent_dir = excluded.parent_dir,
         ext        = excluded.ext,
         size       = excluded.size,
         is_dir     = excluded.is_dir,
         mtime      = excluded.mtime,
         ctime      = excluded.ctime,
         file_kind  = excluded.file_kind,
         indexed_at = excluded.indexed_at";

/// Bind one record to a prepared `UPSERT_FILE_SQL` statement.
fn upsert_record(
    stmt: &mut rusqlite::Statement,
    r: &SearchResult,
    mtime: i64,
    ctime: i64,
    indexed_at: i64,
) -> rusqlite::Result<usize> {
    let p = Path::new(&r.file_path);
    let parent = p
        .parent()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase());
    stmt.execute(params![
        r.file_path,
        r.file_name,
        parent,
        ext,
        r.file_size as i64,
        r.is_folder as i64,
        mtime,
        ctime,
        r.file_kind,
        indexed_at,
    ])
}

/// Walk one directory subtree, collecting every indexable entry.
fn collect_subtree(root: &str, max_depth: usize) -> Vec<(SearchResult, i64, i64)> {
    let collected: Arc<Mutex<Vec<(SearchResult, i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));

    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .max_depth(Some(max_depth))
        .threads(4)
        .build_parallel();

    let sink = collected.clone();
    walker.run(move || {
        let sink = sink.clone();
        Box::new(move |entry_result| {
            if let Ok(entry) = entry_result {
                let path = entry.path();
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                if !extension_allows(path, is_dir) {
                    return ignore::WalkState::Continue;
                }
                if let Ok(metadata) = entry.metadata() {
                    if let Ok(mut lock) = sink.lock() {
                        lock.push(record_for(path, &metadata));
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });

    let out = collected.lock().map(|l| l.clone()).unwrap_or_default();
    out
}

/// Load the persisted index for instant startup.
/// Returns rows ordered by best-date desc; capped to 50k to keep the FFI
/// crossing cheap. If the DB isn't openable yet (first launch race), returns [].
#[uniffi::export]
pub fn load_cached_index() -> Vec<SearchResult> {
    let conn = match db::open_default() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn.prepare(
        "SELECT path, name, size, is_dir, file_kind,
                MAX(mtime, ctime) AS date_value,
                CASE WHEN ctime > mtime THEN 'Created' ELSE 'Modified' END AS date_kind,
                MAX(mtime, ctime) AS score
         FROM files
         ORDER BY date_value DESC
         LIMIT 50000",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], map_row)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Get a compact file listing for AI context (saves tokens)
#[uniffi::export]
pub fn get_file_listing_for_ai(path: String) -> String {
    use std::fs;
    
    let mut files: Vec<serde_json::Value> = Vec::new();
    
    if let Ok(entries) = fs::read_dir(&path) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        
        for entry in entries.filter_map(|e| e.ok()).take(100) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            
            if let Ok(meta) = entry.metadata() {
                let is_dir = meta.is_dir();
                let size = if is_dir { 0 } else { meta.len() };
                let kind = get_file_kind(&path, is_dir);
                
                let (date_val, _) = get_best_date(&meta);
                let age_days = (now - date_val) / 86400;
                
                let full_path = path.to_string_lossy().into_owned();
                files.push(serde_json::json!({
                    "name": name,
                    "path": full_path,
                    "size": size,
                    "kind": kind,
                    "age_days": age_days
                }));
            }
        }
    }
    
    serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string())
}

/// Full rescan of every scan root. Expensive: this walks the whole tree, so
/// the app should call it on first run or when the incremental event stream
/// has gone stale — `index_paths` handles the steady state.
#[uniffi::export]
pub fn rebuild_index() -> Vec<SearchResult> {
    let scan_folders = scan_roots();

    let mut collected: Vec<(SearchResult, i64, i64)> = Vec::new();
    for folder in &scan_folders {
        if !Path::new(folder).exists() {
            continue;
        }
        collected.extend(collect_subtree(folder, MAX_SCAN_DEPTH));
    }
    collected.sort_by(|a, b| b.0.date_value.cmp(&a.0.date_value));

    if let Ok(mut conn) = db::open_default() {
        let now = blocks::now_ts();
        if let Ok(tx) = conn.transaction() {
            {
                if let Ok(mut stmt) = tx.prepare(UPSERT_FILE_SQL) {
                    for (r, mtime, ctime) in &collected {
                        let _ = upsert_record(&mut stmt, r, *mtime, *ctime, now);
                    }
                }

                // Tombstone-prune: any row under a scan root whose indexed_at
                // is older than this run was not re-visited and must have been
                // removed/renamed/moved. CASCADE drops dependent rows.
                // Range rather than LIKE, for the reason on DESCENDANTS_OF.
                if let Ok(mut prune) = tx.prepare(
                    "DELETE FROM files
                     WHERE indexed_at < ?1
                       AND (path > ?2 || '/' AND path < ?2 || '0')",
                ) {
                    for folder in &scan_folders {
                        let _ = prune.execute(params![now, folder]);
                    }
                }
            }
            let _ = tx.commit();
        }
        // Stamp the completion so the app can tell whether the incremental
        // stream is still trustworthy on next launch.
        let _ = db::set_setting(&conn, SETTING_LAST_FULL_SCAN, &now.to_string());
    }

    collected.into_iter().map(|(sr, _, _)| sr).collect()
}

// ============== INCREMENTAL INDEXING ==============

/// Settings keys backing the incremental pipeline.
const SETTING_LAST_FULL_SCAN: &str = "last_full_scan_at";
const SETTING_LAST_EVENT_ID: &str = "fsevents_last_id";

/// How long a full rescan stays trustworthy. Past this we assume the FSEvents
/// stream may have dropped something and re-walk from scratch.
const FULL_SCAN_MAX_AGE_SECS: i64 = 60 * 60 * 24;

/// What one incremental pass changed. Returned so the UI can skip a refresh
/// when nothing actually moved.
#[derive(uniffi::Record, Clone)]
pub struct IndexUpdate {
    pub upserted: u32,
    pub removed: u32,
}

impl IndexUpdate {
    fn empty() -> Self {
        IndexUpdate { upserted: 0, removed: 0 }
    }

    /// True when the pass was a no-op — FSEvents fires for plenty of paths we
    /// don't index, so this is the common case.
    pub fn is_noop(&self) -> bool {
        self.upserted == 0 && self.removed == 0
    }
}

/// Reconcile the index against a specific set of paths, as reported by
/// FSEvents. This is the steady-state path: it touches only what changed
/// instead of re-walking every scan root.
///
/// For each path:
///   - gone from disk → delete its row, plus every row beneath it (a deleted
///     or renamed directory takes its whole subtree with it)
///   - a directory → re-walk that subtree and tombstone-prune it, which also
///     covers the coalesced `MustScanSubDirs` events FSEvents sends under load
///   - a file → upsert if indexable, otherwise make sure it isn't lingering
///
/// Paths outside the scan roots are ignored, so the caller can forward the
/// raw event list without pre-filtering.
#[uniffi::export]
pub fn index_paths(paths: Vec<String>) -> IndexUpdate {
    if paths.is_empty() {
        return IndexUpdate::empty();
    }
    let Ok(mut conn) = db::open_default() else {
        return IndexUpdate::empty();
    };

    let now = blocks::now_ts();
    let mut upserted: u32 = 0;
    let mut removed: u32 = 0;

    let Ok(tx) = conn.transaction() else {
        return IndexUpdate::empty();
    };
    {
        let (Ok(mut upsert), Ok(mut delete_subtree), Ok(mut children_of)) = (
            tx.prepare(UPSERT_FILE_SQL),
            // `path = ?1` catches the entry itself; the range catches a
            // deleted directory's descendants, which FSEvents does not
            // enumerate. See DESCENDANTS_OF on why this isn't a LIKE.
            tx.prepare(&format!(
                "DELETE FROM files WHERE path = ?1 OR ({})",
                DESCENDANTS_OF
            )),
            tx.prepare(&format!("SELECT path FROM files WHERE {}", DESCENDANTS_OF)),
        ) else {
            return IndexUpdate::empty();
        };

        for raw in &paths {
            let path = Path::new(raw);
            if !path.is_absolute() || scan_root_for(path).is_none() {
                continue;
            }

            // symlink_metadata: a symlink is indexed as itself, and a dangling
            // one reads as "exists" rather than following into nothing.
            match fs::symlink_metadata(path) {
                Err(_) => {
                    removed += delete_subtree.execute(params![raw]).unwrap_or(0) as u32;
                }
                Ok(meta) if meta.is_dir() => {
                    if !is_indexable(path, true) {
                        removed += delete_subtree.execute(params![raw]).unwrap_or(0) as u32;
                        continue;
                    }
                    // Re-walk the subtree with the depth budget it would have
                    // had during a full scan, so a deep directory event can't
                    // pull in rows the full rescan would later prune.
                    let root_depth = scan_root_for(path)
                        .and_then(|root| path.strip_prefix(&root).ok().map(|r| r.components().count()))
                        .unwrap_or(0);
                    let budget = MAX_SCAN_DEPTH.saturating_sub(root_depth);

                    let found = collect_subtree(raw, budget);
                    let on_disk: HashSet<&str> =
                        found.iter().map(|(r, _, _)| r.file_path.as_str()).collect();

                    for (r, mtime, ctime) in &found {
                        if upsert_record(&mut upsert, r, *mtime, *ctime, now).is_ok() {
                            upserted += 1;
                        }
                    }

                    // Prune by diffing against what we just walked rather than
                    // by `indexed_at < now`: two events landing in the same
                    // wall-clock second would make a timestamp comparison skip
                    // genuinely stale rows.
                    let stale: Vec<String> = children_of
                        .query_map(params![raw], |r| r.get::<_, String>(0))
                        .map(|rows| {
                            rows.filter_map(|r| r.ok())
                                .filter(|p| !on_disk.contains(p.as_str()))
                                .collect()
                        })
                        .unwrap_or_default();
                    for p in stale {
                        removed += delete_subtree.execute(params![p]).unwrap_or(0) as u32;
                    }
                }
                Ok(meta) => {
                    if is_indexable(path, false) {
                        let (r, mtime, ctime) = record_for(path, &meta);
                        if upsert_record(&mut upsert, &r, mtime, ctime, now).is_ok() {
                            upserted += 1;
                        }
                    } else {
                        // Renamed into a non-indexed extension, or newly
                        // hidden — drop any row we were still holding.
                        removed += delete_subtree.execute(params![raw]).unwrap_or(0) as u32;
                    }
                }
            }
        }
    }
    let _ = tx.commit();

    IndexUpdate { upserted, removed }
}

/// True when the app should run a full `rebuild_index` instead of relying on
/// the incremental stream: nothing indexed yet, or the last full scan is old
/// enough that we can't trust having seen every event since.
#[uniffi::export]
pub fn needs_full_rescan() -> bool {
    let Ok(conn) = db::open_default() else {
        return true;
    };
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    if count == 0 {
        return true;
    }
    match db::get_setting(&conn, SETTING_LAST_FULL_SCAN).and_then(|v| v.parse::<i64>().ok()) {
        Some(last) => blocks::now_ts() - last > FULL_SCAN_MAX_AGE_SECS,
        None => true,
    }
}

/// Last FSEvents id we have already folded into the index, or 0 if none.
/// The app passes this back as the stream's `sinceWhen` so a relaunch replays
/// only what changed while it was closed instead of re-walking everything.
#[uniffi::export]
pub fn last_event_id() -> u64 {
    let Ok(conn) = db::open_default() else {
        return 0;
    };
    db::get_setting(&conn, SETTING_LAST_EVENT_ID)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Persist the FSEvents id reached after a successful `index_paths` pass.
#[uniffi::export]
pub fn set_last_event_id(id: u64) {
    if let Ok(conn) = db::open_default() {
        let _ = db::set_setting(&conn, SETTING_LAST_EVENT_ID, &id.to_string());
    }
}

/// Sanitize free-text user input into an FTS5 query: replace non-alphanumeric
/// chars with spaces (so `Q3.pdf` splits into `Q3 pdf`), lowercase to match
/// the unicode61 index, then `*`-suffix each token for prefix matching.
/// Lowercasing also neutralizes FTS5 keywords (`OR`, `AND`, `NOT`, `NEAR`)
/// which are uppercase-only. Empty / all-noise input returns None.
fn build_fts_query(raw: &str) -> Option<String> {
    let normalized: String = raw
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { ' ' })
        .collect();
    let tokens: Vec<String> = normalized
        .split_whitespace()
        .map(|t| format!("{}*", t))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

#[uniffi::export]
pub fn search_files(query: String) -> Vec<SearchResult> {
    let Some(fts) = build_fts_query(&query) else {
        return Vec::new();
    };
    let Ok(conn) = db::open_default() else {
        return Vec::new();
    };

    // bm25() returns negative numbers; lower = better. We invert into a
    // positive score so the existing Swift sort-by-score-desc behavior
    // continues to work. Recency is the second sort key for tie-breaks.
    let mut stmt = match conn.prepare(
        "SELECT f.path, f.name, f.size, f.is_dir, f.file_kind,
                MAX(f.mtime, f.ctime) AS date_value,
                CASE WHEN f.ctime > f.mtime THEN 'Created' ELSE 'Modified' END AS date_kind,
                CAST(-bm25(files_fts) * 1000 AS INTEGER) AS score
         FROM files_fts
         JOIN files f ON f.id = files_fts.rowid
         WHERE files_fts MATCH ?1
         ORDER BY score DESC, date_value DESC
         LIMIT 50",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    stmt.query_map(params![fts], map_row)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

#[uniffi::export]
pub fn get_recent_files() -> Vec<SearchResult> {
    let Ok(conn) = db::open_default() else {
        return Vec::new();
    };
    let week_ago = blocks::now_ts() - (60 * 60 * 24 * 7);

    let mut stmt = match conn.prepare(
        "SELECT path, name, size, is_dir, file_kind,
                MAX(mtime, ctime) AS date_value,
                CASE WHEN ctime > mtime THEN 'Created' ELSE 'Modified' END AS date_kind,
                MAX(mtime, ctime) AS score
         FROM files
         WHERE MAX(mtime, ctime) > ?1
           AND is_dir = 0
         ORDER BY date_value DESC
         LIMIT 50",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    stmt.query_map(params![week_ago], map_row)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

// ============== FILE OPERATIONS ==============

/// Result type for file operations
#[derive(uniffi::Record, Clone)]
pub struct FileOpResult {
    pub success: bool,
    pub message: String,
    pub affected_count: i32,
}

/// Move files to a destination folder
#[uniffi::export]
pub fn move_files(source_paths: Vec<String>, destination: String) -> FileOpResult {
    // Safety: refuse system paths, sensitive subtrees, oversize batches.
    if let Err((idx, rej)) = safety::check_sources(&source_paths) {
        let attempted = serde_json::json!({"sources": source_paths, "destination": destination});
        blocks::record_best_effort("moveFiles", &attempted, None, blocks::STATUS_FAILED);
        let target = source_paths.get(idx).map(|s| s.as_str()).unwrap_or("?");
        return FileOpResult {
            success: false,
            message: format!("Refused: {} ({})", rej.explain(), target),
            affected_count: 0,
        };
    }
    if let Err(rej) = safety::check_destination(&destination) {
        let attempted = serde_json::json!({"sources": source_paths, "destination": destination});
        blocks::record_best_effort("moveFiles", &attempted, None, blocks::STATUS_FAILED);
        return FileOpResult {
            success: false,
            message: format!("Refused destination: {}", rej.explain()),
            affected_count: 0,
        };
    }

    let dest_path = std::path::Path::new(&destination);

    if !dest_path.exists() {
        if let Err(e) = fs::create_dir_all(dest_path) {
            return FileOpResult {
                success: false,
                message: format!("Failed to create destination: {}", e),
                affected_count: 0,
            };
        }
    }

    let mut moves: Vec<(String, String)> = Vec::new();
    let mut errors = Vec::new();

    for src in &source_paths {
        let src_path = std::path::Path::new(src);
        if let Some(file_name) = src_path.file_name() {
            let dest_file = dest_path.join(file_name);
            let dest_str = dest_file.to_string_lossy().into_owned();
            match fs::rename(src_path, &dest_file) {
                Ok(_) => moves.push((src.clone(), dest_str)),
                Err(_) => {
                    if let Err(copy_err) = fs::copy(src_path, &dest_file) {
                        errors.push(format!("{}: {}", src, copy_err));
                    } else {
                        let _ = fs::remove_file(src_path);
                        moves.push((src.clone(), dest_str));
                    }
                }
            }
        }
    }

    let moved = moves.len() as i32;
    let payload = serde_json::json!({
        "moves": moves.iter().map(|(f, t)| serde_json::json!({"from": f, "to": t})).collect::<Vec<_>>()
    });
    let inverse = serde_json::json!({
        "moves": moves.iter().map(|(f, t)| serde_json::json!({"from": t, "to": f})).collect::<Vec<_>>()
    });
    let status = if errors.is_empty() { blocks::STATUS_EXECUTED } else { blocks::STATUS_FAILED };
    blocks::record_best_effort("moveFiles", &payload, Some(&inverse), status);

    FileOpResult {
        success: errors.is_empty(),
        message: if errors.is_empty() {
            format!("Moved {} files", moved)
        } else {
            format!("Moved {} files, {} errors: {}", moved, errors.len(), errors.join("; "))
        },
        affected_count: moved,
    }
}

/// Copy files to a destination folder
#[uniffi::export]
pub fn copy_files(source_paths: Vec<String>, destination: String) -> FileOpResult {
    if let Err((idx, rej)) = safety::check_sources(&source_paths) {
        let attempted = serde_json::json!({"sources": source_paths, "destination": destination});
        blocks::record_best_effort("copyFiles", &attempted, None, blocks::STATUS_FAILED);
        let target = source_paths.get(idx).map(|s| s.as_str()).unwrap_or("?");
        return FileOpResult {
            success: false,
            message: format!("Refused: {} ({})", rej.explain(), target),
            affected_count: 0,
        };
    }
    if let Err(rej) = safety::check_destination(&destination) {
        let attempted = serde_json::json!({"sources": source_paths, "destination": destination});
        blocks::record_best_effort("copyFiles", &attempted, None, blocks::STATUS_FAILED);
        return FileOpResult {
            success: false,
            message: format!("Refused destination: {}", rej.explain()),
            affected_count: 0,
        };
    }

    let dest_path = std::path::Path::new(&destination);

    if !dest_path.exists() {
        if let Err(e) = fs::create_dir_all(dest_path) {
            return FileOpResult {
                success: false,
                message: format!("Failed to create destination: {}", e),
                affected_count: 0,
            };
        }
    }

    let mut copies: Vec<(String, String)> = Vec::new();
    let mut errors = Vec::new();

    for src in &source_paths {
        let src_path = std::path::Path::new(src);
        if let Some(file_name) = src_path.file_name() {
            let dest_file = dest_path.join(file_name);
            let dest_str = dest_file.to_string_lossy().into_owned();
            match fs::copy(src_path, &dest_file) {
                Ok(_) => copies.push((src.clone(), dest_str)),
                Err(e) => errors.push(format!("{}: {}", src, e)),
            }
        }
    }

    let copied = copies.len() as i32;
    let payload = serde_json::json!({
        "copies": copies.iter().map(|(f, t)| serde_json::json!({"from": f, "to": t})).collect::<Vec<_>>()
    });
    // Inverse: trash the copies. Originals are untouched.
    let inverse = serde_json::json!({
        "trash": copies.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>()
    });
    let status = if errors.is_empty() { blocks::STATUS_EXECUTED } else { blocks::STATUS_FAILED };
    blocks::record_best_effort("copyFiles", &payload, Some(&inverse), status);

    FileOpResult {
        success: errors.is_empty(),
        message: if errors.is_empty() {
            format!("Copied {} files", copied)
        } else {
            format!("Copied {} files, {} errors", copied, errors.len())
        },
        affected_count: copied,
    }
}

/// Move files to Trash
#[uniffi::export]
pub fn trash_files(paths: Vec<String>) -> FileOpResult {
    if let Err((idx, rej)) = safety::check_sources(&paths) {
        let attempted = serde_json::json!({"sources": paths});
        blocks::record_best_effort("trashFiles", &attempted, None, blocks::STATUS_FAILED);
        let target = paths.get(idx).map(|s| s.as_str()).unwrap_or("?");
        return FileOpResult {
            success: false,
            message: format!("Refused: {} ({})", rej.explain(), target),
            affected_count: 0,
        };
    }
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let trash_path = std::path::Path::new(&home).join(".Trash");

    let mut trashed: Vec<(String, String)> = Vec::new(); // (trash_loc, original)
    let mut errors = Vec::new();

    for src in &paths {
        let src_path = std::path::Path::new(src);
        if let Some(file_name) = src_path.file_name() {
            let mut dest_file = trash_path.join(file_name);
            let mut counter = 1;
            while dest_file.exists() {
                let stem = src_path.file_stem().unwrap_or_default().to_string_lossy();
                let ext = src_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                dest_file = trash_path.join(format!("{} {}{}", stem, counter, ext));
                counter += 1;
            }

            match fs::rename(src_path, &dest_file) {
                Ok(_) => trashed.push((dest_file.to_string_lossy().into_owned(), src.clone())),
                Err(e) => errors.push(format!("{}: {}", src, e)),
            }
        }
    }

    let count = trashed.len() as i32;
    let payload = serde_json::json!({
        "trashed": trashed.iter().map(|(t, o)| serde_json::json!({"from": o, "to": t})).collect::<Vec<_>>()
    });
    // Inverse: move from trash back to original location.
    let inverse = serde_json::json!({
        "moves": trashed.iter().map(|(t, o)| serde_json::json!({"from": t, "to": o})).collect::<Vec<_>>()
    });
    let status = if errors.is_empty() { blocks::STATUS_EXECUTED } else { blocks::STATUS_FAILED };
    blocks::record_best_effort("trashFiles", &payload, Some(&inverse), status);

    FileOpResult {
        success: errors.is_empty(),
        message: if errors.is_empty() {
            format!("Moved {} items to Trash", count)
        } else {
            format!("Trashed {} items, {} errors", count, errors.len())
        },
        affected_count: count,
    }
}

/// Rename a file. `new_name` must be a basename (no `/`, no `..`).
#[uniffi::export]
pub fn rename_file(path: String, new_name: String) -> FileOpResult {
    if let Err(rej) = safety::check_path(&path) {
        let attempted = serde_json::json!({"path": path, "new_name": new_name});
        blocks::record_best_effort("renameFile", &attempted, None, blocks::STATUS_FAILED);
        return FileOpResult {
            success: false,
            message: format!("Refused: {}", rej.explain()),
            affected_count: 0,
        };
    }
    // new_name must be a bare filename, not a path. Reject anything that
    // would let an attacker rewrite the destination directory.
    if new_name.is_empty()
        || new_name.contains('/')
        || new_name == "."
        || new_name == ".."
    {
        return FileOpResult {
            success: false,
            message: "New name must be a plain filename".to_string(),
            affected_count: 0,
        };
    }

    let src_path = std::path::Path::new(&path);
    let Some(parent) = src_path.parent() else {
        return FileOpResult {
            success: false,
            message: "Invalid path".to_string(),
            affected_count: 0,
        };
    };
    let new_path = parent.join(&new_name);

    if new_path.exists() {
        return FileOpResult {
            success: false,
            message: format!("File '{}' already exists", new_name),
            affected_count: 0,
        };
    }

    match fs::rename(src_path, &new_path) {
        Ok(_) => {
            // Normalize rename into the same "moves" primitive the undo
            // executor uses for moveFiles. One shape, one inverter.
            let new_full = new_path.to_string_lossy().into_owned();
            let payload = serde_json::json!({
                "moves": [{"from": path, "to": new_full}]
            });
            let inverse = serde_json::json!({
                "moves": [{"from": new_full, "to": path}]
            });
            blocks::record_best_effort("renameFile", &payload, Some(&inverse), blocks::STATUS_EXECUTED);
            FileOpResult {
                success: true,
                message: format!("Renamed to '{}'", new_name),
                affected_count: 1,
            }
        }
        Err(e) => FileOpResult {
            success: false,
            message: format!("Rename failed: {}", e),
            affected_count: 0,
        },
    }
}

/// Create a new folder
#[uniffi::export]
pub fn create_folder(path: String) -> FileOpResult {
    if let Err(rej) = safety::check_destination(&path) {
        let attempted = serde_json::json!({"path": path});
        blocks::record_best_effort("createFolder", &attempted, None, blocks::STATUS_FAILED);
        return FileOpResult {
            success: false,
            message: format!("Refused: {}", rej.explain()),
            affected_count: 0,
        };
    }
    // Only record inverse if the folder didn't already exist — otherwise undo
    // would delete a folder the user already had.
    let preexisted = std::path::Path::new(&path).exists();
    match fs::create_dir_all(&path) {
        Ok(_) => {
            let payload = serde_json::json!({"create_folder": path});
            let inverse = if preexisted {
                None
            } else {
                Some(serde_json::json!({"rmdir": path}))
            };
            blocks::record_best_effort("createFolder", &payload, inverse.as_ref(), blocks::STATUS_EXECUTED);
            FileOpResult {
                success: true,
                message: format!("Created folder"),
                affected_count: 1,
            }
        }
        Err(e) => FileOpResult {
            success: false,
            message: format!("Failed to create folder: {}", e),
            affected_count: 0,
        },
    }
}

/// Compress files into a ZIP archive
#[uniffi::export]
pub fn compress_files(paths: Vec<String>, archive_path: String) -> FileOpResult {
    use std::io::{Read, Write};

    if let Err((idx, rej)) = safety::check_sources(&paths) {
        let attempted = serde_json::json!({"sources": paths, "archive": archive_path});
        blocks::record_best_effort("compressFiles", &attempted, None, blocks::STATUS_FAILED);
        let target = paths.get(idx).map(|s| s.as_str()).unwrap_or("?");
        return FileOpResult {
            success: false,
            message: format!("Refused: {} ({})", rej.explain(), target),
            affected_count: 0,
        };
    }
    if let Err(rej) = safety::check_destination(&archive_path) {
        let attempted = serde_json::json!({"sources": paths, "archive": archive_path});
        blocks::record_best_effort("compressFiles", &attempted, None, blocks::STATUS_FAILED);
        return FileOpResult {
            success: false,
            message: format!("Refused archive path: {}", rej.explain()),
            affected_count: 0,
        };
    }

    let file = match fs::File::create(&archive_path) {
        Ok(f) => f,
        Err(e) => return FileOpResult {
            success: false,
            message: format!("Failed to create archive: {}", e),
            affected_count: 0,
        },
    };
    
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    
    let mut added = 0;
    
    for src in &paths {
        let src_path = std::path::Path::new(src);
        if let Some(file_name) = src_path.file_name() {
            if src_path.is_file() {
                if let Ok(mut f) = fs::File::open(src_path) {
                    let mut buffer = Vec::new();
                    if f.read_to_end(&mut buffer).is_ok() {
                        if zip.start_file(file_name.to_string_lossy(), options).is_ok() {
                            if zip.write_all(&buffer).is_ok() {
                                added += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    
    if zip.finish().is_err() {
        return FileOpResult {
            success: false,
            message: "Failed to finalize archive".to_string(),
            affected_count: 0,
        };
    }

    let payload = serde_json::json!({
        "compress": {"sources": paths, "archive": archive_path}
    });
    // Inverse: trash the archive. Sources were never touched.
    let inverse = serde_json::json!({"trash": [archive_path]});
    blocks::record_best_effort("compressFiles", &payload, Some(&inverse), blocks::STATUS_EXECUTED);

    FileOpResult {
        success: true,
        message: format!("Compressed {} files", added),
        affected_count: added,
    }
}

// ============== UNDO ==============

/// Apply a stored `inverse_payload`. Recognized primitives:
///   - `{"moves": [{"from": "...", "to": "..."}, ...]}` → fs::rename each
///   - `{"trash": ["...", ...]}` → send each to ~/.Trash with collision suffix
///   - `{"rmdir": "/path"}` → remove the directory (only succeeds if empty)
///
/// Every path is re-checked through `safety::check_path`. A DB write
/// vulnerability is still a vulnerability, but it can't be leveraged into
/// arbitrary FS access through undo.
fn execute_inverse(inverse: &serde_json::Value) -> FileOpResult {
    let mut affected: i32 = 0;
    let mut errors: Vec<String> = Vec::new();

    if let Some(moves) = inverse.get("moves").and_then(|v| v.as_array()) {
        for m in moves {
            let from = m.get("from").and_then(|v| v.as_str()).unwrap_or("");
            let to = m.get("to").and_then(|v| v.as_str()).unwrap_or("");
            if let Err(rej) = safety::check_path(from) {
                errors.push(format!("from {}: {}", from, rej.explain()));
                continue;
            }
            if let Err(rej) = safety::check_path(to) {
                errors.push(format!("to {}: {}", to, rej.explain()));
                continue;
            }
            if let Some(parent) = std::path::Path::new(to).parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::rename(from, to) {
                Ok(_) => affected += 1,
                Err(e) => errors.push(format!("rename {} → {}: {}", from, to, e)),
            }
        }
    }

    if let Some(trash_arr) = inverse.get("trash").and_then(|v| v.as_array()) {
        let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let trash_dir = std::path::Path::new(&home).join(".Trash");
        for entry in trash_arr {
            let p = entry.as_str().unwrap_or("");
            if let Err(rej) = safety::check_path(p) {
                errors.push(format!("trash {}: {}", p, rej.explain()));
                continue;
            }
            let src = std::path::Path::new(p);
            if let Some(file_name) = src.file_name() {
                let mut dest = trash_dir.join(file_name);
                let mut counter = 1;
                while dest.exists() {
                    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
                    let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                    dest = trash_dir.join(format!("{} {}{}", stem, counter, ext));
                    counter += 1;
                }
                match fs::rename(src, &dest) {
                    Ok(_) => affected += 1,
                    Err(e) => errors.push(format!("trash {}: {}", p, e)),
                }
            }
        }
    }

    if let Some(rmdir) = inverse.get("rmdir").and_then(|v| v.as_str()) {
        if let Err(rej) = safety::check_path(rmdir) {
            errors.push(format!("rmdir {}: {}", rmdir, rej.explain()));
        } else {
            match fs::remove_dir(rmdir) {
                Ok(_) => affected += 1,
                Err(e) => errors.push(format!("rmdir {}: {}", rmdir, e)),
            }
        }
    }

    FileOpResult {
        success: errors.is_empty(),
        message: if errors.is_empty() {
            format!("Reversed {} operation(s)", affected)
        } else {
            format!("Reversed {} ops, {} errors: {}", affected, errors.len(), errors.join("; "))
        },
        affected_count: affected,
    }
}

/// Undo the most recent reversible, still-executed block. Returns an
/// affected_count of 0 with success=false when there's nothing to undo
/// (UI should treat that as a no-op, not an error).
#[uniffi::export]
pub fn undo_last_block() -> FileOpResult {
    let conn = match db::open_default() {
        Ok(c) => c,
        Err(_) => return FileOpResult {
            success: false,
            message: "Index unavailable".to_string(),
            affected_count: 0,
        },
    };

    let row: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT id, kind, inverse_payload FROM blocks
             WHERE status = 'executed' AND inverse_payload IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    let Some((id, kind, inverse_payload)) = row else {
        return FileOpResult {
            success: false,
            message: "Nothing to undo".to_string(),
            affected_count: 0,
        };
    };
    let Some(inverse_str) = inverse_payload else {
        return FileOpResult {
            success: false,
            message: "Last action is not reversible".to_string(),
            affected_count: 0,
        };
    };
    let inverse: serde_json::Value = match serde_json::from_str(&inverse_str) {
        Ok(v) => v,
        Err(_) => return FileOpResult {
            success: false,
            message: "Corrupt undo data — refusing to act".to_string(),
            affected_count: 0,
        },
    };

    let result = execute_inverse(&inverse);

    if result.success {
        let _ = conn.execute(
            "UPDATE blocks SET status = 'undone' WHERE id = ?1",
            params![id],
        );
    }

    FileOpResult {
        success: result.success,
        message: if result.success {
            format!("Undid {} ({} item(s))", kind, result.affected_count)
        } else {
            format!("Undo failed: {}", result.message)
        },
        affected_count: result.affected_count,
    }
}

/// Returns true if there is at least one reversible block that hasn't been
/// undone — the Swift menu uses this to enable/disable the Cmd+Z menu item.
#[uniffi::export]
pub fn can_undo() -> bool {
    let Ok(conn) = db::open_default() else { return false; };
    conn.query_row(
        "SELECT 1 FROM blocks
         WHERE status = 'executed' AND inverse_payload IS NOT NULL
         LIMIT 1",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // MARK: - format_relative_date

    #[test]
    fn test_format_relative_date_just_now() {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative_date(now), "Just now");
        assert_eq!(format_relative_date(now - 30), "Just now");
    }

    #[test]
    fn test_format_relative_date_minutes() {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative_date(now - 120), "2m ago");
        assert_eq!(format_relative_date(now - 3000), "50m ago");
    }

    #[test]
    fn test_format_relative_date_hours() {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative_date(now - 7200), "2h ago");
        assert_eq!(format_relative_date(now - 43200), "12h ago");
    }

    #[test]
    fn test_format_relative_date_days() {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative_date(now - 86400), "1d ago");
        assert_eq!(format_relative_date(now - 86400 * 3), "3d ago");
    }

    // MARK: - get_file_kind

    #[test]
    fn test_get_file_kind_folder() {
        let path = std::path::Path::new("/some/folder");
        assert_eq!(get_file_kind(path, true), "Folder");
    }

    #[test]
    fn test_get_file_kind_pdf() {
        let path = std::path::Path::new("/some/file.pdf");
        assert_eq!(get_file_kind(path, false), "PDF Document");
    }

    #[test]
    fn test_get_file_kind_unknown() {
        let path = std::path::Path::new("/some/file.xyz");
        assert_eq!(get_file_kind(path, false), "XYZ File");
    }

    #[test]
    fn test_get_file_kind_no_extension() {
        let path = std::path::Path::new("/some/file");
        assert_eq!(get_file_kind(path, false), "Document");
    }

    #[test]
    fn test_get_file_kind_swift() {
        let path = std::path::Path::new("/some/file.swift");
        assert_eq!(get_file_kind(path, false), "Swift Source");
    }

    // Serialize FS-op tests that share the process-global FAST_FINDER_DB_PATH
    // env var. Each test points the var at its own tempdir before exercising
    // the public API, so block recording lands somewhere disposable.
    static FS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct DbScope {
        _guard: std::sync::MutexGuard<'static, ()>,
        tmp: tempfile::TempDir,
        prev_home: Option<String>,
    }

    impl DbScope {
        fn path(&self) -> &std::path::Path { self.tmp.path() }
    }

    impl Drop for DbScope {
        fn drop(&mut self) {
            // Restore HOME so subsequent tests (and the test runner itself)
            // don't inherit the tempdir HOME after this scope exits.
            match self.prev_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn db_scope() -> DbScope {
        let guard = FS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("FAST_FINDER_DB_PATH", tmp.path().join("test.sqlite3"));
        // Override HOME so safety checks treat the tempdir as the user's home
        // and trash_files lands inside tempdir/.Trash. Required for the FS ops
        // that derive paths from HOME (trash, app-data-dir checks, etc.).
        let prev_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        std::fs::create_dir_all(tmp.path().join(".Trash")).unwrap();
        DbScope { _guard: guard, tmp, prev_home }
    }

    fn last_block(kind: &str) -> Option<(String, Option<String>, String)> {
        let conn = db::open_default().ok()?;
        conn.query_row(
            "SELECT payload, inverse_payload, status FROM blocks
             WHERE kind = ?1 ORDER BY id DESC LIMIT 1",
            rusqlite::params![kind],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
    }

    // MARK: - search_files

    #[test]
    fn test_search_files_empty_query() {
        let results = search_files("".to_string());
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_files_whitespace_query() {
        let results = search_files("   ".to_string());
        assert!(results.is_empty());
    }

    #[test]
    fn test_build_fts_query_sanitizes_and_prefixes() {
        assert_eq!(build_fts_query("").as_deref(), None);
        assert_eq!(build_fts_query("   ").as_deref(), None);
        assert_eq!(build_fts_query("!!!").as_deref(), None);
        assert_eq!(build_fts_query("report").as_deref(), Some("report*"));
        // Non-alphanumerics split into tokens; lowercase matches the index.
        assert_eq!(build_fts_query("Report Q3.pdf").as_deref(), Some("report* q3* pdf*"));
        // FTS5 keywords (OR/AND/NOT/NEAR) are uppercase-only; lowercasing
        // disarms them so user input can't leak boolean semantics.
        assert_eq!(build_fts_query("a\"b OR c").as_deref(), Some("a* b* or* c*"));
    }

    #[test]
    fn test_search_files_uses_fts_index() {
        let _scope = db_scope();
        // Seed the index with one row.
        {
            let conn = db::open_default().unwrap();
            conn.execute(
                "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, ctime, file_kind, indexed_at)
                 VALUES ('/tmp/Report Q3.pdf', 'Report Q3.pdf', '/tmp', 'pdf', 1024, 0, 1700000000, 0, 'PDF Document', 1700000000)",
                [],
            ).unwrap();
        }
        let hits = search_files("repor".to_string());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_name, "Report Q3.pdf");
        assert_eq!(hits[0].file_path, "/tmp/Report Q3.pdf");
    }

    // MARK: - move_files

    #[test]
    fn test_move_files() {
        let _scope = db_scope();
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("test.txt");
        let dest_dir = dir.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();

        fs::write(&src_path, "hello").unwrap();

        let result = move_files(
            vec![src_path.to_string_lossy().to_string()],
            dest_dir.to_string_lossy().to_string(),
        );

        assert!(result.success);
        assert_eq!(result.affected_count, 1);
        assert!(!src_path.exists());
        assert!(dest_dir.join("test.txt").exists());

        // The op must have recorded a block with a populated inverse.
        let (payload, inverse, status) = last_block("moveFiles").expect("block not recorded");
        assert_eq!(status, "executed");
        assert!(payload.contains("\"moves\""));
        let inv = inverse.expect("inverse_payload must be set for moveFiles");
        // Inverse points back to the original src location.
        assert!(inv.contains(&src_path.to_string_lossy().to_string()));
    }

    // MARK: - rename_file

    #[test]
    fn test_rename_file() {
        let _scope = db_scope();
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("old.txt");
        fs::write(&src_path, "content").unwrap();

        let result = rename_file(
            src_path.to_string_lossy().to_string(),
            "new.txt".to_string(),
        );

        assert!(result.success);
        assert!(!src_path.exists());
        assert!(dir.path().join("new.txt").exists());

        let (_, inverse, _) = last_block("renameFile").expect("block not recorded");
        let inv = inverse.expect("inverse must restore old name");
        // Inverse uses the unified "moves" primitive: from new full path → to old full path.
        let old_full = src_path.to_string_lossy().to_string();
        let new_full = dir.path().join("new.txt").to_string_lossy().to_string();
        assert!(inv.contains(&format!("\"from\":\"{}\"", new_full)), "inverse: {}", inv);
        assert!(inv.contains(&format!("\"to\":\"{}\"", old_full)), "inverse: {}", inv);
    }

    #[test]
    fn test_rename_file_already_exists() {
        let _scope = db_scope();
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("old.txt");
        let existing = dir.path().join("new.txt");
        fs::write(&src_path, "content1").unwrap();
        fs::write(&existing, "content2").unwrap();

        let result = rename_file(
            src_path.to_string_lossy().to_string(),
            "new.txt".to_string(),
        );

        assert!(!result.success);
        // No block should have been recorded for the rejected rename.
        assert!(last_block("renameFile").is_none());
    }

    // MARK: - compress_files

    #[test]
    fn test_compress_files() {
        let _scope = db_scope();
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("a.txt");
        let file2 = dir.path().join("b.txt");
        fs::write(&file1, "aaa").unwrap();
        fs::write(&file2, "bbb").unwrap();

        let archive = dir.path().join("out.zip");
        let result = compress_files(
            vec![
                file1.to_string_lossy().to_string(),
                file2.to_string_lossy().to_string(),
            ],
            archive.to_string_lossy().to_string(),
        );

        assert!(result.success);
        assert_eq!(result.affected_count, 2);
        assert!(archive.exists());
        assert!(archive.metadata().unwrap().len() > 0);

        // Inverse for compress is trash-the-archive.
        let (_, inverse, _) = last_block("compressFiles").expect("block not recorded");
        let inv = inverse.expect("inverse must trash the archive");
        assert!(inv.contains(&archive.to_string_lossy().to_string()));
    }

    // MARK: - load_cached_index / get_recent_files via SQLite

    // MARK: - safety integration with FS ops

    #[test]
    fn test_move_files_rejects_system_path() {
        let _scope = db_scope();
        let dest = _scope.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let result = move_files(
            vec!["/etc/passwd".to_string()],
            dest.to_string_lossy().to_string(),
        );

        assert!(!result.success);
        assert_eq!(result.affected_count, 0);
        assert!(result.message.contains("protected system directory"), "got: {}", result.message);

        // A failed-status block must have been recorded for audit.
        let (_, inverse, status) = last_block("moveFiles").expect("audit block missing");
        assert_eq!(status, "failed");
        assert!(inverse.is_none(), "rejected ops must not have inverses");
    }

    #[test]
    fn test_trash_files_rejects_home_root_dotfile() {
        let _scope = db_scope();
        // Create a dotfile at the (overridden) HOME root and try to trash it.
        let dotfile = _scope.path().join(".zshrc");
        std::fs::write(&dotfile, "echo hi").unwrap();

        let result = trash_files(vec![dotfile.to_string_lossy().to_string()]);

        assert!(!result.success);
        assert!(result.message.contains("top-level home dotfile"), "got: {}", result.message);
        // File must still be on disk.
        assert!(dotfile.exists());
    }

    #[test]
    fn test_move_files_rejects_app_data_dir_as_destination() {
        let _scope = db_scope();
        let src = _scope.path().join("file.txt");
        std::fs::write(&src, "x").unwrap();

        // Destination inside our own index dir must be refused.
        let bad_dest = _scope.path().join(".fast-finder").join("subdir");

        let result = move_files(
            vec![src.to_string_lossy().to_string()],
            bad_dest.to_string_lossy().to_string(),
        );

        assert!(!result.success);
        assert!(result.message.contains("Fast-Finder index"), "got: {}", result.message);
        // Source untouched.
        assert!(src.exists());
    }

    #[test]
    fn test_rename_rejects_path_traversal_in_new_name() {
        let _scope = db_scope();
        let src = _scope.path().join("file.txt");
        std::fs::write(&src, "x").unwrap();

        let r = rename_file(src.to_string_lossy().to_string(), "../escape.txt".to_string());
        assert!(!r.success);
        assert!(r.message.contains("plain filename"), "got: {}", r.message);
        assert!(src.exists());

        let r2 = rename_file(src.to_string_lossy().to_string(), "a/b.txt".to_string());
        assert!(!r2.success);
    }

    #[test]
    fn test_bulk_cap_in_move_files() {
        let _scope = db_scope();
        // We don't need real files; safety bulk-cap fires before any FS access.
        let many: Vec<String> = (0..safety::MAX_BULK_PATHS + 1)
            .map(|i| _scope.path().join(format!("f{}.txt", i)).to_string_lossy().to_string())
            .collect();
        let r = move_files(many, _scope.path().join("dest").to_string_lossy().to_string());
        assert!(!r.success);
        assert!(r.message.contains("too many paths"), "got: {}", r.message);
    }

    // MARK: - undo

    #[test]
    fn test_undo_move_files_restores_originals() {
        let _scope = db_scope();
        let src = _scope.path().join("file.txt");
        let dest = _scope.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(&src, "hello").unwrap();

        let r = move_files(
            vec![src.to_string_lossy().to_string()],
            dest.to_string_lossy().to_string(),
        );
        assert!(r.success);
        assert!(!src.exists());
        assert!(dest.join("file.txt").exists());

        let u = undo_last_block();
        assert!(u.success, "undo failed: {}", u.message);
        assert_eq!(u.affected_count, 1);
        assert!(src.exists(), "original location should be restored");
        assert!(!dest.join("file.txt").exists());
    }

    #[test]
    fn test_undo_trash_files_restores_originals() {
        let _scope = db_scope();
        let src = _scope.path().join("file.txt");
        std::fs::write(&src, "hello").unwrap();

        let r = trash_files(vec![src.to_string_lossy().to_string()]);
        assert!(r.success, "trash failed: {}", r.message);
        assert!(!src.exists());

        let u = undo_last_block();
        assert!(u.success, "undo failed: {}", u.message);
        assert!(src.exists(), "file should be restored from trash");
    }

    #[test]
    fn test_undo_rename_restores_old_name() {
        let _scope = db_scope();
        let old = _scope.path().join("old.txt");
        std::fs::write(&old, "x").unwrap();

        let r = rename_file(old.to_string_lossy().to_string(), "new.txt".to_string());
        assert!(r.success);
        assert!(!old.exists());
        assert!(_scope.path().join("new.txt").exists());

        let u = undo_last_block();
        assert!(u.success, "{}", u.message);
        assert!(old.exists());
        assert!(!_scope.path().join("new.txt").exists());
    }

    #[test]
    fn test_undo_create_folder_removes_empty_folder() {
        let _scope = db_scope();
        let new_folder = _scope.path().join("brand_new");

        let r = create_folder(new_folder.to_string_lossy().to_string());
        assert!(r.success);
        assert!(new_folder.exists());

        let u = undo_last_block();
        assert!(u.success, "{}", u.message);
        assert!(!new_folder.exists());
    }

    #[test]
    fn test_undo_compress_files_trashes_archive() {
        let _scope = db_scope();
        let f = _scope.path().join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let archive = _scope.path().join("out.zip");

        let r = compress_files(
            vec![f.to_string_lossy().to_string()],
            archive.to_string_lossy().to_string(),
        );
        assert!(r.success);
        assert!(archive.exists());

        let u = undo_last_block();
        assert!(u.success, "{}", u.message);
        assert!(!archive.exists(), "archive should have been trashed");
        // Source file untouched.
        assert!(f.exists());
    }

    #[test]
    fn test_can_undo_lifecycle() {
        let _scope = db_scope();
        assert!(!can_undo(), "no blocks yet");

        let src = _scope.path().join("file.txt");
        std::fs::write(&src, "x").unwrap();
        let r = rename_file(src.to_string_lossy().to_string(), "new.txt".to_string());
        assert!(r.success);
        assert!(can_undo(), "should be undoable after a rename");

        let u = undo_last_block();
        assert!(u.success);
        assert!(!can_undo(), "after undoing the only block, nothing left to undo");
    }

    #[test]
    fn test_undo_with_no_blocks_is_a_noop_failure() {
        let _scope = db_scope();
        let r = undo_last_block();
        assert!(!r.success);
        assert_eq!(r.affected_count, 0);
        assert!(r.message.to_lowercase().contains("nothing"), "got: {}", r.message);
    }

    #[test]
    fn test_undo_refuses_corrupt_inverse() {
        let _scope = db_scope();
        let conn = db::open_default().unwrap();
        // Plant a block with malformed JSON in inverse_payload.
        conn.execute(
            "INSERT INTO blocks (kind, payload, inverse_payload, status, created_at)
             VALUES ('moveFiles', '{}', 'not json', 'executed', ?1)",
            rusqlite::params![blocks::now_ts()],
        ).unwrap();

        let r = undo_last_block();
        assert!(!r.success);
        assert!(r.message.to_lowercase().contains("corrupt"), "got: {}", r.message);
    }

    #[test]
    fn test_undo_refuses_inverse_targeting_system_path() {
        let _scope = db_scope();
        let conn = db::open_default().unwrap();
        // Plant a block whose inverse_payload tries to move a system file.
        // Even if the DB is compromised, undo must refuse to act on /etc.
        conn.execute(
            "INSERT INTO blocks (kind, payload, inverse_payload, status, created_at)
             VALUES ('moveFiles', '{}',
                     '{\"moves\":[{\"from\":\"/etc/passwd\",\"to\":\"/tmp/owned\"}]}',
                     'executed', ?1)",
            rusqlite::params![blocks::now_ts()],
        ).unwrap();

        let r = undo_last_block();
        assert!(!r.success);
        assert!(r.message.contains("protected system directory"), "got: {}", r.message);
    }

    #[test]
    fn test_get_recent_files_returns_recent_rows() {
        let _scope = db_scope();
        let conn = db::open_default().unwrap();
        let now = blocks::now_ts();
        let yesterday = now - 86_400;
        let last_year = now - 60 * 60 * 24 * 400;

        conn.execute(
            "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, ctime, file_kind, indexed_at)
             VALUES ('/tmp/fresh.txt', 'fresh.txt', '/tmp', 'txt', 1, 0, ?1, 0, 'Plain Text', ?1),
                    ('/tmp/old.txt',   'old.txt',   '/tmp', 'txt', 1, 0, ?2, 0, 'Plain Text', ?2)",
            rusqlite::params![yesterday, last_year],
        ).unwrap();

        let recents = get_recent_files();
        assert_eq!(recents.len(), 1, "only the recent file should appear");
        assert_eq!(recents[0].file_name, "fresh.txt");
    }

    // MARK: - indexing policy

    /// db_scope() points HOME at a tempdir, so scan_roots() resolves under it.
    fn docs_root(scope: &DbScope) -> std::path::PathBuf {
        let docs = scope.path().join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        docs
    }

    #[test]
    fn test_is_indexable_scope_rules() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);

        assert!(is_indexable(&docs.join("report.pdf"), false));
        // Extension not on the allow-list.
        assert!(!is_indexable(&docs.join("core.dump"), false));
        // A file with no extension is skipped; a directory with none is kept.
        assert!(!is_indexable(&docs.join("Makefile"), false));
        assert!(is_indexable(&docs.join("Projects"), true));
        // Hidden anywhere in the relative path.
        assert!(!is_indexable(&docs.join(".secret.pdf"), false));
        assert!(!is_indexable(&docs.join(".cache/report.pdf"), false));
        // Outside every scan root.
        assert!(!is_indexable(&_scope.path().join("Movies/clip.mp4"), false));
        // Past the depth budget (root + 6 components).
        let deep = docs.join("a/b/c/d/e/f.pdf");
        assert!(!is_indexable(&deep, false));
        assert!(is_indexable(&docs.join("a/b/c/d/e.pdf"), false));
    }

    #[test]
    fn test_index_paths_upserts_a_new_file() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let f = docs.join("notes.md");
        fs::write(&f, "hello").unwrap();

        let update = index_paths(vec![f.to_string_lossy().into_owned()]);
        assert_eq!(update.upserted, 1);
        assert_eq!(update.removed, 0);

        // Reachable through FTS, i.e. the mirror triggers fired.
        let hits = search_files("notes".to_string());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_path, f.to_string_lossy());
    }

    #[test]
    fn test_index_paths_is_idempotent() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let f = docs.join("notes.md");
        fs::write(&f, "hello").unwrap();
        let arg = vec![f.to_string_lossy().into_owned()];

        index_paths(arg.clone());
        index_paths(arg.clone());

        let conn = db::open_default().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "re-indexing the same path must not duplicate rows");
    }

    #[test]
    fn test_index_paths_removes_a_deleted_file() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let f = docs.join("gone.md");
        fs::write(&f, "x").unwrap();
        let arg = vec![f.to_string_lossy().into_owned()];

        assert_eq!(index_paths(arg.clone()).upserted, 1);
        fs::remove_file(&f).unwrap();

        let update = index_paths(arg);
        assert_eq!(update.removed, 1);
        assert!(search_files("gone".to_string()).is_empty());
    }

    #[test]
    fn test_index_paths_ignores_paths_outside_scan_roots() {
        let _scope = db_scope();
        let movies = _scope.path().join("Movies");
        std::fs::create_dir_all(&movies).unwrap();
        let f = movies.join("clip.mp4");
        fs::write(&f, "x").unwrap();

        let update = index_paths(vec![f.to_string_lossy().into_owned()]);
        assert!(update.is_noop(), "outside a scan root, nothing should change");
    }

    #[test]
    fn test_index_paths_walks_a_directory_and_prunes_its_deletions() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let proj = docs.join("Project");
        std::fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("a.md"), "a").unwrap();
        fs::write(proj.join("b.md"), "b").unwrap();
        let arg = vec![proj.to_string_lossy().into_owned()];

        // The directory row itself plus both files.
        let first = index_paths(arg.clone());
        assert_eq!(first.upserted, 3);
        assert_eq!(first.removed, 0);

        // FSEvents coalesces: we only hear about the directory, not b.md.
        fs::remove_file(proj.join("b.md")).unwrap();
        let second = index_paths(arg);
        assert_eq!(second.removed, 1, "b.md should be pruned by the subtree diff");
        assert!(search_files("b".to_string()).is_empty());
        assert_eq!(search_files("a".to_string()).len(), 1);
    }

    #[test]
    fn test_index_paths_deleted_directory_takes_its_subtree() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let proj = docs.join("Project");
        std::fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("a.md"), "a").unwrap();
        let arg = vec![proj.to_string_lossy().into_owned()];
        assert_eq!(index_paths(arg.clone()).upserted, 2);

        std::fs::remove_dir_all(&proj).unwrap();
        let update = index_paths(arg);
        // The directory row and the file beneath it.
        assert_eq!(update.removed, 2);
        assert!(search_files("a".to_string()).is_empty());
    }

    #[test]
    fn test_index_paths_drops_a_file_renamed_to_an_unindexed_extension() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let old = docs.join("notes.md");
        fs::write(&old, "x").unwrap();
        assert_eq!(index_paths(vec![old.to_string_lossy().into_owned()]).upserted, 1);

        let new = docs.join("notes.dump");
        fs::rename(&old, &new).unwrap();

        // FSEvents reports both sides of a rename.
        let update = index_paths(vec![
            old.to_string_lossy().into_owned(),
            new.to_string_lossy().into_owned(),
        ]);
        assert_eq!(update.removed, 1, "the old path is gone from disk");
        assert_eq!(update.upserted, 0, "the new extension is not indexed");
        assert!(search_files("notes".to_string()).is_empty());
    }

    #[test]
    fn test_needs_full_rescan_when_index_is_empty() {
        let _scope = db_scope();
        assert!(needs_full_rescan(), "an empty index always needs a full scan");
    }

    #[test]
    fn test_needs_full_rescan_false_after_a_recent_scan() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        fs::write(docs.join("a.md"), "a").unwrap();

        rebuild_index();
        assert!(!needs_full_rescan(), "a fresh full scan should be trusted");

        // Age the stamp past the trust window.
        let conn = db::open_default().unwrap();
        let stale = blocks::now_ts() - FULL_SCAN_MAX_AGE_SECS - 1;
        db::set_setting(&conn, SETTING_LAST_FULL_SCAN, &stale.to_string()).unwrap();
        assert!(needs_full_rescan(), "a stale scan stamp should force a rescan");
    }

    #[test]
    fn test_index_paths_prune_does_not_treat_filenames_as_sql_wildcards() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);

        // `_` is a single-character wildcard in LIKE and utterly ordinary in a
        // folder name. `tax_2025` must not reach into `taxX2025`.
        let target = docs.join("tax_2025");
        let bystander = docs.join("taxX2025");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&bystander).unwrap();
        fs::write(target.join("a.md"), "a").unwrap();
        fs::write(bystander.join("b.md"), "b").unwrap();

        index_paths(vec![
            target.to_string_lossy().into_owned(),
            bystander.to_string_lossy().into_owned(),
        ]);
        assert_eq!(search_files("b".to_string()).len(), 1, "precondition");

        // Re-index only `tax_2025`. Nothing under `taxX2025` may be pruned.
        index_paths(vec![target.to_string_lossy().into_owned()]);

        let conn = db::open_default().unwrap();
        let survived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = ?1",
                rusqlite::params![bystander.join("b.md").to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(survived, 1, "a sibling folder must not be pruned by a wildcard match");
    }

    #[test]
    fn test_file_ops_refuse_a_symlink_pointing_at_a_sensitive_path() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);

        // The classic shape: something that arrives inside an archive.
        let secret_dir = _scope.path().join(".ssh");
        std::fs::create_dir_all(&secret_dir).unwrap();
        let secret = secret_dir.join("id_rsa");
        fs::write(&secret, "PRIVATE KEY MATERIAL").unwrap();

        let bait = docs.join("invoice.pdf");
        std::os::unix::fs::symlink(&secret, &bait).unwrap();

        let dest = docs.join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let copied = copy_files(
            vec![bait.to_string_lossy().into_owned()],
            dest.to_string_lossy().into_owned(),
        );
        assert!(!copied.success, "copying through a symlink into ~/.ssh must be refused");
        assert!(!dest.join("invoice.pdf").exists(), "no key material may be materialised");

        let archive = docs.join("out.zip");
        let zipped = compress_files(
            vec![bait.to_string_lossy().into_owned()],
            archive.to_string_lossy().into_owned(),
        );
        assert!(!zipped.success, "archiving through the same symlink must be refused");
    }

    #[test]
    fn test_move_refuses_launch_agents_as_destination() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let src = docs.join("payload.json");
        fs::write(&src, "{}").unwrap();

        let launch_agents = _scope.path().join("Library/LaunchAgents");
        let result = move_files(
            vec![src.to_string_lossy().into_owned()],
            launch_agents.to_string_lossy().into_owned(),
        );
        assert!(!result.success, "LaunchAgents is code-execution at login");
        assert!(src.exists(), "source must be untouched");
    }

    #[test]
    fn test_last_event_id_round_trip() {
        let _scope = db_scope();
        assert_eq!(last_event_id(), 0, "no stored id yet");
        set_last_event_id(918_273_645);
        assert_eq!(last_event_id(), 918_273_645);
    }
}