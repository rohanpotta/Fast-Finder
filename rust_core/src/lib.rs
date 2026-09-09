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
mod query;
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

// Only use mtime and birthtime (atime is unreliable on macOS)
fn get_best_date(metadata: &std::fs::Metadata) -> (i64, &'static str) {
    let (mtime, birthtime) = extract_times(metadata);
    if birthtime > mtime {
        (birthtime, "Added")
    } else {
        (mtime, "Modified")
    }
}

/// Raw (mtime, birthtime) extraction.
///
/// `created()` maps to st_birthtime on macOS — when the file came into
/// existence — not st_ctime. Storing both is what makes "sort by date added"
/// answerable at all; collapsing them to whichever is newer (which every read
/// path used to do) throws away the distinction the user actually wants.
fn extract_times(metadata: &std::fs::Metadata) -> (i64, i64) {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let birthtime = metadata
        .created()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (mtime, birthtime)
}

// ============== DATE FIELD SELECTION ==============

/// Which timestamp drives sorting, filtering and the date column.
///
/// `Either` is the historical behaviour: show whichever of the two is more
/// recent. It's a reasonable default for "what did I touch lately" and a
/// terrible one for "what landed here last week", which is why the other two
/// exist and why the choice is the caller's.
///
/// Caveat worth knowing: `Added` is birthtime, i.e. when the file was created.
/// Finder's "Date Added" column is `kMDItemDateAdded` — when the file arrived
/// in *that folder* — which differs for anything moved after creation. Closing
/// that gap means pulling Spotlight metadata into `file_signals`; birthtime is
/// the honest approximation until then.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum DateField {
    Added,
    Modified,
    Either,
}

impl DateField {
    /// SQL expression yielding the timestamp this field selects.
    ///
    /// `birthtime` is 0 when the filesystem couldn't supply one (rare on
    /// APFS/HFS+, but possible on network mounts). Falling back to mtime keeps
    /// those rows in a sensible sort position instead of dumping them at the
    /// very bottom as an unexplained block of zeroes.
    fn value_expr(&self, prefix: &str) -> String {
        match self {
            DateField::Added => {
                format!("COALESCE(NULLIF({p}birthtime, 0), {p}mtime)", p = prefix)
            }
            DateField::Modified => format!("{p}mtime", p = prefix),
            DateField::Either => format!("MAX({p}mtime, {p}birthtime)", p = prefix),
        }
    }

    /// SQL expression yielding the label shown next to the date.
    fn kind_expr(&self, prefix: &str) -> String {
        match self {
            DateField::Added => "'Added'".to_string(),
            DateField::Modified => "'Modified'".to_string(),
            DateField::Either => format!(
                "CASE WHEN {p}birthtime > {p}mtime THEN 'Added' ELSE 'Modified' END",
                p = prefix
            ),
        }
    }
}

// ============== INDEXING POLICY ==============
//
// The full rescan and the incremental updater MUST agree on what belongs in
// the index. If they diverge, `index_paths` will happily insert rows that the
// next `rebuild_index` prunes (or vice versa) and the index starts flickering.
// Everything that decides "is this path in scope" lives in this section.

/// Directory depth below a scan root that we index, matching the walker's
/// `max_depth`. Root itself is depth 0, its children depth 1.
///
/// Was 5, which was survivable only because the roots were three shallow
/// folders. Now that any folder can be a root — `~` included — 5 would cut off
/// most real project trees.
const MAX_SCAN_DEPTH: usize = 10;

/// Folders the indexer walks, from the `settings` table.
///
/// Falls back to the historical three when unset, so an existing install keeps
/// behaving as it did until the user says otherwise.
fn scan_roots() -> Vec<String> {
    if let Ok(conn) = db::open_default() {
        if let Some(json) = db::get_setting(&conn, SETTING_INDEXED_FOLDERS) {
            if let Ok(folders) = serde_json::from_str::<Vec<String>>(&json) {
                if !folders.is_empty() {
                    return folders;
                }
            }
        }
    }
    default_scan_roots()
}

fn default_scan_roots() -> Vec<String> {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    vec![
        format!("{}/Documents", home),
        format!("{}/Downloads", home),
        format!("{}/Desktop", home),
    ]
}

/// Directory names never worth indexing, matched anywhere beneath a root.
///
/// `Library` is the big one: ~920k files of application internals on a typical
/// Mac, which Finder also hides by default. The rest are build and dependency
/// caches. `.gitignore` already prunes most of them inside repos — this list is
/// what catches them when they're *not* in a repo.
///
/// Deliberately excluded from this list: `build`, `dist`, `target`. They're
/// plausible names for real user folders, and gitignore handles the cases that
/// matter, so pruning them by name would cost more than it saves.
fn excluded_dir_names() -> &'static HashSet<&'static str> {
    static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        [
            "Library",
            "node_modules",
            "DerivedData",
            "Pods",
            "__pycache__",
            "venv",
            ".venv",
        ]
        .into_iter()
        .collect()
    })
}

/// Component-level policy, shared by the parallel walker and the single-path
/// check so the full scan and the incremental updater cannot drift apart.
/// Returns the path's depth below `root` when it is acceptable.
///
/// The hidden check here is not redundant with `WalkBuilder::hidden(true)`: an
/// explicit `!` negation in a `.gitignore` (the usual way people commit a
/// `.gitkeep` or `.env.example`) whitelists the file and overrides the walker's
/// hidden filter. Without this backstop the full scan indexed a handful of
/// dotfiles that `index_paths` would always refuse — the two paths disagreeing
/// about the same file, which is the one thing this section exists to prevent.
fn component_depth_if_allowed(path: &Path, root: &str) -> Option<usize> {
    let rel = path.strip_prefix(root).ok()?;
    let mut depth = 0usize;
    for component in rel.components() {
        depth += 1;
        let name = component.as_os_str().to_string_lossy();
        if name.starts_with('.') {
            return None;
        }
        if excluded_dir_names().contains(name.as_ref()) {
            return None;
        }
    }
    Some(depth)
}

/// The scan root containing `path`, if any. Returned so callers can compute
/// depth and scope prune statements to a single root.
fn scan_root_for(path: &Path) -> Option<String> {
    scan_roots()
        .into_iter()
        .find(|root| path.starts_with(root))
}

/// Full eligibility check for a single path: inside a scan root, within the
/// depth limit, not hidden, and not inside an excluded directory.
///
/// There is deliberately no longer an extension allow-list. It made anything
/// unlisted — a `Makefile`, an `.xcodeproj`, a file with no extension at all —
/// permanently unfindable, which is the opposite of replacing Finder.
///
/// This is a *scope* check only. It does not consult `.gitignore`, so it is not
/// sufficient on its own — see `walker_accepts`.
fn is_indexable(path: &Path, _is_dir: bool) -> bool {
    let Some(root) = scan_root_for(path) else {
        return false;
    };
    match component_depth_if_allowed(path, &root) {
        Some(depth) => depth <= MAX_SCAN_DEPTH,
        None => false,
    }
}

/// Build the FFI record plus the raw (mtime, birthtime) we persist alongside it.
fn record_for(path: &Path, metadata: &fs::Metadata) -> (SearchResult, i64, i64) {
    let is_folder = metadata.is_dir();
    let (mtime, birthtime) = extract_times(metadata);
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
        birthtime,
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
const UPSERT_FILE_SQL: &str = "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
     ON CONFLICT(path) DO UPDATE SET
         name       = excluded.name,
         parent_dir = excluded.parent_dir,
         ext        = excluded.ext,
         size       = excluded.size,
         is_dir     = excluded.is_dir,
         mtime      = excluded.mtime,
         birthtime      = excluded.birthtime,
         file_kind  = excluded.file_kind,
         indexed_at = excluded.indexed_at";

/// Bind one record to a prepared `UPSERT_FILE_SQL` statement.
fn upsert_record(
    stmt: &mut rusqlite::Statement,
    r: &SearchResult,
    mtime: i64,
    birthtime: i64,
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
        birthtime,
        r.file_kind,
        indexed_at,
    ])
}

/// Build the gitignore matcher governing `dir`, or None if `dir` isn't inside a
/// git repository (mirroring `WalkBuilder::require_git(true)`).
///
/// Collects every `.gitignore` from the repo root down to `dir`, in that order,
/// so deeper files override shallower ones the way git does.
fn build_gitignore_for(dir: &Path) -> Option<ignore::gitignore::Gitignore> {
    let mut chain: Vec<std::path::PathBuf> = Vec::new();
    let mut repo_root: Option<std::path::PathBuf> = None;
    let mut cursor = Some(dir);
    while let Some(d) = cursor {
        chain.push(d.to_path_buf());
        if d.join(".git").exists() {
            repo_root = Some(d.to_path_buf());
            break;
        }
        cursor = d.parent();
    }
    let root = repo_root?;

    let mut builder = ignore::gitignore::GitignoreBuilder::new(&root);
    for d in chain.iter().rev() {
        let candidate = d.join(".gitignore");
        if candidate.is_file() {
            builder.add(candidate);
        }
    }
    builder.build().ok()
}

/// Is `path` excluded by git's ignore rules?
///
/// `is_indexable` is a scope check and knows nothing about `.gitignore`, so on
/// its own it let every `cargo build` inside an indexed repo pour thousands of
/// `target/` artifacts into the index through FSEvents — ~14k rows, 40% of the
/// index, which each full rescan then dutifully removed. The row count swung
/// depending on whether a scan or an event replay ran last.
///
/// The obvious fix — list the file's parent through `WalkBuilder` and check
/// membership — does not work: the walker never filters its own walk root, so
/// listing a gitignored directory cheerfully yields its contents. Asking the
/// gitignore matcher directly is the crate's supported answer, and
/// `matched_path_or_any_parents` is what makes a rule like `/target/` apply to
/// `target/debug/build-script-build`.
///
/// `cache` is per-batch and keyed by directory, so a batch touching many files
/// in one directory builds the matcher once.
fn git_ignored(
    path: &Path,
    cache: &mut std::collections::HashMap<
        std::path::PathBuf,
        Option<ignore::gitignore::Gitignore>,
    >,
) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let entry = cache
        .entry(parent.to_path_buf())
        .or_insert_with(|| build_gitignore_for(parent));
    match entry {
        Some(gi) => gi
            .matched_path_or_any_parents(path, path.is_dir())
            .is_ignore(),
        None => false,
    }
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
    let root_owned = root.to_string();
    walker.run(move || {
        let sink = sink.clone();
        let root_owned = root_owned.clone();
        Box::new(move |entry_result| {
            if let Ok(entry) = entry_result {
                let path = entry.path();
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                if component_depth_if_allowed(path, &root_owned).is_none() {
                    // Prune the whole subtree rather than skipping entry by
                    // entry — descending into ~/Library to reject each of its
                    // ~920k files costs far more than not descending at all.
                    return if is_dir {
                        ignore::WalkState::Skip
                    } else {
                        ignore::WalkState::Continue
                    };
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
/// Returns rows ordered by the selected date desc; capped to 50k to keep the
/// FFI crossing cheap. If the DB isn't openable yet, returns [].
#[uniffi::export]
pub fn load_cached_index(date_field: DateField) -> Vec<SearchResult> {
    let conn = match db::open_default() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let sql = format!(
        "SELECT path, name, size, is_dir, file_kind,
                {value} AS date_value,
                {kind} AS date_kind,
                {value} AS score
         FROM files
         ORDER BY date_value DESC
         LIMIT 50000",
        value = date_field.value_expr(""),
        kind = date_field.kind_expr(""),
    );
    let mut stmt = match conn.prepare(&sql) {
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
                    for (r, mtime, birthtime) in &collected {
                        let _ = upsert_record(&mut stmt, r, *mtime, *birthtime, now);
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
        // stream is still trustworthy on next launch, and record which policy
        // these rows were collected under.
        let _ = db::set_setting(&conn, SETTING_LAST_FULL_SCAN, &now.to_string());
        let _ = db::set_setting(&conn, SETTING_POLICY_VERSION, INDEX_POLICY_VERSION);
    }

    collected.into_iter().map(|(sr, _, _)| sr).collect()
}

// ============== INCREMENTAL INDEXING ==============

/// Settings keys backing the incremental pipeline.
const SETTING_LAST_FULL_SCAN: &str = "last_full_scan_at";
const SETTING_LAST_EVENT_ID: &str = "fsevents_last_id";
const SETTING_INDEXED_FOLDERS: &str = "indexed_folders";
const SETTING_POLICY_VERSION: &str = "index_policy_version";

/// Bump this whenever *what gets indexed* changes — the depth cap, the
/// exclusion list, the extension policy, the default roots.
///
/// Without it, an upgrade leaves rows that were collected under the old rules
/// sitting in the index until the 24h staleness window happens to expire. That
/// bit us exactly once: dropping the extension allow-list and adding the
/// directory exclusions left ~2.5k node_modules rows from the previous policy
/// visible in search, because a scan had run 23 minutes earlier and
/// `needs_full_rescan` had no way to know the rules had moved underneath it.
/// 2: dropped the extension allow-list, added directory exclusions, depth 5→10.
/// 3: hidden-component backstop, so a gitignore-whitelisted dotfile stays out.
/// 4: single-file events honour .gitignore, so build artifacts stop leaking in.
/// 5: directory events honour it too — the walker never filters its own root.
const INDEX_POLICY_VERSION: &str = "5";

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
    // Directory listings reused across this batch — see `walker_accepts`.
    let mut gitignore_cache: std::collections::HashMap<
        std::path::PathBuf,
        Option<ignore::gitignore::Gitignore>,
    > = std::collections::HashMap::new();

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
                    // The gitignore check matters just as much here as for a
                    // single file: WalkBuilder never filters its own walk root,
                    // so collecting a gitignored directory would happily upsert
                    // everything inside it.
                    if !is_indexable(path, true) || git_ignored(path, &mut gitignore_cache) {
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

                    for (r, mtime, birthtime) in &found {
                        if upsert_record(&mut upsert, r, *mtime, *birthtime, now).is_ok() {
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
                    // Scope first (cheap), then the walker's own verdict, which
                    // is what brings `.gitignore` into agreement with the full
                    // scan. Without the second check, a gitignored build
                    // artifact gets indexed here and pruned by the next rescan.
                    if is_indexable(path, false) && !git_ignored(path, &mut gitignore_cache) {
                        let (r, mtime, birthtime) = record_for(path, &meta);
                        if upsert_record(&mut upsert, &r, mtime, birthtime, now).is_ok() {
                            upserted += 1;
                        }
                    } else {
                        // Newly ignored, newly hidden, or out of scope — drop
                        // any row we were still holding.
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
    // Rows collected under a different indexing policy are not trustworthy no
    // matter how recently they were written.
    if db::get_setting(&conn, SETTING_POLICY_VERSION).as_deref() != Some(INDEX_POLICY_VERSION) {
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

// ============== INDEXED FOLDERS ==============

/// Folders currently indexed. This is the real list the indexer uses — the
/// Settings UI reads it from here rather than keeping its own copy, which is
/// how it ended up displaying three folders it had no influence over.
#[uniffi::export]
pub fn get_indexed_folders() -> Vec<String> {
    scan_roots()
}

/// Outcome of changing the folder list, so the UI can report what happened
/// instead of silently succeeding.
#[derive(uniffi::Record, Clone)]
pub struct FolderUpdate {
    pub accepted: Vec<String>,
    pub rejected: Vec<String>,
    /// Rows dropped because they no longer sit under any indexed folder.
    pub pruned: u32,
}

/// Replace the indexed-folder list.
///
/// Rejects anything that isn't an existing directory or that `safety` refuses
/// as a read root, and reports it rather than failing the whole call — one bad
/// entry shouldn't discard the user's other choices.
///
/// Rows outside the new roots are pruned immediately, and the full-scan stamp
/// is cleared so the caller's next `needs_full_rescan` returns true: widening
/// the roots means there is now content we've never walked.
#[uniffi::export]
pub fn set_indexed_folders(folders: Vec<String>) -> FolderUpdate {
    let mut accepted: Vec<String> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();

    for raw in &folders {
        match safety::check_index_root(raw) {
            Ok(p) if p.is_dir() => {
                let s = p.to_string_lossy().into_owned();
                if !accepted.contains(&s) {
                    accepted.push(s);
                }
            }
            _ => rejected.push(raw.clone()),
        }
    }

    // Refuse to persist an empty list — that would index nothing at all and
    // look identical to a broken app. Fall back to the defaults instead.
    let to_store = if accepted.is_empty() {
        default_scan_roots()
    } else {
        accepted.clone()
    };

    let mut pruned: u32 = 0;
    if let Ok(conn) = db::open_default() {
        if let Ok(json) = serde_json::to_string(&to_store) {
            let _ = db::set_setting(&conn, SETTING_INDEXED_FOLDERS, &json);
        }

        // Drop everything that is no longer under any root. Built as one
        // NOT(... OR ...) so a file kept by any root survives.
        let mut clauses: Vec<String> = Vec::new();
        let mut binds: Vec<String> = Vec::new();
        for (i, root) in to_store.iter().enumerate() {
            let n = i + 1;
            clauses.push(format!("(path > ?{n} || '/' AND path < ?{n} || '0')", n = n));
            binds.push(root.clone());
        }
        let sql = format!(
            "DELETE FROM files WHERE NOT ({})",
            clauses.join(" OR ")
        );
        let params: Vec<&dyn rusqlite::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        pruned = conn.execute(&sql, params.as_slice()).unwrap_or(0) as u32;

        // Force a full walk next time: new roots have never been visited.
        let _ = conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![SETTING_LAST_FULL_SCAN],
        );
    }

    FolderUpdate { accepted, rejected, pruned }
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

/// Result cap for a single search. Generous enough that a filter-only query
/// ("every PDF") is useful rather than an arbitrary top-50 slice.
const SEARCH_LIMIT: usize = 200;

/// One parsed filter, for the UI to render as a removable chip.
#[derive(uniffi::Record, Clone)]
pub struct QueryChip {
    /// Human-readable, e.g. "PDF" or "added < 7d ago".
    pub label: String,
    /// The original token, so the UI can strip it from the query.
    pub token: String,
}

/// How the search bar's raw text was understood.
#[derive(uniffi::Record, Clone)]
pub struct ParsedQuery {
    /// The part that goes to full-text search.
    pub text: String,
    pub chips: Vec<QueryChip>,
    /// Tokens that looked like filters but couldn't be read, e.g. `added:banana`.
    pub invalid: Vec<String>,
}

/// Parse search-bar input without running it, so the UI can show what it
/// understood as the user types.
#[uniffi::export]
pub fn parse_query(raw: String) -> ParsedQuery {
    let p = query::parse(&raw);
    ParsedQuery {
        text: p.text,
        chips: p
            .filters
            .iter()
            .map(|f| QueryChip { label: f.label.clone(), token: f.token.clone() })
            .collect(),
        invalid: p.invalid,
    }
}

/// Search the index. `query` may mix free text with filter tokens
/// (`report kind:pdf added:<7d`); filters alone are a valid query.
///
/// Everything happens in SQLite against indexed columns — no network, no model
/// call. That's the point: `kind:pdf added:<7d` used to be expressible only as
/// prose through the AI bar.
#[uniffi::export]
pub fn search_files(query: String, date_field: DateField) -> Vec<SearchResult> {
    let parsed = query::parse(&query);
    let fts = build_fts_query(&parsed.text);

    // Nothing to go on at all. Filters alone are fine; empty input is not.
    if fts.is_none() && parsed.filters.is_empty() {
        return Vec::new();
    }
    let Ok(conn) = db::open_default() else {
        return Vec::new();
    };

    let now = blocks::now_ts();
    let prefix = if fts.is_some() { "f." } else { "" };
    let (mut clauses, filter_params) = query::compile(&parsed.filters, prefix, now);
    let mut params: Vec<rusqlite::types::Value> = Vec::new();

    let sql = if let Some(fts) = fts {
        // bm25() returns negative numbers; lower = better. Invert into a
        // positive score so sort-by-score-desc reads naturally. The selected
        // date breaks ties.
        params.push(rusqlite::types::Value::Text(fts));
        params.extend(filter_params);
        clauses.insert(0, "files_fts MATCH ?".to_string());
        format!(
            "SELECT f.path, f.name, f.size, f.is_dir, f.file_kind,
                    {value} AS date_value,
                    {kind} AS date_kind,
                    CAST(-bm25(files_fts) * 1000 AS INTEGER) AS score
             FROM files_fts
             JOIN files f ON f.id = files_fts.rowid
             WHERE {where_clause}
             ORDER BY score DESC, date_value DESC
             LIMIT {limit}",
            value = date_field.value_expr("f."),
            kind = date_field.kind_expr("f."),
            where_clause = clauses.join(" AND "),
            limit = SEARCH_LIMIT,
        )
    } else {
        // Filter-only: no relevance to rank by, so order by the chosen date.
        params.extend(filter_params);
        format!(
            "SELECT path, name, size, is_dir, file_kind,
                    {value} AS date_value,
                    {kind} AS date_kind,
                    {value} AS score
             FROM files
             WHERE {where_clause}
             ORDER BY date_value DESC
             LIMIT {limit}",
            value = date_field.value_expr(""),
            kind = date_field.kind_expr(""),
            where_clause = clauses.join(" AND "),
            limit = SEARCH_LIMIT,
        )
    };

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params_from_iter(params), map_row)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Recent files by the selected date field.
///
/// `within_days` bounds the window; 0 means "no lower bound", which is what
/// makes "everything by date added" answerable rather than silently capped.
#[uniffi::export]
pub fn get_recent_files(date_field: DateField, within_days: u32) -> Vec<SearchResult> {
    let Ok(conn) = db::open_default() else {
        return Vec::new();
    };
    let cutoff = if within_days == 0 {
        0
    } else {
        blocks::now_ts() - (60 * 60 * 24 * within_days as i64)
    };

    // The cutoff is applied to the same expression that's selected and sorted
    // on, so switching the field changes which files qualify — not just their
    // order. That's the whole point: "added this week" and "modified this
    // week" are different sets, and the old MAX() collapsed them into one.
    let sql = format!(
        "SELECT path, name, size, is_dir, file_kind,
                {value} AS date_value,
                {kind} AS date_kind,
                {value} AS score
         FROM files
         WHERE {value} > ?1
           AND is_dir = 0
         ORDER BY date_value DESC
         LIMIT 50",
        value = date_field.value_expr(""),
        kind = date_field.kind_expr(""),
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    stmt.query_map(params![cutoff], map_row)
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
        let results = search_files("".to_string(), DateField::Either);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_files_whitespace_query() {
        let results = search_files("   ".to_string(), DateField::Either);
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
                "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
                 VALUES ('/tmp/Report Q3.pdf', 'Report Q3.pdf', '/tmp', 'pdf', 1024, 0, 1700000000, 0, 'PDF Document', 1700000000)",
                [],
            ).unwrap();
        }
        let hits = search_files("repor".to_string(), DateField::Either);
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
            "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
             VALUES ('/tmp/fresh.txt', 'fresh.txt', '/tmp', 'txt', 1, 0, ?1, 0, 'Plain Text', ?1),
                    ('/tmp/old.txt',   'old.txt',   '/tmp', 'txt', 1, 0, ?2, 0, 'Plain Text', ?2)",
            rusqlite::params![yesterday, last_year],
        ).unwrap();

        let recents = get_recent_files(DateField::Either, 7);
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
        // No extension allow-list any more: an unusual extension, or none at
        // all, is still findable. This is the point of dropping it.
        assert!(is_indexable(&docs.join("core.dump"), false));
        assert!(is_indexable(&docs.join("Makefile"), false));
        assert!(is_indexable(&docs.join("Projects"), true));
        // Hidden anywhere in the relative path.
        assert!(!is_indexable(&docs.join(".secret.pdf"), false));
        assert!(!is_indexable(&docs.join(".cache/report.pdf"), false));
        // Excluded directory names, at any depth.
        assert!(!is_indexable(&docs.join("Library/prefs.plist"), false));
        assert!(!is_indexable(&docs.join("app/node_modules/left-pad/index.js"), false));
        assert!(!is_indexable(&docs.join("py/__pycache__/mod.pyc"), false));
        // ...but a name merely containing an excluded word is fine.
        assert!(is_indexable(&docs.join("LibraryNotes/report.pdf"), false));
        // Outside every scan root.
        assert!(!is_indexable(&_scope.path().join("Movies/clip.mp4"), false));
        // Past the depth budget (root + 11 components).
        assert!(!is_indexable(&docs.join("a/b/c/d/e/f/g/h/i/j/k.pdf"), false));
        assert!(is_indexable(&docs.join("a/b/c/d/e/f/g/h/i/j.pdf"), false));
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
        let hits = search_files("notes".to_string(), DateField::Either);
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
        assert!(search_files("gone".to_string(), DateField::Either).is_empty());
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
        assert!(search_files("b".to_string(), DateField::Either).is_empty());
        assert_eq!(search_files("a".to_string(), DateField::Either).len(), 1);
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
        assert!(search_files("a".to_string(), DateField::Either).is_empty());
    }

    #[test]
    fn test_index_paths_follows_a_rename_to_both_sides() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let old = docs.join("notes.md");
        fs::write(&old, "x").unwrap();
        assert_eq!(index_paths(vec![old.to_string_lossy().into_owned()]).upserted, 1);

        let new = docs.join("minutes.md");
        fs::rename(&old, &new).unwrap();

        // FSEvents reports both sides of a rename in one batch.
        let update = index_paths(vec![
            old.to_string_lossy().into_owned(),
            new.to_string_lossy().into_owned(),
        ]);
        assert_eq!(update.removed, 1, "the old path is gone from disk");
        assert_eq!(update.upserted, 1, "the new path takes its place");
        assert!(search_files("notes".to_string(), DateField::Either).is_empty());
        assert_eq!(search_files("minutes".to_string(), DateField::Either).len(), 1);
    }

    #[test]
    fn test_full_scan_excludes_a_gitignore_whitelisted_dotfile() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let proj = docs.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        // The idiom that defeated WalkBuilder's hidden filter: ignore the
        // directory's contents but explicitly whitelist a dotfile.
        fs::write(proj.join(".gitignore"), "uploads/*\n!uploads/.gitkeep\n").unwrap();
        let uploads = proj.join("uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        fs::write(uploads.join(".gitkeep"), "").unwrap();
        fs::write(proj.join("main.rs"), "fn main(){}").unwrap();

        rebuild_index();

        let conn = db::open_default().unwrap();
        let hidden: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE name LIKE '.%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hidden, 0, "a whitelisted dotfile must still stay out of the index");
        // The scan itself worked.
        assert_eq!(search_files("main".to_string(), DateField::Either).len(), 1);
    }

    #[test]
    fn test_index_paths_skips_excluded_directories() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let nm = docs.join("app/node_modules/left-pad");
        std::fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("index.js"), "x").unwrap();
        fs::write(docs.join("app/main.js"), "y").unwrap();

        index_paths(vec![docs.join("app").to_string_lossy().into_owned()]);

        assert_eq!(search_files("main".to_string(), DateField::Either).len(), 1);
        assert!(
            search_files("left".to_string(), DateField::Either).is_empty(),
            "node_modules must never reach the index"
        );
    }

    // MARK: - indexed folders

    #[test]
    fn test_set_indexed_folders_accepts_dirs_and_rejects_junk() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let movies = _scope.path().join("Movies");
        std::fs::create_dir_all(&movies).unwrap();
        let a_file = docs.join("not-a-dir.txt");
        fs::write(&a_file, "x").unwrap();

        let update = set_indexed_folders(vec![
            docs.to_string_lossy().into_owned(),
            movies.to_string_lossy().into_owned(),
            a_file.to_string_lossy().into_owned(),      // a file, not a folder
            "relative/path".to_string(),                 // not absolute
            "/etc".to_string(),                          // system dir
            _scope.path().join(".ssh").to_string_lossy().into_owned(), // sensitive
        ]);

        assert_eq!(update.accepted.len(), 2, "the two real directories");
        assert_eq!(update.rejected.len(), 4, "file, relative, system, sensitive");

        // And the accepted list is what the indexer will actually walk.
        let roots = get_indexed_folders();
        assert!(roots.iter().any(|r| r.ends_with("Documents")));
        assert!(roots.iter().any(|r| r.ends_with("Movies")));
    }

    #[test]
    fn test_set_indexed_folders_prunes_rows_outside_the_new_roots() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let movies = _scope.path().join("Movies");
        std::fs::create_dir_all(&movies).unwrap();
        fs::write(docs.join("keep.md"), "k").unwrap();
        fs::write(movies.join("drop.md"), "d").unwrap();

        set_indexed_folders(vec![
            docs.to_string_lossy().into_owned(),
            movies.to_string_lossy().into_owned(),
        ]);
        rebuild_index();
        assert_eq!(search_files("keep".to_string(), DateField::Either).len(), 1);
        assert_eq!(search_files("drop".to_string(), DateField::Either).len(), 1);

        // Narrowing the roots must clear out what they no longer cover.
        let update = set_indexed_folders(vec![docs.to_string_lossy().into_owned()]);
        assert!(update.pruned > 0, "rows under Movies should have been pruned");
        assert_eq!(search_files("keep".to_string(), DateField::Either).len(), 1);
        assert!(search_files("drop".to_string(), DateField::Either).is_empty());
    }

    #[test]
    fn test_set_indexed_folders_never_persists_an_empty_list() {
        let _scope = db_scope();
        // All rejected -> fall back to defaults rather than indexing nothing.
        let update = set_indexed_folders(vec!["/etc".to_string()]);
        assert!(update.accepted.is_empty());
        assert_eq!(get_indexed_folders(), default_scan_roots());
    }

    #[test]
    fn test_a_policy_change_forces_a_full_rescan() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        fs::write(docs.join("a.md"), "a").unwrap();
        rebuild_index();
        assert!(!needs_full_rescan(), "fresh scan under the current policy");

        // Simulate an upgrade: rows on disk were collected under older rules.
        let conn = db::open_default().unwrap();
        db::set_setting(&conn, SETTING_POLICY_VERSION, "1").unwrap();
        assert!(
            needs_full_rescan(),
            "rows from a previous indexing policy must be re-walked regardless of age"
        );
    }

    #[test]
    fn test_changing_folders_forces_a_full_rescan() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        fs::write(docs.join("a.md"), "a").unwrap();
        rebuild_index();
        assert!(!needs_full_rescan(), "just scanned");

        set_indexed_folders(vec![docs.to_string_lossy().into_owned()]);
        assert!(
            needs_full_rescan(),
            "new roots have never been walked, so a full scan is required"
        );
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
        assert_eq!(search_files("b".to_string(), DateField::Either).len(), 1, "precondition");

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

    /// The bug this pins down: a `cargo build` inside an indexed repo emits
    /// thousands of FSEvents under a gitignored `target/`. `index_paths` judged
    /// them on scope alone, indexed them all, and the next full rescan removed
    /// them again — the index swung by ~14k rows (40%) depending on which ran
    /// last, and gitignored build output showed up in search results.
    #[test]
    fn test_file_events_honour_gitignore_like_the_full_scan_does() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        let repo = docs.join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap(); // marks it a repo
        fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
        fs::write(repo.join("main.rs"), "fn main() {}").unwrap();
        let target = repo.join("target/debug");
        std::fs::create_dir_all(&target).unwrap();
        let artifact = target.join("build-script-build");
        fs::write(&artifact, "binary").unwrap();

        // Baseline: the full scan already excludes the artifact.
        rebuild_index();
        let conn = db::open_default().unwrap();
        let scanned: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        let artifact_rows = |c: &rusqlite::Connection| -> i64 {
            c.query_row(
                "SELECT COUNT(*) FROM files WHERE path LIKE '%/target/%'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(artifact_rows(&conn), 0, "full scan must skip gitignored target/");

        // The event the build emits. It must not be indexed.
        let update = index_paths(vec![artifact.to_string_lossy().into_owned()]);
        assert_eq!(
            artifact_rows(&conn),
            0,
            "a gitignored build artifact must not enter the index via a file event"
        );
        assert_eq!(update.upserted, 0);

        // The same must hold for a *directory* event. FSEvents reports
        // directories too, and collecting one walks it — with the walk root
        // unfiltered, so this leaked the entire ignored subtree.
        let dir_update = index_paths(vec![target.to_string_lossy().into_owned()]);
        assert_eq!(
            artifact_rows(&conn),
            0,
            "a directory event on a gitignored folder must not index its contents"
        );
        assert_eq!(dir_update.upserted, 0);

        // A non-ignored sibling still indexes normally — we haven't just
        // broken incremental indexing to win the test.
        fs::write(repo.join("lib.rs"), "pub fn x() {}").unwrap();
        let ok = index_paths(vec![repo.join("lib.rs").to_string_lossy().into_owned()]);
        assert_eq!(ok.upserted, 1, "ordinary files must still be indexed");

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, scanned + 1, "exactly the one new real file");
    }

    #[test]
    fn test_directory_event_does_not_erode_the_index() {
        let _scope = db_scope();
        let docs = docs_root(&_scope);
        // A tree deep enough that a subtree walk has a smaller depth budget
        // than the full scan had.
        let deep = docs.join("proj/a/b/c/d");
        std::fs::create_dir_all(&deep).unwrap();
        for (i, dir) in [
            docs.join("proj"),
            docs.join("proj/a"),
            docs.join("proj/a/b"),
            docs.join("proj/a/b/c"),
            deep.clone(),
        ]
        .iter()
        .enumerate()
        {
            fs::write(dir.join(format!("f{}.md", i)), "x").unwrap();
        }

        rebuild_index();
        let conn = db::open_default().unwrap();
        let after_scan: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert!(after_scan > 5, "sanity: the tree was indexed");

        // An FSEvents directory notification for an unchanged directory must be
        // a no-op. If the subtree walk sees less than the index holds, the
        // diff-prune deletes the difference and files silently vanish.
        index_paths(vec![docs.join("proj").to_string_lossy().into_owned()]);
        let after_event: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after_event, after_scan,
            "a directory event on an unchanged tree must not remove rows"
        );
    }

    // MARK: - filter tokens end to end

    /// Seed a spread of files so filters have something to discriminate.
    fn seed_for_filters() -> i64 {
        let now = blocks::now_ts();
        let conn = db::open_default().unwrap();
        let day = 86_400i64;
        let rows: [(&str, &str, &str, i64, i64, i64); 5] = [
            // path, name, ext, size, mtime, birthtime
            ("/tmp/d/report.pdf",   "report.pdf",   "pdf",  2_000_000, now - day,       now - day),
            ("/tmp/d/old.pdf",      "old.pdf",      "pdf",        500, now - day * 400, now - day * 400),
            ("/tmp/d/photo.png",    "photo.png",    "png",  5_000_000, now - day * 2,   now - day * 2),
            ("/tmp/d/notes.md",     "notes.md",     "md",         100, now - day * 3,   now - day * 3),
            ("/tmp/other/report.md","report.md",    "md",         100, now - day,       now - day),
        ];
        for (path, name, ext, size, mtime, birth) in rows {
            conn.execute(
                "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
                 VALUES (?1, ?2, '/tmp/d', ?3, ?4, 0, ?5, ?6, 'x', ?5)",
                rusqlite::params![path, name, ext, size, mtime, birth],
            )
            .unwrap();
        }
        // A folder, to prove is:folder/is:file discriminate.
        conn.execute(
            "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
             VALUES ('/tmp/d/archive', 'archive', '/tmp/d', NULL, 0, 1, ?1, ?1, 'Folder', ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        now
    }

    fn names(results: &[SearchResult]) -> Vec<String> {
        let mut v: Vec<String> = results.iter().map(|r| r.file_name.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn test_filter_only_query_needs_no_search_text() {
        let _scope = db_scope();
        seed_for_filters();
        let hits = search_files("kind:pdf".to_string(), DateField::Either);
        assert_eq!(names(&hits), vec!["old.pdf", "report.pdf"]);
    }

    #[test]
    fn test_text_and_filter_combine() {
        let _scope = db_scope();
        seed_for_filters();
        // Two files match "report"; only one is a PDF.
        assert_eq!(
            names(&search_files("report".to_string(), DateField::Either)).len(),
            2
        );
        assert_eq!(
            names(&search_files("report kind:pdf".to_string(), DateField::Either)),
            vec!["report.pdf"]
        );
    }

    #[test]
    fn test_age_filter_excludes_old_files() {
        let _scope = db_scope();
        seed_for_filters();
        // old.pdf is 400 days old, report.pdf is 1 day old.
        assert_eq!(
            names(&search_files("kind:pdf added:<7d".to_string(), DateField::Either)),
            vec!["report.pdf"]
        );
        assert_eq!(
            names(&search_files("kind:pdf added:>1y".to_string(), DateField::Either)),
            vec!["old.pdf"]
        );
    }

    #[test]
    fn test_size_and_kind_group_filters() {
        let _scope = db_scope();
        seed_for_filters();
        assert_eq!(
            names(&search_files("size:>1mb".to_string(), DateField::Either)),
            vec!["photo.png", "report.pdf"]
        );
        // kind:image expands to an extension set.
        assert_eq!(
            names(&search_files("kind:image".to_string(), DateField::Either)),
            vec!["photo.png"]
        );
    }

    #[test]
    fn test_is_folder_and_in_path_filters() {
        let _scope = db_scope();
        seed_for_filters();
        assert_eq!(
            names(&search_files("is:folder".to_string(), DateField::Either)),
            vec!["archive"]
        );
        // `in:` scopes to a path fragment.
        assert_eq!(
            names(&search_files("in:other".to_string(), DateField::Either)),
            vec!["report.md"]
        );
    }

    #[test]
    fn test_filter_values_cannot_inject_sql() {
        let _scope = db_scope();
        seed_for_filters();
        // If values were interpolated rather than bound, this would drop the
        // table. It must simply match nothing.
        let hits = search_files(
            "ext:pdf'); DROP TABLE files; --".to_string(),
            DateField::Either,
        );
        assert!(hits.is_empty());
        let conn = db::open_default().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 6, "table must still be intact");
    }

    #[test]
    fn test_empty_query_still_returns_nothing() {
        let _scope = db_scope();
        seed_for_filters();
        assert!(search_files("".to_string(), DateField::Either).is_empty());
        assert!(search_files("   ".to_string(), DateField::Either).is_empty());
    }

    #[test]
    fn test_parse_query_exposes_chips_and_invalid_tokens() {
        let parsed = parse_query("report kind:pdf added:<7d added:banana".to_string());
        assert_eq!(parsed.text, "report");
        assert_eq!(parsed.chips.len(), 2);
        assert_eq!(parsed.chips[0].label, "PDF");
        assert_eq!(parsed.chips[0].token, "kind:pdf");
        assert_eq!(parsed.invalid, vec!["added:banana".to_string()]);
    }

    // MARK: - date field selection

    /// Two files that disagree about which date is which:
    ///   `old_file_touched_today` was added long ago, modified today
    ///   `new_file_untouched`     was added today, never modified since
    /// Under the old MAX(mtime, birthtime) both look "recent" and there was no
    /// way to tell them apart. That's the complaint this phase exists to fix.
    fn seed_divergent_dates(now: i64) {
        let conn = db::open_default().unwrap();
        let long_ago = now - 60 * 60 * 24 * 300;
        conn.execute(
            "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
             VALUES ('/tmp/old_added.txt', 'old_added.txt', '/tmp', 'txt', 1, 0, ?1, ?2, 'Plain Text', ?1),
                    ('/tmp/new_added.txt', 'new_added.txt', '/tmp', 'txt', 1, 0, ?2, ?1, 'Plain Text', ?1)",
            rusqlite::params![now, long_ago],
        )
        .unwrap();
    }

    #[test]
    fn test_recents_by_added_and_modified_select_different_files() {
        let _scope = db_scope();
        let now = blocks::now_ts();
        seed_divergent_dates(now);

        // Modified this week: only the file touched today.
        let modified = get_recent_files(DateField::Modified, 7);
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].file_name, "old_added.txt");
        assert_eq!(modified[0].date_kind, "Modified");

        // Added this week: only the file that landed today. Different set,
        // not merely a different order.
        let added = get_recent_files(DateField::Added, 7);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].file_name, "new_added.txt");
        assert_eq!(added[0].date_kind, "Added");

        // Either: both qualify, which is exactly the mushy old behaviour.
        assert_eq!(get_recent_files(DateField::Either, 7).len(), 2);
    }

    #[test]
    fn test_within_days_zero_means_no_lower_bound() {
        let _scope = db_scope();
        let now = blocks::now_ts();
        seed_divergent_dates(now);

        // Both files were added at some point, however long ago.
        assert_eq!(get_recent_files(DateField::Added, 0).len(), 2);
        assert_eq!(get_recent_files(DateField::Modified, 0).len(), 2);
    }

    #[test]
    fn test_search_orders_ties_by_the_selected_date() {
        let _scope = db_scope();
        let now = blocks::now_ts();
        let long_ago = now - 60 * 60 * 24 * 300;
        let conn = db::open_default().unwrap();
        // Identical names so bm25 ties and the date field breaks it.
        conn.execute(
            "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
             VALUES ('/tmp/a/report.txt', 'report.txt', '/tmp/a', 'txt', 1, 0, ?1, ?2, 'Plain Text', ?1),
                    ('/tmp/b/report.txt', 'report.txt', '/tmp/b', 'txt', 1, 0, ?2, ?1, 'Plain Text', ?1)",
            rusqlite::params![now, long_ago],
        )
        .unwrap();

        let by_modified = search_files("report".to_string(), DateField::Modified);
        assert_eq!(by_modified.len(), 2);
        assert_eq!(by_modified[0].file_path, "/tmp/a/report.txt");

        let by_added = search_files("report".to_string(), DateField::Added);
        assert_eq!(by_added.len(), 2);
        assert_eq!(by_added[0].file_path, "/tmp/b/report.txt", "added flips the tie-break");
    }

    #[test]
    fn test_added_falls_back_to_mtime_when_birthtime_is_missing() {
        let _scope = db_scope();
        let now = blocks::now_ts();
        let conn = db::open_default().unwrap();
        // birthtime 0 = filesystem couldn't supply one.
        conn.execute(
            "INSERT INTO files (path, name, parent_dir, ext, size, is_dir, mtime, birthtime, file_kind, indexed_at)
             VALUES ('/tmp/nobirth.txt', 'nobirth.txt', '/tmp', 'txt', 1, 0, ?1, 0, 'Plain Text', ?1)",
            rusqlite::params![now],
        )
        .unwrap();

        let added = get_recent_files(DateField::Added, 7);
        assert_eq!(added.len(), 1, "must not be stranded at timestamp zero");
        assert_eq!(added[0].date_value, now, "falls back to mtime");
    }

    #[test]
    fn test_last_event_id_round_trip() {
        let _scope = db_scope();
        assert_eq!(last_event_id(), 0, "no stored id yet");
        set_last_event_id(918_273_645);
        assert_eq!(last_event_id(), 918_273_645);
    }
}



