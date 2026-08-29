//! Defense-in-depth path validation for filesystem mutations.
//!
//! The threat model here is two-fold:
//! 1. A buggy or hallucinated AI plan that targets the wrong path
//!    (e.g. "trash all PDFs" matches /Library/Documentation/*.pdf).
//! 2. A malicious caller — including a prompt-injected one — that
//!    constructs source_paths pointing at sensitive files.
//!
//! These checks are NOT a substitute for user confirmation in the UI.
//! They're the last layer that says "this should never happen under any
//! UX flow we can think of, refuse and surface an error."

use std::env;
use std::path::{Component, Path, PathBuf};

/// Soft cap on how many paths a single FS-op call may touch. AI flows
/// occasionally try to enumerate huge directories; require explicit
/// chunking above this and surface a clear error.
pub const MAX_BULK_PATHS: usize = 500;

#[derive(Debug, PartialEq, Eq)]
pub enum PathRejection {
    Empty,
    NotAbsolute,
    Root,
    SystemDir,        // /System, /Library, /usr, /bin, /sbin, /etc, /var, /dev, /private
    OutsideHome,      // not under $HOME (and not /tmp or /var/folders)
    Sensitive,        // ~/.ssh, ~/Library/Keychains, ~/.aws, ~/.config, etc.
    AppDataDir,       // our own ~/.fast-finder/* — refuse to let FS ops touch the index
    HomeRootDotfile,  // ~/.zshrc, ~/.profile and similar at $HOME root
    TooManyPaths,     // bulk cap exceeded
}

impl PathRejection {
    pub fn explain(&self) -> &'static str {
        match self {
            PathRejection::Empty => "empty path",
            PathRejection::NotAbsolute => "path must be absolute",
            PathRejection::Root => "refusing to operate on filesystem root",
            PathRejection::SystemDir => "path is in a protected system directory",
            PathRejection::OutsideHome => "path is outside the user's home directory",
            PathRejection::Sensitive => "path is in a sensitive location (credentials, keys, config)",
            PathRejection::AppDataDir => "path is inside the Fast-Finder index — refusing to touch",
            PathRejection::HomeRootDotfile => "refusing to operate on a top-level home dotfile",
            PathRejection::TooManyPaths => "too many paths in one operation",
        }
    }
}

fn home_dir() -> PathBuf {
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| "/".to_string()))
}

fn app_data_dir() -> PathBuf {
    home_dir().join(".fast-finder")
}

/// Forbidden prefixes that no FS op may touch. Anchored absolute paths.
/// Order matters only for readability — the check is `starts_with` for each.
fn system_prefixes() -> [&'static str; 9] {
    ["/System", "/Library", "/usr", "/bin", "/sbin", "/etc", "/var", "/dev", "/private"]
}

/// Sensitive paths under $HOME that we refuse to touch even though they
/// live in the home tree. Stored as suffix-relative-to-home strings.
fn sensitive_home_suffixes() -> [&'static str; 6] {
    [".ssh", ".aws", ".gnupg", ".config", "Library/Keychains", "Library/Cookies"]
}

/// Returns the path with `.` / `..` components resolved against `base`,
/// without requiring the path to exist (so it works for destinations).
/// We deliberately do NOT follow symlinks here — symlink resolution is
/// the caller's job via `std::fs::canonicalize` when the target exists.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in p.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Core path policy check. Used for both sources and destination *parents*.
pub fn check_path(raw: &str) -> Result<PathBuf, PathRejection> {
    if raw.is_empty() {
        return Err(PathRejection::Empty);
    }
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err(PathRejection::NotAbsolute);
    }
    let normalized = lexical_normalize(p);
    if normalized.as_os_str().is_empty() || normalized == Path::new("/") {
        return Err(PathRejection::Root);
    }

    let norm_str = normalized.to_string_lossy();

    // User-writable tmp areas are carve-outs from the system_prefixes
    // reject list. macOS hands out `/var/folders/...` as the app tempdir;
    // the system-dir check would otherwise eat it because `/var` is listed.
    let in_tmp = norm_str.starts_with("/tmp/") || norm_str == "/tmp"
        || norm_str.starts_with("/var/folders/");

    // System directories are off-limits — except the tmp carve-outs above.
    if !in_tmp {
        for sys in system_prefixes() {
            if norm_str == sys || norm_str.starts_with(&format!("{}/", sys)) {
                return Err(PathRejection::SystemDir);
            }
        }
    }

    // Our own DB lives under ~/.fast-finder; we never expose it to FS ops.
    let app_dir = app_data_dir();
    if normalized.starts_with(&app_dir) {
        return Err(PathRejection::AppDataDir);
    }

    let home = home_dir();
    let in_home = normalized.starts_with(&home);
    if !in_home && !in_tmp {
        return Err(PathRejection::OutsideHome);
    }

    if in_home {
        // Sensitive subtrees.
        for suffix in sensitive_home_suffixes() {
            let full = home.join(suffix);
            if normalized.starts_with(&full) {
                return Err(PathRejection::Sensitive);
            }
        }

        // Top-level $HOME dotfiles (e.g. ~/.zshrc). Only the immediate child:
        // ~/.config/x is handled by sensitive_home_suffixes; ~/.zshrc isn't.
        if let Ok(rel) = normalized.strip_prefix(&home) {
            let mut comps = rel.components();
            if let (Some(first), None) = (comps.next(), comps.next()) {
                if let Component::Normal(name) = first {
                    if name.to_string_lossy().starts_with('.') {
                        return Err(PathRejection::HomeRootDotfile);
                    }
                }
            }
        }
    }

    Ok(normalized)
}

/// Source paths must point at things that exist; destinations can be new.
pub fn check_sources(paths: &[String]) -> Result<Vec<PathBuf>, (usize, PathRejection)> {
    if paths.len() > MAX_BULK_PATHS {
        return Err((paths.len(), PathRejection::TooManyPaths));
    }
    let mut out = Vec::with_capacity(paths.len());
    for (i, p) in paths.iter().enumerate() {
        match check_path(p) {
            Ok(np) => out.push(np),
            Err(e) => return Err((i, e)),
        }
    }
    Ok(out)
}

/// Validate a destination directory or file path.
/// We check the directory the path lives in (creating subdirs is fine,
/// but the *root* of the destination must be a sane place).
pub fn check_destination(raw: &str) -> Result<PathBuf, PathRejection> {
    check_path(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_home<F: FnOnce()>(home: &str, f: F) {
        let prev = env::var("HOME").ok();
        env::set_var("HOME", home);
        f();
        match prev {
            Some(v) => env::set_var("HOME", v),
            None => env::remove_var("HOME"),
        }
    }

    // These tests mutate the process-global HOME env var. Serialize so
    // parallel runs don't see each other's HOME. Poison-tolerant: a
    // panicking test must not cascade.
    static SAFETY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        SAFETY_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn rejects_empty_and_relative() {
        let _g = lock();
        assert_eq!(check_path(""), Err(PathRejection::Empty));
        assert_eq!(check_path("relative/path"), Err(PathRejection::NotAbsolute));
    }

    #[test]
    fn rejects_root() {
        let _g = lock();
        assert_eq!(check_path("/"), Err(PathRejection::Root));
        assert_eq!(check_path("/.."), Err(PathRejection::Root));
    }

    #[test]
    fn rejects_system_dirs() {
        let _g = lock();
        for p in [
            "/System/Library/CoreServices",
            "/usr/local/bin/foo",
            "/etc/hosts",
            "/Library/Application Support/Foo",
            "/private/var/db/foo",
        ] {
            assert!(matches!(check_path(p), Err(PathRejection::SystemDir)), "should reject: {}", p);
        }
    }

    #[test]
    fn rejects_outside_home_for_user_paths() {
        let _g = lock();
        with_home("/Users/ropo", || {
            assert!(matches!(check_path("/Volumes/External/file.txt"), Err(PathRejection::OutsideHome)));
            assert!(matches!(check_path("/Users/someone-else/Documents/file.txt"), Err(PathRejection::OutsideHome)));
        });
    }

    #[test]
    fn allows_tmp_and_var_folders() {
        let _g = lock();
        with_home("/Users/ropo", || {
            assert!(check_path("/tmp/foo").is_ok());
            assert!(check_path("/var/folders/xx/yy/T/foo").is_ok());
        });
    }

    #[test]
    fn rejects_sensitive_home_subtrees() {
        let _g = lock();
        with_home("/Users/ropo", || {
            for p in [
                "/Users/ropo/.ssh/id_rsa",
                "/Users/ropo/.aws/credentials",
                "/Users/ropo/Library/Keychains/login.keychain-db",
                "/Users/ropo/.config/foo/bar",
            ] {
                assert!(matches!(check_path(p), Err(PathRejection::Sensitive)), "should reject: {}", p);
            }
        });
    }

    #[test]
    fn rejects_home_root_dotfiles() {
        let _g = lock();
        with_home("/Users/ropo", || {
            for p in ["/Users/ropo/.zshrc", "/Users/ropo/.profile", "/Users/ropo/.gitconfig"] {
                assert!(matches!(check_path(p), Err(PathRejection::HomeRootDotfile)), "should reject: {}", p);
            }
            // But a deeper dotfile inside Documents is fine.
            assert!(check_path("/Users/ropo/Documents/.hidden/notes.md").is_ok());
        });
    }

    #[test]
    fn rejects_app_data_dir() {
        let _g = lock();
        with_home("/Users/ropo", || {
            assert!(matches!(check_path("/Users/ropo/.fast-finder/index.sqlite3"), Err(PathRejection::AppDataDir)));
        });
    }

    #[test]
    fn allows_normal_user_paths() {
        let _g = lock();
        with_home("/Users/ropo", || {
            assert!(check_path("/Users/ropo/Documents/report.pdf").is_ok());
            assert!(check_path("/Users/ropo/Downloads/big folder/file.zip").is_ok());
            assert!(check_path("/Users/ropo/Desktop/screenshot.png").is_ok());
        });
    }

    #[test]
    fn dotdot_traversal_cannot_escape_check() {
        let _g = lock();
        with_home("/Users/ropo", || {
            // /Users/ropo/Documents/../../etc/passwd → /etc/passwd → SystemDir
            let res = check_path("/Users/ropo/Documents/../../../etc/passwd");
            assert!(matches!(res, Err(PathRejection::SystemDir)));
            // /Users/ropo/Documents/../.ssh/id_rsa → ~/.ssh → Sensitive
            let res = check_path("/Users/ropo/Documents/../.ssh/id_rsa");
            assert!(matches!(res, Err(PathRejection::Sensitive)));
        });
    }

    #[test]
    fn bulk_cap_enforced() {
        let _g = lock();
        with_home("/Users/ropo", || {
            let many: Vec<String> = (0..MAX_BULK_PATHS + 1)
                .map(|i| format!("/Users/ropo/Documents/f{}.txt", i))
                .collect();
            assert!(matches!(check_sources(&many), Err((_, PathRejection::TooManyPaths))));
        });
    }

    #[test]
    fn check_sources_reports_offending_index() {
        let _g = lock();
        with_home("/Users/ropo", || {
            let paths = vec![
                "/Users/ropo/Documents/ok.txt".to_string(),
                "/etc/passwd".to_string(),
                "/Users/ropo/Documents/also_ok.txt".to_string(),
            ];
            let err = check_sources(&paths).unwrap_err();
            assert_eq!(err.0, 1);
            assert!(matches!(err.1, PathRejection::SystemDir));
        });
    }
}
