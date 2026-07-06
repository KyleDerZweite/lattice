use crate::{
    default_ignore_overrides, is_permission_denied_ignore_error, vault_relative_path, Workspace,
};
use anyhow::{bail, Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use lattice_core::VaultPath;
use regex::{Captures, Regex, RegexBuilder};
use std::collections::BTreeSet;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

const DEFAULT_MAX_RESULTS: usize = 10_000;
const MAX_SEARCH_FILE_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceSearchOptions {
    pub query: SearchQuery,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub max_results: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatch {
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub path: VaultPath,
    pub line_number: usize,
    pub column: usize,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_text: String,
    pub preview_match_start: usize,
    pub preview_match_end: usize,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceSearchResult {
    pub matches: Vec<SearchMatch>,
    pub files_scanned: usize,
    pub files_with_matches: usize,
    pub truncated: bool,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceReplaceReport {
    pub files_changed: usize,
    pub replacements: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
    pub elapsed_ms: u128,
}

#[derive(Clone)]
struct CompiledSearch {
    regex: Regex,
    regex_replacement: bool,
}

impl CompiledSearch {
    fn new(query: &SearchQuery) -> Result<Self> {
        if query.text.is_empty() {
            bail!("search query cannot be empty");
        }

        let source = if query.use_regex {
            query.text.clone()
        } else {
            regex::escape(&query.text)
        };
        let source = if query.whole_word {
            format!(r"\b(?:{source})\b")
        } else {
            source
        };
        let regex = RegexBuilder::new(&source)
            .case_insensitive(!query.case_sensitive)
            .multi_line(true)
            // Let ^/$ match at CRLF line endings too, not only bare \n.
            .crlf(true)
            .build()
            .with_context(|| "invalid regular expression")?;
        Ok(Self {
            regex,
            regex_replacement: query.use_regex,
        })
    }

    fn find(&self, text: &str, limit: usize) -> Vec<TextMatch> {
        self.regex
            .find_iter(text)
            .take(limit)
            .map(|found| TextMatch {
                byte_start: found.start(),
                byte_end: found.end(),
            })
            .collect()
    }

    fn replace_all(&self, text: &str, replacement: &str) -> (String, usize) {
        let count = self.regex.find_iter(text).count();
        if count == 0 {
            return (text.to_owned(), 0);
        }
        let replaced = if self.regex_replacement {
            self.regex.replace_all(text, replacement).into_owned()
        } else {
            self.regex
                .replace_all(text, |_: &Captures<'_>| replacement)
                .into_owned()
        };
        (replaced, count)
    }

    fn replacement_for_match(
        &self,
        text: &str,
        matched: &TextMatch,
        replacement: &str,
    ) -> Option<String> {
        let found = self.regex.find_at(text, matched.byte_start)?;
        if found.start() != matched.byte_start || found.end() != matched.byte_end {
            return None;
        }
        if !self.regex_replacement {
            return Some(replacement.to_owned());
        }
        let captures = self.regex.captures_at(text, matched.byte_start)?;
        let whole = captures.get(0)?;
        if whole.start() != matched.byte_start || whole.end() != matched.byte_end {
            return None;
        }
        let mut expanded = String::new();
        captures.expand(replacement, &mut expanded);
        Some(expanded)
    }
}

pub fn find_text_matches(text: &str, query: &SearchQuery, limit: usize) -> Result<Vec<TextMatch>> {
    Ok(CompiledSearch::new(query)?.find(text, limit))
}

pub fn replace_all_text(
    text: &str,
    query: &SearchQuery,
    replacement: &str,
) -> Result<(String, usize)> {
    Ok(CompiledSearch::new(query)?.replace_all(text, replacement))
}

pub fn replace_text_match(
    text: &str,
    query: &SearchQuery,
    matched: &TextMatch,
    replacement: &str,
) -> Result<Option<String>> {
    let compiled = CompiledSearch::new(query)?;
    let Some(replacement) = compiled.replacement_for_match(text, matched, replacement) else {
        return Ok(None);
    };
    let mut updated = String::with_capacity(
        text.len() - matched.byte_end.saturating_sub(matched.byte_start) + replacement.len(),
    );
    updated.push_str(&text[..matched.byte_start]);
    updated.push_str(&replacement);
    updated.push_str(&text[matched.byte_end..]);
    Ok(Some(updated))
}

pub fn search_path_matches(path: &VaultPath, options: &WorkspaceSearchOptions) -> Result<bool> {
    let overrides = search_overrides(std::path::Path::new("."), options)?;
    Ok(!overrides
        .matched(path.as_path().as_std_path(), false)
        .is_ignore())
}

pub(crate) fn search_workspace<F>(
    workspace: &Workspace,
    options: &WorkspaceSearchOptions,
    is_cancelled: F,
) -> Result<WorkspaceSearchResult>
where
    F: Fn() -> bool + Sync,
{
    search_workspace_with_open_files(workspace, options, &[], is_cancelled)
}

pub(crate) fn search_workspace_with_open_files<F>(
    workspace: &Workspace,
    options: &WorkspaceSearchOptions,
    open_files: &[(VaultPath, String)],
    is_cancelled: F,
) -> Result<WorkspaceSearchResult>
where
    F: Fn() -> bool + Sync,
{
    let started = Instant::now();
    let compiled = CompiledSearch::new(&options.query)?;
    let max_results = effective_limit(options.max_results);
    let root = workspace.vault.root.as_path();
    let builder = search_walk_builder(root, options)?;
    let excluded_paths: BTreeSet<_> = open_files.iter().map(|(path, _)| path.clone()).collect();
    let mut matches = Vec::new();
    let mut initial_truncated = false;
    let mut open_files_scanned = 0;
    for (path, text) in open_files {
        if is_cancelled() || !search_path_matches(path, options)? {
            continue;
        }
        open_files_scanned += 1;
        let remaining = max_results.saturating_sub(matches.len());
        if remaining == 0 {
            initial_truncated = true;
            break;
        }
        let found = compiled.find(text, remaining.saturating_add(1));
        let take = found.len().min(remaining);
        matches.extend(enrich_matches(path.clone(), text, &found[..take]));
        if found.len() > take {
            initial_truncated = true;
            break;
        }
    }
    let result_count = AtomicUsize::new(matches.len());
    let files_scanned = AtomicUsize::new(open_files_scanned);
    let truncated = AtomicBool::new(false);
    let (sender, receiver) = mpsc::channel();

    builder.build_parallel().run(|| {
        let sender = sender.clone();
        let compiled = compiled.clone();
        let result_count = &result_count;
        let files_scanned = &files_scanned;
        let truncated = &truncated;
        let is_cancelled = &is_cancelled;
        let excluded_paths = &excluded_paths;
        Box::new(move |entry| {
            if is_cancelled() || result_count.load(Ordering::Relaxed) >= max_results {
                truncated.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if is_permission_denied_ignore_error(&error) => {
                    return ignore::WalkState::Continue;
                }
                Err(error) => {
                    log::warn!("workspace search skipped entry: {error}");
                    return ignore::WalkState::Continue;
                }
            };
            let Some(file_type) = entry.file_type() else {
                return ignore::WalkState::Continue;
            };
            if !file_type.is_file() {
                return ignore::WalkState::Continue;
            }
            let relative = match vault_relative_path(root, entry.path()) {
                Ok(path) => path,
                Err(error) => {
                    log::warn!("workspace search skipped path: {error}");
                    return ignore::WalkState::Continue;
                }
            };
            let path = match VaultPath::new(&relative) {
                Ok(path) => path,
                Err(error) => {
                    log::warn!("workspace search skipped path: {error}");
                    return ignore::WalkState::Continue;
                }
            };
            if excluded_paths.contains(&path) {
                return ignore::WalkState::Continue;
            }
            files_scanned.fetch_add(1, Ordering::Relaxed);
            let bytes = match fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(error) => {
                    log::warn!(
                        "workspace search could not read {}: {error}",
                        entry.path().display()
                    );
                    return ignore::WalkState::Continue;
                }
            };
            if bytes.contains(&0) {
                return ignore::WalkState::Continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return ignore::WalkState::Continue;
            };
            let remaining = max_results.saturating_sub(result_count.load(Ordering::Relaxed));
            if remaining == 0 {
                truncated.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            let matches = compiled.find(text, remaining.saturating_add(1));
            if matches.is_empty() {
                return ignore::WalkState::Continue;
            }
            let claimed = claim_result_slots(result_count, matches.len(), max_results);
            if claimed < matches.len() {
                truncated.store(true, Ordering::Relaxed);
            }
            let matches = &matches[..claimed];
            let batch = enrich_matches(path, text, matches);
            let _ = sender.send(batch);
            ignore::WalkState::Continue
        })
    });
    drop(sender);

    for mut batch in receiver {
        matches.append(&mut batch);
    }
    matches.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.byte_start.cmp(&b.byte_start))
    });
    let files_with_matches = matches
        .iter()
        .map(|matched| &matched.path)
        .collect::<BTreeSet<_>>()
        .len();
    Ok(WorkspaceSearchResult {
        matches,
        files_scanned: files_scanned.load(Ordering::Relaxed),
        files_with_matches,
        truncated: (initial_truncated || truncated.load(Ordering::Relaxed)) && !is_cancelled(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub(crate) fn replace_workspace<F>(
    workspace: &Workspace,
    options: &WorkspaceSearchOptions,
    replacement: &str,
    include_paths: Option<&BTreeSet<VaultPath>>,
    excluded_paths: &BTreeSet<VaultPath>,
    is_cancelled: F,
) -> Result<WorkspaceReplaceReport>
where
    F: Fn() -> bool + Sync,
{
    let started = Instant::now();
    let compiled = CompiledSearch::new(&options.query)?;
    let root = workspace.vault.root.as_path();
    let builder = search_walk_builder(root, options)?;
    let files_changed = AtomicUsize::new(0);
    let replacement_count = AtomicUsize::new(0);
    let (error_sender, error_receiver) = mpsc::channel();

    builder.build_parallel().run(|| {
        let error_sender = error_sender.clone();
        let compiled = compiled.clone();
        let is_cancelled = &is_cancelled;
        let files_changed = &files_changed;
        let replacement_count = &replacement_count;
        Box::new(move |entry| {
            if is_cancelled() {
                return ignore::WalkState::Quit;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return ignore::WalkState::Continue,
            };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                return ignore::WalkState::Continue;
            }
            let relative = match vault_relative_path(root, entry.path()) {
                Ok(path) => path,
                Err(_) => return ignore::WalkState::Continue,
            };
            let path = match VaultPath::new(&relative) {
                Ok(path) => path,
                Err(_) => return ignore::WalkState::Continue,
            };
            if excluded_paths.contains(&path)
                || include_paths.is_some_and(|paths| !paths.contains(&path))
            {
                return ignore::WalkState::Continue;
            }
            let bytes = match fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(error) => {
                    let _ = error_sender.send(format!("{}: {error}", path.as_str()));
                    return ignore::WalkState::Continue;
                }
            };
            if bytes.contains(&0) {
                return ignore::WalkState::Continue;
            }
            let text = match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => return ignore::WalkState::Continue,
            };
            let (updated, replacements) = compiled.replace_all(&text, replacement);
            if replacements > 0 {
                match workspace.save_file(&path, &updated) {
                    Ok(()) => {
                        files_changed.fetch_add(1, Ordering::Relaxed);
                        replacement_count.fetch_add(replacements, Ordering::Relaxed);
                    }
                    Err(error) => {
                        let _ = error_sender.send(format!("{}: {error}", path.as_str()));
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });
    drop(error_sender);

    Ok(WorkspaceReplaceReport {
        files_changed: files_changed.load(Ordering::Relaxed),
        replacements: replacement_count.load(Ordering::Relaxed),
        errors: error_receiver.into_iter().collect(),
        cancelled: is_cancelled(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn search_walk_builder(
    root: &std::path::Path,
    options: &WorkspaceSearchOptions,
) -> Result<WalkBuilder> {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .max_filesize(Some(MAX_SEARCH_FILE_BYTES))
        .overrides(search_overrides(root, options)?);
    Ok(builder)
}

fn search_overrides(
    root: &std::path::Path,
    options: &WorkspaceSearchOptions,
) -> Result<ignore::overrides::Override> {
    if options.include_globs.is_empty() && options.exclude_globs.is_empty() {
        return default_ignore_overrides(root);
    }
    let mut overrides = OverrideBuilder::new(root);
    for ignored in crate::DEFAULT_IGNORES {
        overrides.add(&format!("!{ignored}"))?;
        overrides.add(&format!("!{ignored}/**"))?;
    }
    for glob in &options.include_globs {
        overrides.add(glob)?;
    }
    for glob in &options.exclude_globs {
        overrides.add(&format!("!{glob}"))?;
    }
    Ok(overrides.build()?)
}

fn effective_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_MAX_RESULTS
    } else {
        limit
    }
}

fn claim_result_slots(counter: &AtomicUsize, wanted: usize, limit: usize) -> usize {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current >= limit {
            return 0;
        }
        let claimed = wanted.min(limit - current);
        match counter.compare_exchange_weak(
            current,
            current + claimed,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return claimed,
            Err(actual) => current = actual,
        }
    }
}

fn enrich_matches(path: VaultPath, text: &str, matches: &[TextMatch]) -> Vec<SearchMatch> {
    let line_starts = line_starts(text);
    matches
        .iter()
        .map(|matched| {
            let line_index = line_starts.partition_point(|start| *start <= matched.byte_start) - 1;
            let line_start = line_starts[line_index];
            let line_end = text[line_start..]
                .find('\n')
                .map(|offset| line_start + offset)
                .unwrap_or(text.len());
            let raw_line = text[line_start..line_end].trim_end_matches('\r');
            let (line_text, preview_match_start, preview_match_end) = compact_preview(
                raw_line,
                matched.byte_start - line_start,
                matched.byte_end - line_start,
            );
            SearchMatch {
                path: path.clone(),
                line_number: line_index + 1,
                column: text[line_start..matched.byte_start].chars().count() + 1,
                byte_start: matched.byte_start,
                byte_end: matched.byte_end,
                line_text,
                preview_match_start,
                preview_match_end,
            }
        })
        .collect()
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(text.len() / 40 + 1);
    starts.push(0);
    starts.extend(
        text.bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    starts
}

fn compact_preview(line: &str, match_start: usize, match_end: usize) -> (String, usize, usize) {
    const LIMIT: usize = 500;
    let line_chars = line.chars().count();
    if line_chars <= LIMIT {
        return (
            line.to_owned(),
            match_start.min(line.len()),
            match_end.min(line.len()),
        );
    }
    let match_start = byte_to_char_offset(line, match_start);
    let match_end = byte_to_char_offset(line, match_end);
    let match_len = match_end.saturating_sub(match_start);
    let context_before = (LIMIT.saturating_sub(match_len)) / 2;
    let window_start = match_start.saturating_sub(context_before);
    let window_start = window_start.min(line_chars.saturating_sub(LIMIT));
    let mut preview = String::new();
    if window_start > 0 {
        preview.push_str("...");
    }
    let visible: String = line.chars().skip(window_start).take(LIMIT).collect();
    let match_start_in_visible: String = line
        .chars()
        .skip(window_start)
        .take(match_start.saturating_sub(window_start))
        .collect();
    let match_end_in_visible: String = line
        .chars()
        .skip(window_start)
        .take(match_end.saturating_sub(window_start).min(LIMIT))
        .collect();
    let prefix_len = preview.len();
    let preview_match_start = prefix_len + match_start_in_visible.len();
    let preview_match_end = prefix_len + match_end_in_visible.len();
    preview.push_str(&visible);
    if window_start + LIMIT < line_chars {
        preview.push_str("...");
    }
    (preview, preview_match_start, preview_match_end)
}

fn byte_to_char_offset(text: &str, byte_offset: usize) -> usize {
    let mut byte_offset = byte_offset.min(text.len());
    while !text.is_char_boundary(byte_offset) {
        byte_offset -= 1;
    }
    text[..byte_offset].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> (std::path::PathBuf, Workspace) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "lattice-search-test-{}-{nonce}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let workspace = Workspace::open_vault(root.clone()).unwrap();
        (root, workspace)
    }

    fn options(text: &str) -> WorkspaceSearchOptions {
        WorkspaceSearchOptions {
            query: SearchQuery {
                text: text.to_owned(),
                ..Default::default()
            },
            max_results: 100,
            ..Default::default()
        }
    }

    #[test]
    fn text_search_supports_case_whole_word_and_regex() {
        let text = "alpha alphabet ALPHA\nbeta-42";
        let mut query = SearchQuery {
            text: "alpha".to_owned(),
            ..Default::default()
        };
        assert_eq!(find_text_matches(text, &query, 10).unwrap().len(), 3);
        query.whole_word = true;
        assert_eq!(find_text_matches(text, &query, 10).unwrap().len(), 2);
        query.case_sensitive = true;
        assert_eq!(find_text_matches(text, &query, 10).unwrap().len(), 1);
        query.text = r"beta-\d+".to_owned();
        query.use_regex = true;
        query.whole_word = false;
        assert_eq!(find_text_matches(text, &query, 10).unwrap().len(), 1);
    }

    #[test]
    fn line_anchors_match_crlf_line_endings() {
        let query = SearchQuery {
            text: r"foo$".to_owned(),
            use_regex: true,
            case_sensitive: true,
            ..Default::default()
        };
        assert_eq!(
            find_text_matches("foo\r\nbar foo\r\n", &query, 10)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn literal_and_regex_replacements_have_expected_expansion_rules() {
        let literal = SearchQuery {
            text: "price".to_owned(),
            ..Default::default()
        };
        let (text, count) = replace_all_text("price price", &literal, "$1").unwrap();
        assert_eq!(text, "$1 $1");
        assert_eq!(count, 2);

        let regex = SearchQuery {
            text: r"(\w+), (\w+)".to_owned(),
            use_regex: true,
            ..Default::default()
        };
        let (text, count) = replace_all_text("Doe, Jane", &regex, "$2 $1").unwrap();
        assert_eq!(text, "Jane Doe");
        assert_eq!(count, 1);
    }

    #[test]
    fn workspace_search_honors_ignores_globs_and_limits() {
        let (root, workspace) = temp_workspace();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "needle\nneedle").unwrap();
        fs::write(root.join("src/main.txt"), "needle").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/out.rs"), "needle").unwrap();
        let mut options = options("needle");
        options.include_globs = vec!["*.rs".to_owned()];
        options.max_results = 1;

        let result = search_workspace(&workspace, &options, || false).unwrap();

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path.as_str(), "src/main.rs");
        assert!(result.truncated);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_replace_is_atomic_per_file_and_can_filter_paths() {
        let (root, workspace) = temp_workspace();
        fs::write(root.join("one.rs"), "old old").unwrap();
        fs::write(root.join("two.rs"), "old").unwrap();
        let options = options("old");
        let included = BTreeSet::from([VaultPath::try_from("one.rs").unwrap()]);

        let report = replace_workspace(
            &workspace,
            &options,
            "new",
            Some(&included),
            &BTreeSet::new(),
            || false,
        )
        .unwrap();

        assert_eq!(report.files_changed, 1);
        assert_eq!(report.replacements, 2);
        assert_eq!(fs::read_to_string(root.join("one.rs")).unwrap(), "new new");
        assert_eq!(fs::read_to_string(root.join("two.rs")).unwrap(), "old");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_search_reports_unicode_columns_and_skips_binary_files() {
        let (root, workspace) = temp_workspace();
        fs::write(root.join("text.rs"), "åß needle\n").unwrap();
        fs::write(root.join("binary.dat"), b"needle\0needle").unwrap();

        let result = search_workspace(&workspace, &options("needle"), || false).unwrap();

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path.as_str(), "text.rs");
        assert_eq!(result.matches[0].line_number, 1);
        assert_eq!(result.matches[0].column, 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_search_honors_excludes_and_cancellation() {
        let (root, workspace) = temp_workspace();
        fs::create_dir_all(root.join("generated")).unwrap();
        fs::write(root.join("main.rs"), "needle").unwrap();
        fs::write(root.join(".env"), "needle").unwrap();
        fs::write(root.join("generated/code.rs"), "needle").unwrap();
        let mut filtered = options("needle");
        filtered.exclude_globs = vec!["generated/**".to_owned()];

        let result = search_workspace(&workspace, &filtered, || false).unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].path.as_str(), ".env");
        assert_eq!(result.matches[1].path.as_str(), "main.rs");

        let cancelled = search_workspace(&workspace, &options("needle"), || true).unwrap();
        assert!(cancelled.matches.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_replace_handles_many_files_without_buffering_contents() {
        let (root, workspace) = temp_workspace();
        for index in 0..96 {
            fs::write(root.join(format!("file-{index}.rs")), "old").unwrap();
        }

        let report = replace_workspace(
            &workspace,
            &options("old"),
            "new",
            None,
            &BTreeSet::new(),
            || false,
        )
        .unwrap();

        assert_eq!(report.files_changed, 96);
        assert_eq!(report.replacements, 96);
        assert!(report.errors.is_empty());
        assert_eq!(fs::read_to_string(root.join("file-95.rs")).unwrap(), "new");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_path_filter_matches_workspace_globs() {
        let mut options = options("needle");
        options.include_globs = vec!["*.rs".to_owned()];
        options.exclude_globs = vec!["generated/**".to_owned()];

        assert!(
            search_path_matches(&VaultPath::try_from("src/main.rs").unwrap(), &options).unwrap()
        );
        assert!(
            !search_path_matches(&VaultPath::try_from("src/main.txt").unwrap(), &options).unwrap()
        );
        assert!(
            !search_path_matches(&VaultPath::try_from("generated/main.rs").unwrap(), &options)
                .unwrap()
        );
    }

    #[test]
    fn workspace_search_uses_open_buffer_instead_of_stale_disk_content() {
        let (root, workspace) = temp_workspace();
        fs::write(root.join("main.rs"), "stale disk needle").unwrap();
        let open_files = vec![(
            VaultPath::try_from("main.rs").unwrap(),
            "unsaved buffer value".to_owned(),
        )];

        let stale = search_workspace_with_open_files(
            &workspace,
            &options("needle"),
            &open_files,
            || false,
        )
        .unwrap();
        assert!(stale.matches.is_empty());

        let current = search_workspace_with_open_files(
            &workspace,
            &options("buffer"),
            &open_files,
            || false,
        )
        .unwrap();
        assert_eq!(current.matches.len(), 1);
        assert_eq!(current.matches[0].path.as_str(), "main.rs");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn long_line_preview_keeps_the_match_visible() {
        let line = format!("{}needle{}", "a".repeat(700), "b".repeat(700));
        let start = 700;
        let end = start + "needle".len();

        let (preview, preview_start, preview_end) = compact_preview(&line, start, end);

        assert!(preview.len() < line.len());
        assert_eq!(&preview[preview_start..preview_end], "needle");
    }
}
