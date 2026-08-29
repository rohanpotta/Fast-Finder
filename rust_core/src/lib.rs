use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
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

/// Rebuild the index and save to cache (call in background)
#[uniffi::export]
pub fn rebuild_index() -> Vec<SearchResult> {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    
    let scan_folders = vec![
        format!("{}/Documents", home),
        format!("{}/Downloads", home),
        format!("{}/Desktop", home),
    ];
    
    let allowed_extensions: std::collections::HashSet<&str> = [
        "pdf", "doc", "docx", "txt", "rtf", "md", "pages", "odt",
        "xls", "xlsx", "csv", "numbers",
        "ppt", "pptx", "key",
        "jpg", "jpeg", "png", "gif", "heic", "webp", "svg", "psd", "ai",
        "mp4", "mov", "avi", "mkv", "webm",
        "mp3", "wav", "aac", "flac", "m4a",
        "py", "js", "ts", "rs", "swift", "java", "go", "html", "css", "json",
        "zip", "tar", "gz", "rar", "7z", "dmg",
    ].iter().cloned().collect();
    
    // Tuple Vec so we can persist raw (mtime, ctime) alongside the
    // FFI-visible SearchResult without bloating its public shape.
    let collected_mutex: Arc<Mutex<Vec<(SearchResult, i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));

    for folder in &scan_folders {
        if !std::path::Path::new(folder).exists() {
            continue;
        }

        let results_clone = collected_mutex.clone();
        let allowed_ext = allowed_extensions.clone();

        let walker = WalkBuilder::new(folder)
            .hidden(true)
            .git_ignore(true)
            .max_depth(Some(5))
            .threads(4)
            .build_parallel();

        walker.run(move || {
            let results = results_clone.clone();
            let allowed_ext = allowed_ext.clone();

            Box::new(move |entry_result| {
                if let Ok(entry) = entry_result {
                    let path = entry.path();

                    if let Some(ext) = path.extension() {
                        let ext_lower = ext.to_string_lossy().to_lowercase();
                        if !allowed_ext.contains(ext_lower.as_str()) {
                            return ignore::WalkState::Continue;
                        }
                    } else if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                        return ignore::WalkState::Continue;
                    }

                    if let Ok(metadata) = entry.metadata() {
                        let is_folder = metadata.is_dir();
                        let (mtime, ctime) = extract_times(&metadata);
                        let (date_value, date_kind) = get_best_date(&metadata);
                        let name = entry.file_name().to_string_lossy().to_string();
                        let path_str = path.to_string_lossy().to_string();
                        let file_kind = get_file_kind(path, is_folder);

                        if let Ok(mut lock) = results.lock() {
                            lock.push((
                                SearchResult {
                                    file_name: name,
                                    file_path: path_str,
                                    file_size: metadata.len(),
                                    is_folder,
                                    score: date_value,
                                    date_value,
                                    date_kind: date_kind.to_string(),
                                    file_kind,
                                    pretty_date: format_relative_date(date_value),
                                },
                                mtime,
                                ctime,
                            ));
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    }

    let mut collected = collected_mutex.lock().unwrap().clone();
    collected.sort_by(|a, b| b.0.date_value.cmp(&a.0.date_value));

    // Persist to SQLite. UPSERT on path so stable file ids survive across
    // rebuilds — the blocks table FKs into files(id) and we must not nuke
    // those references on every scan. After the inserts, tombstone-prune any
    // rows under a scan root that we *didn't* re-touch (i.e. files removed
    // from disk since last index).
    if let Ok(mut conn) = db::open_default() {
        let now = blocks::now_ts();
        if let Ok(tx) = conn.transaction() {
            {
                let stmt = tx.prepare(
                    "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, ctime, file_kind, indexed_at)
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
                         indexed_at = excluded.indexed_at",
                ).ok();

                if let Some(mut stmt) = stmt {
                    for (r, mtime, ctime) in &collected {
                        let p = std::path::Path::new(&r.file_path);
                        let parent = p.parent()
                            .map(|x| x.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let ext = p.extension()
                            .map(|e| e.to_string_lossy().to_lowercase());
                        let _ = stmt.execute(params![
                            r.file_path,
                            r.file_name,
                            parent,
                            ext,
                            r.file_size as i64,
                            r.is_folder as i64,
                            mtime,
                            ctime,
                            r.file_kind,
                            now,
                        ]);
                    }
                }

                // Tombstone-prune: any row under a scan root whose indexed_at
                // is older than this run was not re-visited and must have
                // been removed/renamed/moved. CASCADE drops dependent rows.
                if let Ok(mut prune) = tx.prepare(
                    "DELETE FROM files
                     WHERE indexed_at < ?1
                       AND (path LIKE ?2 || '/%')",
                ) {
                    for folder in &scan_folders {
                        let _ = prune.execute(params![now, folder]);
                    }
                }
            }
            let _ = tx.commit();
        }
    }

    collected.into_iter().map(|(sr, _, _)| sr).collect()
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
}