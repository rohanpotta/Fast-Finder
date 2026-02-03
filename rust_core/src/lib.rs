use std::env;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use ignore::WalkBuilder;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

uniffi::setup_scaffolding!();

#[derive(uniffi::Record, Clone)]
pub struct SearchResult {
    pub file_name: String,
    pub file_path: String,
    pub file_size: u64,
    pub is_folder: bool,
    pub score: i64,
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
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    let path = entry.path().to_string_lossy().to_string();

                    if let Ok(mut lock) = results.lock() {
                        if lock.len() < 2000 {
                            lock.push(SearchResult {
                                file_name: file_name.to_string(),
                                file_path: path,
                                file_size: size,
                                is_folder: is_folder,
                                score: score,
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
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    
    // Only scan folders where real user files live
    let user_folders = vec![
        format!("{}/Documents", home),
        format!("{}/Downloads", home),
        format!("{}/Desktop", home),
    ];
    
    // File extensions that represent real user files
    let allowed_extensions: std::collections::HashSet<&str> = [
        // Documents
        "pdf", "doc", "docx", "txt", "rtf", "md", "pages", "odt",
        // Spreadsheets
        "xls", "xlsx", "csv", "numbers",
        // Presentations
        "ppt", "pptx", "key",
        // Images
        "jpg", "jpeg", "png", "gif", "heic", "webp", "svg", "psd", "ai",
        // Videos
        "mp4", "mov", "avi", "mkv", "webm",
        // Audio
        "mp3", "wav", "aac", "flac", "m4a",
        // Code
        "py", "js", "ts", "rs", "swift", "java", "go", "html", "css", "json",
        // Archives
        "zip", "tar", "gz", "rar", "7z", "dmg",
    ].iter().cloned().collect();

    let results_mutex = Arc::new(Mutex::new(Vec::new()));

    for folder in user_folders {
        if !std::path::Path::new(&folder).exists() {
            continue;
        }
        
        let results_clone = results_mutex.clone();
        let allowed_ext = allowed_extensions.clone();

        let walker = WalkBuilder::new(&folder)
            .hidden(true)
            .git_ignore(true)
            .max_depth(Some(4))
            .threads(2)
            .build_parallel();

        walker.run(move || {
            let results = results_clone.clone();
            let allowed_ext = allowed_ext.clone();
            
            Box::new(move |entry_result| {
                if let Ok(entry) = entry_result {
                    let path = entry.path();
                    
                    // Only include files with allowed extensions
                    if let Some(ext) = path.extension() {
                        let ext_lower = ext.to_string_lossy().to_lowercase();
                        if !allowed_ext.contains(ext_lower.as_str()) {
                            return ignore::WalkState::Continue;
                        }
                    } else {
                        return ignore::WalkState::Continue;
                    }

                    if let Ok(metadata) = entry.metadata() {
                        if metadata.is_file() {
                            if let Ok(modified) = metadata.modified() {
                                if let Ok(duration) = SystemTime::now().duration_since(modified) {
                                    // Last 7 days
                                    if duration.as_secs() < 60 * 60 * 24 * 7 {
                                        let name = entry.file_name().to_string_lossy().to_string();
                                        let path_str = path.to_string_lossy().to_string();
                                        
                                        let score = modified
                                            .duration_since(SystemTime::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs() as i64;

                                        if let Ok(mut lock) = results.lock() {
                                            if lock.len() < 100 {
                                                lock.push(SearchResult {
                                                    file_name: name,
                                                    file_path: path_str,
                                                    file_size: metadata.len(),
                                                    is_folder: false,
                                                    score: score,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    }

    let mut final_results = results_mutex.lock().unwrap().clone();
    final_results.sort_by(|a, b| b.score.cmp(&a.score));
    final_results.truncate(20);

    final_results
}