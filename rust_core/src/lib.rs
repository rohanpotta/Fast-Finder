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
    let dest_path = std::path::Path::new(&destination);
    
    // Create destination if it doesn't exist
    if !dest_path.exists() {
        if let Err(e) = fs::create_dir_all(dest_path) {
            return FileOpResult {
                success: false,
                message: format!("Failed to create destination: {}", e),
                affected_count: 0,
            };
        }
    }
    
    let mut moved = 0;
    let mut errors = Vec::new();
    
    for src in &source_paths {
        let src_path = std::path::Path::new(src);
        if let Some(file_name) = src_path.file_name() {
            let dest_file = dest_path.join(file_name);
            match fs::rename(src_path, &dest_file) {
                Ok(_) => moved += 1,
                Err(_e) => {
                    // If rename fails (cross-device), try copy + delete
                    if let Err(copy_err) = fs::copy(src_path, &dest_file) {
                        errors.push(format!("{}: {}", src, copy_err));
                    } else {
                        let _ = fs::remove_file(src_path);
                        moved += 1;
                    }
                }
            }
        }
    }
    
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
    
    let mut copied = 0;
    let mut errors = Vec::new();
    
    for src in &source_paths {
        let src_path = std::path::Path::new(src);
        if let Some(file_name) = src_path.file_name() {
            let dest_file = dest_path.join(file_name);
            match fs::copy(src_path, &dest_file) {
                Ok(_) => copied += 1,
                Err(e) => errors.push(format!("{}: {}", src, e)),
            }
        }
    }
    
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
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let trash_path = std::path::Path::new(&home).join(".Trash");
    
    let mut trashed = 0;
    let mut errors = Vec::new();
    
    for src in &paths {
        let src_path = std::path::Path::new(src);
        if let Some(file_name) = src_path.file_name() {
            // Generate unique name if file already exists in trash
            let mut dest_file = trash_path.join(file_name);
            let mut counter = 1;
            while dest_file.exists() {
                let stem = src_path.file_stem().unwrap_or_default().to_string_lossy();
                let ext = src_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                dest_file = trash_path.join(format!("{} {}{}", stem, counter, ext));
                counter += 1;
            }
            
            match fs::rename(src_path, &dest_file) {
                Ok(_) => trashed += 1,
                Err(e) => errors.push(format!("{}: {}", src, e)),
            }
        }
    }
    
    FileOpResult {
        success: errors.is_empty(),
        message: if errors.is_empty() {
            format!("Moved {} items to Trash", trashed)
        } else {
            format!("Trashed {} items, {} errors", trashed, errors.len())
        },
        affected_count: trashed,
    }
}

/// Rename a file
#[uniffi::export]
pub fn rename_file(path: String, new_name: String) -> FileOpResult {
    let src_path = std::path::Path::new(&path);
    
    if let Some(parent) = src_path.parent() {
        let new_path = parent.join(&new_name);
        
        if new_path.exists() {
            return FileOpResult {
                success: false,
                message: format!("File '{}' already exists", new_name),
                affected_count: 0,
            };
        }
        
        match fs::rename(src_path, &new_path) {
            Ok(_) => FileOpResult {
                success: true,
                message: format!("Renamed to '{}'", new_name),
                affected_count: 1,
            },
            Err(e) => FileOpResult {
                success: false,
                message: format!("Rename failed: {}", e),
                affected_count: 0,
            },
        }
    } else {
        FileOpResult {
            success: false,
            message: "Invalid path".to_string(),
            affected_count: 0,
        }
    }
}

/// Create a new folder
#[uniffi::export]
pub fn create_folder(path: String) -> FileOpResult {
    match fs::create_dir_all(&path) {
        Ok(_) => FileOpResult {
            success: true,
            message: format!("Created folder"),
            affected_count: 1,
        },
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
    
    FileOpResult {
        success: true,
        message: format!("Compressed {} files", added),
        affected_count: added,
    }
}