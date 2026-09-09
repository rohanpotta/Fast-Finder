//! Filter tokens for the search bar: `kind:pdf added:<7d size:>10mb`.
//!
//! The point of this module is that filtering never leaves the machine. Before
//! it, the only way to express "PDFs I added last week" was to type it as prose
//! and let the AI bar round-trip to an API — network latency and a per-call
//! cost for something SQLite answers off an index in microseconds. The AI bar
//! is for genuinely ambiguous bulk work, not for `kind:pdf`.
//!
//! Anything that isn't a recognised `key:value` token stays as free text and
//! goes to FTS, so a filename containing a colon still searches normally.

use rusqlite::types::Value;

/// Comparison direction for a bounded filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Less,
    Greater,
}

/// One parsed filter, kept alongside the token that produced it so the UI can
/// render a removable chip without re-deriving the text.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub token: String,
    pub label: String,
    pub clause: FilterClause,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterClause {
    /// Any of these extensions (lowercase, no dot).
    Extensions(Vec<String>),
    IsDir(bool),
    /// Age in seconds relative to now: Less = newer than, Greater = older than.
    Age { column: DateColumn, cmp: Cmp, secs: i64 },
    /// Absolute unix timestamp: Less = before, Greater = after.
    Instant { column: DateColumn, cmp: Cmp, ts: i64 },
    Size { cmp: Cmp, bytes: i64 },
    PathContains(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateColumn {
    Added,
    Modified,
}

impl DateColumn {
    fn sql(&self) -> &'static str {
        match self {
            // Mirrors DateField::Added's fallback so a filter and the sort
            // column can't disagree about the same file.
            DateColumn::Added => "COALESCE(NULLIF({p}birthtime, 0), {p}mtime)",
            DateColumn::Modified => "{p}mtime",
        }
    }
}

/// Result of parsing raw search-bar input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Free text, for FTS.
    pub text: String,
    pub filters: Vec<Filter>,
    /// Tokens shaped like filters whose key we know but value we couldn't read.
    /// Surfaced rather than silently ignored — a typo'd filter that quietly
    /// matches everything is worse than one that says it's wrong.
    pub invalid: Vec<String>,
}

/// Extension groups behind `kind:`. Deliberately small: the useful groups, not
/// an exhaustive MIME table.
fn kind_group(name: &str) -> Option<Vec<String>> {
    let exts: &[&str] = match name {
        "image" | "images" | "img" => {
            &["jpg", "jpeg", "png", "gif", "heic", "webp", "svg", "tiff", "bmp", "psd", "ai"]
        }
        "video" | "videos" | "movie" => &["mp4", "mov", "avi", "mkv", "webm", "m4v", "wmv"],
        "audio" | "music" => &["mp3", "wav", "aac", "flac", "m4a", "aiff", "ogg"],
        "doc" | "docs" | "document" | "documents" => {
            &["pdf", "doc", "docx", "txt", "rtf", "md", "pages", "odt", "epub"]
        }
        "sheet" | "sheets" | "spreadsheet" => &["xls", "xlsx", "csv", "numbers"],
        "slides" | "presentation" => &["ppt", "pptx", "key"],
        "code" => &[
            "rs", "swift", "py", "js", "ts", "tsx", "jsx", "java", "go", "c", "h", "cpp", "hpp",
            "rb", "php", "sh", "html", "css", "json", "yaml", "yml", "toml", "sql",
        ],
        "archive" | "archives" | "zip" => &["zip", "tar", "gz", "bz2", "rar", "7z", "dmg"],
        _ => return None,
    };
    Some(exts.iter().map(|s| s.to_string()).collect())
}

/// `2w` / `30d` / `12h` → seconds. Units are deliberately unambiguous: `m` is
/// never accepted, because it reads as both minutes and months.
fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| c.is_alphabetic())?);
    let n: i64 = num.parse().ok()?;
    if n < 0 {
        return None;
    }
    let secs = match unit.to_ascii_lowercase().as_str() {
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        "w" | "wk" | "week" | "weeks" => 604_800,
        "mo" | "month" | "months" => 2_592_000,
        "y" | "yr" | "year" | "years" => 31_536_000,
        _ => return None,
    };
    Some(n * secs)
}

/// `10mb` / `512kb` / `2gb` / `900` (bare = bytes) → bytes.
fn parse_size(s: &str) -> Option<i64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    if n < 0.0 {
        return None;
    }
    let mult: f64 = match unit {
        "" | "b" => 1.0,
        "k" | "kb" => 1_024.0,
        "m" | "mb" => 1_048_576.0,
        "g" | "gb" => 1_073_741_824.0,
        _ => return None,
    };
    Some((n * mult) as i64)
}

/// `YYYY-MM-DD` → unix timestamp at UTC midnight. Hand-rolled because pulling
/// in a date crate for one format isn't worth the dependency.
fn parse_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i64 = parts[0].parse().ok()?;
    let m: i64 = parts[1].parse().ok()?;
    let d: i64 = parts[2].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1970 {
        return None;
    }
    // Days since epoch via the standard civil-from-days algorithm.
    let (y_adj, m_adj) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = y_adj / 400;
    let yoe = y_adj - era * 400;
    let doy = (153 * (m_adj - 3) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400)
}

/// Strip a leading `>` or `<`. Bare values mean "within the last N", the way
/// people actually say it ("added 2w" = added in the past two weeks).
fn split_cmp(value: &str) -> (Cmp, &str) {
    if let Some(rest) = value.strip_prefix('>') {
        (Cmp::Greater, rest)
    } else if let Some(rest) = value.strip_prefix('<') {
        (Cmp::Less, rest)
    } else {
        (Cmp::Less, value)
    }
}

fn date_filter(token: &str, column: DateColumn, value: &str) -> Option<Filter> {
    let (cmp, rest) = split_cmp(value);
    let name = match column {
        DateColumn::Added => "added",
        DateColumn::Modified => "modified",
    };
    if let Some(ts) = parse_date(rest) {
        let word = if cmp == Cmp::Greater { "after" } else { "before" };
        return Some(Filter {
            token: token.to_string(),
            label: format!("{} {} {}", name, word, rest),
            clause: FilterClause::Instant { column, cmp, ts },
        });
    }
    let secs = parse_duration(rest)?;
    // Age semantics: `<7d` is "newer than 7 days old".
    let label = match cmp {
        Cmp::Less => format!("{} < {} ago", name, rest),
        Cmp::Greater => format!("{} > {} ago", name, rest),
    };
    Some(Filter {
        token: token.to_string(),
        label,
        clause: FilterClause::Age { column, cmp, secs },
    })
}

/// Split raw input into free text and filters.
pub fn parse(raw: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut words: Vec<&str> = Vec::new();

    for token in raw.split_whitespace() {
        let Some((key, value)) = token.split_once(':') else {
            words.push(token);
            continue;
        };
        let key_l = key.to_ascii_lowercase();
        let value_t = value.trim();

        // Not one of our keys — treat the whole token as text so a filename
        // like `notes:draft` still searches.
        let known = matches!(
            key_l.as_str(),
            "kind" | "type" | "ext" | "is" | "added" | "created" | "modified" | "size" | "in" | "path"
        );
        if !known {
            words.push(token);
            continue;
        }
        if value_t.is_empty() {
            out.invalid.push(token.to_string());
            continue;
        }

        let filter = match key_l.as_str() {
            "kind" | "type" => {
                let v = value_t.to_ascii_lowercase();
                if v == "folder" || v == "dir" || v == "directory" {
                    Some(Filter {
                        token: token.to_string(),
                        label: "folders".to_string(),
                        clause: FilterClause::IsDir(true),
                    })
                } else if v == "file" {
                    Some(Filter {
                        token: token.to_string(),
                        label: "files".to_string(),
                        clause: FilterClause::IsDir(false),
                    })
                } else if let Some(exts) = kind_group(&v) {
                    Some(Filter {
                        token: token.to_string(),
                        label: v.clone(),
                        clause: FilterClause::Extensions(exts),
                    })
                } else {
                    // `kind:pdf` — a bare extension is a perfectly good kind.
                    Some(Filter {
                        token: token.to_string(),
                        label: v.to_uppercase(),
                        clause: FilterClause::Extensions(vec![v]),
                    })
                }
            }
            "ext" => {
                let v = value_t.trim_start_matches('.').to_ascii_lowercase();
                Some(Filter {
                    token: token.to_string(),
                    label: format!(".{}", v),
                    clause: FilterClause::Extensions(vec![v]),
                })
            }
            "is" => match value_t.to_ascii_lowercase().as_str() {
                "folder" | "dir" | "directory" => Some(Filter {
                    token: token.to_string(),
                    label: "folders".to_string(),
                    clause: FilterClause::IsDir(true),
                }),
                "file" => Some(Filter {
                    token: token.to_string(),
                    label: "files".to_string(),
                    clause: FilterClause::IsDir(false),
                }),
                _ => None,
            },
            "added" | "created" => date_filter(token, DateColumn::Added, value_t),
            "modified" => date_filter(token, DateColumn::Modified, value_t),
            "size" => {
                let (cmp, rest) = split_cmp(value_t);
                parse_size(rest).map(|bytes| Filter {
                    token: token.to_string(),
                    label: format!("size {} {}", if cmp == Cmp::Greater { ">" } else { "<" }, rest),
                    clause: FilterClause::Size { cmp, bytes },
                })
            }
            "in" | "path" => Some(Filter {
                token: token.to_string(),
                label: format!("in {}", value_t),
                clause: FilterClause::PathContains(value_t.to_string()),
            }),
            _ => None,
        };

        match filter {
            Some(f) => out.filters.push(f),
            None => out.invalid.push(token.to_string()),
        }
    }

    out.text = words.join(" ");
    out
}

/// Compile filters into SQL fragments plus their bound parameters.
///
/// `prefix` is the table alias with trailing dot (`"f."`) or empty. Values are
/// always bound, never interpolated — a filter value is user input.
pub fn compile(filters: &[Filter], prefix: &str, now: i64) -> (Vec<String>, Vec<Value>) {
    let mut clauses = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    for f in filters {
        match &f.clause {
            FilterClause::Extensions(exts) => {
                let holes = vec!["?"; exts.len()].join(", ");
                clauses.push(format!("{p}ext IN ({holes})", p = prefix, holes = holes));
                params.extend(exts.iter().map(|e| Value::Text(e.clone())));
            }
            FilterClause::IsDir(is_dir) => {
                clauses.push(format!("{p}is_dir = ?", p = prefix));
                params.push(Value::Integer(i64::from(*is_dir)));
            }
            FilterClause::Age { column, cmp, secs } => {
                let col = column.sql().replace("{p}", prefix);
                // Newer than N seconds old == timestamp greater than now - N.
                let op = if *cmp == Cmp::Less { ">" } else { "<=" };
                clauses.push(format!("{col} {op} ?"));
                params.push(Value::Integer(now - secs));
            }
            FilterClause::Instant { column, cmp, ts } => {
                let col = column.sql().replace("{p}", prefix);
                let op = if *cmp == Cmp::Greater { ">=" } else { "<" };
                clauses.push(format!("{col} {op} ?"));
                params.push(Value::Integer(*ts));
            }
            FilterClause::Size { cmp, bytes } => {
                let op = if *cmp == Cmp::Greater { ">" } else { "<" };
                clauses.push(format!("{p}size {op} ?", p = prefix, op = op));
                params.push(Value::Integer(*bytes));
            }
            FilterClause::PathContains(needle) => {
                // LIKE with the wildcards escaped: a folder called `tax_2025`
                // must not also match `taxX2025`.
                let escaped = needle
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                clauses.push(format!("{p}path LIKE ? ESCAPE '\\'", p = prefix));
                params.push(Value::Text(format!("%{}%", escaped)));
            }
        }
    }

    (clauses, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_has_no_filters() {
        let p = parse("quarterly report");
        assert_eq!(p.text, "quarterly report");
        assert!(p.filters.is_empty());
        assert!(p.invalid.is_empty());
    }

    #[test]
    fn unknown_keys_stay_as_search_text() {
        // A filename with a colon must not be silently eaten.
        let p = parse("notes:draft");
        assert_eq!(p.text, "notes:draft");
        assert!(p.filters.is_empty());
    }

    #[test]
    fn filters_are_split_from_text() {
        let p = parse("report kind:pdf added:<7d");
        assert_eq!(p.text, "report");
        assert_eq!(p.filters.len(), 2);
        assert_eq!(p.filters[0].label, "PDF");
        assert_eq!(p.filters[1].label, "added < 7d ago");
    }

    #[test]
    fn kind_groups_expand_to_extensions() {
        let p = parse("kind:image");
        match &p.filters[0].clause {
            FilterClause::Extensions(exts) => {
                assert!(exts.contains(&"png".to_string()));
                assert!(exts.contains(&"heic".to_string()));
            }
            other => panic!("expected extensions, got {:?}", other),
        }
    }

    #[test]
    fn bare_extension_is_a_valid_kind() {
        let p = parse("kind:pdf");
        assert_eq!(
            p.filters[0].clause,
            FilterClause::Extensions(vec!["pdf".to_string()])
        );
        // ext: accepts a leading dot too.
        assert_eq!(
            parse("ext:.swift").filters[0].clause,
            FilterClause::Extensions(vec!["swift".to_string()])
        );
    }

    #[test]
    fn folders_and_files() {
        assert_eq!(parse("is:folder").filters[0].clause, FilterClause::IsDir(true));
        assert_eq!(parse("kind:folder").filters[0].clause, FilterClause::IsDir(true));
        assert_eq!(parse("is:file").filters[0].clause, FilterClause::IsDir(false));
    }

    #[test]
    fn bare_duration_means_within_the_last() {
        // "added 2w" reads as "in the past two weeks", not "older than".
        let p = parse("added:2w");
        assert_eq!(
            p.filters[0].clause,
            FilterClause::Age {
                column: DateColumn::Added,
                cmp: Cmp::Less,
                secs: 604_800 * 2
            }
        );
    }

    #[test]
    fn older_than_uses_greater_than() {
        let p = parse("modified:>1y");
        assert_eq!(
            p.filters[0].clause,
            FilterClause::Age {
                column: DateColumn::Modified,
                cmp: Cmp::Greater,
                secs: 31_536_000
            }
        );
        assert_eq!(p.filters[0].label, "modified > 1y ago");
    }

    #[test]
    fn ambiguous_minute_month_unit_is_rejected() {
        // `m` could be minutes or months; refuse rather than guess.
        assert_eq!(parse_duration("5m"), None);
        assert_eq!(parse_duration("5mo"), Some(2_592_000 * 5));
    }

    #[test]
    fn absolute_dates_use_before_after() {
        let p = parse("added:>2026-01-01");
        match p.filters[0].clause {
            FilterClause::Instant { cmp, ts, .. } => {
                assert_eq!(cmp, Cmp::Greater);
                assert_eq!(ts, 1_767_225_600, "2026-01-01T00:00:00Z");
            }
            ref other => panic!("expected instant, got {:?}", other),
        }
        assert_eq!(p.filters[0].label, "added after 2026-01-01");
    }

    #[test]
    fn date_epoch_conversion_is_correct() {
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("2000-03-01"), Some(951_868_800));
        assert_eq!(parse_date("2026-09-08"), Some(1_788_825_600));
        assert_eq!(parse_date("not-a-date"), None);
        assert_eq!(parse_date("2026-13-01"), None);
    }

    #[test]
    fn sizes_parse_with_units() {
        assert_eq!(parse_size("900"), Some(900));
        assert_eq!(parse_size("10kb"), Some(10_240));
        assert_eq!(parse_size("2mb"), Some(2_097_152));
        assert_eq!(parse_size("1gb"), Some(1_073_741_824));
        assert_eq!(parse_size("3tb"), None);
    }

    #[test]
    fn malformed_filter_values_are_reported_not_ignored() {
        let p = parse("kind: added:banana size:>3tb");
        assert!(p.filters.is_empty());
        assert_eq!(p.invalid.len(), 3);
        assert!(p.text.is_empty());
    }

    #[test]
    fn compile_binds_every_value() {
        let p = parse("kind:pdf size:>1mb in:Downloads is:file");
        let (clauses, params) = compile(&p.filters, "f.", 1_000_000);
        assert_eq!(clauses.len(), 4);
        // No literal user data spliced into SQL.
        for c in &clauses {
            assert!(!c.contains("pdf") && !c.contains("Downloads"));
        }
        assert_eq!(params.len(), 4);
    }

    #[test]
    fn compile_escapes_like_wildcards_in_paths() {
        let p = parse("in:tax_2025");
        let (clauses, params) = compile(&p.filters, "", 0);
        assert!(clauses[0].contains("ESCAPE"));
        match &params[0] {
            Value::Text(t) => assert_eq!(t, "%tax\\_2025%"),
            other => panic!("expected text, got {:?}", other),
        }
    }

    #[test]
    fn age_compiles_relative_to_now() {
        let now = 1_000_000i64;
        let p = parse("added:<1d");
        let (clauses, params) = compile(&p.filters, "", now);
        assert!(clauses[0].contains(">"), "newer-than is a timestamp lower bound");
        assert_eq!(params[0], Value::Integer(now - 86_400));
    }
}
