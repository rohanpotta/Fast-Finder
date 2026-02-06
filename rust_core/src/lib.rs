use std::env;
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use ignore::WalkBuilder;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};

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
}

// Cache structure for persistence
#[derive(Serialize, Deserialize, Default)]
struct FileCache {
    last_updated: i64,
    files: Vec<SearchResult>,
}

fn cache_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(format!("{}/.fast-finder-cache.json", home))
}

fn load_cache() -> FileCache {
    let path = cache_path();
    if let Ok(file) = fs::File::open(&path) {
        let reader = BufReader::new(file);
        serde_json::from_reader(reader).unwrap_or_default()
    } else {
        FileCache::default()
    }
}

fn save_cache(cache: &FileCache) {
    let path = cache_path();
    if let Ok(file) = fs::File::create(&path) {
        let writer = BufWriter::new(file);
        let _ = serde_json::to_writer(writer, cache);
    }
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
    let mtime = metadata.modified().ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    
    let ctime = metadata.created().ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    
    if ctime > mtime {
        (ctime, "Created")
    } else {
        (mtime, "Modified")
    }
}

/// Load cached index for instant startup
#[uniffi::export]
pub fn load_cached_index() -> Vec<SearchResult> {
    let cache = load_cache();
    cache.files
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
    
    let results_mutex = Arc::new(Mutex::new(Vec::new()));
    
    for folder in scan_folders {
        if !std::path::Path::new(&folder).exists() {
            continue;
        }
        
        let results_clone = results_mutex.clone();
        let allowed_ext = allowed_extensions.clone();
        
        let walker = WalkBuilder::new(&folder)
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
                    
                    // Filter by extension
                    if let Some(ext) = path.extension() {
                        let ext_lower = ext.to_string_lossy().to_lowercase();
                        if !allowed_ext.contains(ext_lower.as_str()) {
                            return ignore::WalkState::Continue;
                        }
                    } else if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                        // Skip files without extensions
                        return ignore::WalkState::Continue;
                    }
                    
                    if let Ok(metadata) = entry.metadata() {
                        let is_folder = metadata.is_dir();
                        let (date_value, date_kind) = get_best_date(&metadata);
                        let name = entry.file_name().to_string_lossy().to_string();
                        let path_str = path.to_string_lossy().to_string();
                        let file_kind = get_file_kind(path, is_folder);
                        
                        if let Ok(mut lock) = results.lock() {
                            lock.push(SearchResult {
                                file_name: name,
                                file_path: path_str,
                                file_size: metadata.len(),
                                is_folder,
                                score: date_value,
                                date_value,
                                date_kind: date_kind.to_string(),
                                file_kind,
                            });
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    }
    
    let mut final_results = results_mutex.lock().unwrap().clone();
    final_results.sort_by(|a, b| b.date_value.cmp(&a.date_value));
    
    // Save to cache
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    
    let cache = FileCache {
        last_updated: now,
        files: final_results.clone(),
    };
    save_cache(&cache);
    
    final_results
}

#[uniffi::export]
pub fn search_files(query: String) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }

    let root_path = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let matcher = Arc::new(SkimMatcherV2::default().smart_case());
    let results = Arc::new(Mutex::new(Vec::new()));

    let walker = WalkBuilder::new(root_path)
        .hidden(true)
        .git_ignore(true)
        .max_depth(Some(6))
        .threads(4)
        .build_parallel();

    let results_clone = results.clone();
    let query_clone = query.clone();

    walker.run(move || {
        let results = results_clone.clone();
        let query = query_clone.clone();
        let matcher = matcher.clone();
        
        Box::new(move |entry_result| {
            if let Ok(entry) = entry_result {
                let file_name = entry.file_name().to_string_lossy();
                
                if let Some(score) = matcher.fuzzy_match(&file_name, &query) {
                    let is_folder = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                    let path = entry.path();
                    let path_str = path.to_string_lossy().to_string();
                    
                    let (size, date_value, date_kind) = if let Ok(metadata) = entry.metadata() {
                        let (dv, dk) = get_best_date(&metadata);
                        (metadata.len(), dv, dk)
                    } else {
                        (0, 0, "Unknown")
                    };
                    
                    let file_kind = get_file_kind(path, is_folder);

                    if let Ok(mut lock) = results.lock() {
                        if lock.len() < 2000 {
                            lock.push(SearchResult {
                                file_name: file_name.to_string(),
                                file_path: path_str,
                                file_size: size,
                                is_folder,
                                score,
                                date_value,
                                date_kind: date_kind.to_string(),
                                file_kind,
                            });
                        } else {
                            return ignore::WalkState::Quit;
                        }
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });

    let mut final_results = results.lock().unwrap().clone();
    final_results.sort_by(|a, b| b.score.cmp(&a.score));
    final_results.truncate(50);

    final_results
}

#[uniffi::export]
pub fn get_recent_files() -> Vec<SearchResult> {
    // First try to return cached data for instant response
    let cache = load_cache();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    
    // Filter to files modified in last 7 days
    let week_ago = now - (60 * 60 * 24 * 7);
    
    let mut recent: Vec<SearchResult> = cache.files
        .into_iter()
        .filter(|f| f.date_value > week_ago)
        .collect();
    
    recent.sort_by(|a, b| b.date_value.cmp(&a.date_value));
    recent.truncate(50);
    
    recent
}