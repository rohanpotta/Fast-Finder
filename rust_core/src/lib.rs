use std::env;
use std::sync::{Arc, Mutex};
use ignore::WalkBuilder;

uniffi::setup_scaffolding!();

#[derive(uniffi::Record, Clone)]
pub struct SearchResult {
    pub file_name: String,
    pub file_path: String,
}

#[uniffi::export]
pub fn search_files(query: String) -> Vec<SearchResult> {
    let query_lower = query.to_lowercase();
    // Fallback to "." if HOME isn't found
    let root_path = env::var("HOME").unwrap_or_else(|_| ".".to_string());

    // Thread-safe container to hold results
    let results = Arc::new(Mutex::new(Vec::new()));

    // 1. Build the walker with your specific limits
    let walker = WalkBuilder::new(root_path)
        .hidden(true)       // Skip hidden files (.DS_Store, etc.)
        .git_ignore(true)   // Respect .gitignore rules
        .max_depth(Some(5)) // Limit recursion depth
        .threads(4)         // Use 4 threads
        .build_parallel();

    // 2. Run the walker in parallel
    let results_clone = results.clone();
    walker.run(move || {
        let results = results_clone.clone();
        let query_lower = query_lower.clone();
        
        Box::new(move |entry_result| {
            if let Ok(entry) = entry_result {
                // We only care about files, not directories
                if let Some(file_type) = entry.file_type() {
                    if file_type.is_file() {
                        let file_name = entry.file_name().to_string_lossy();
                        
                        // 3. Match Logic
                        if file_name.to_lowercase().contains(&query_lower) {
                            let path = entry.path().to_string_lossy().to_string();
                            
                            // Lock and push
                            if let Ok(mut lock) = results.lock() {
                                // Keep the safety limit (100 items) to prevent UI flooding
                                if lock.len() < 100 {
                                    lock.push(SearchResult {
                                        file_name: file_name.to_string(),
                                        file_path: path,
                                    });
                                } else {
                                    return ignore::WalkState::Quit;
                                }
                            }
                        }
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });

    let final_results = results.lock().unwrap().clone();
    final_results
}