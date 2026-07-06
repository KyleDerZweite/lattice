use clap::Parser;
use eframe::egui;
use lattice_core::{AppSettings, OpenFileSnapshot, ThemeMode, VaultPath};
use lattice_editor::EditorBuffer;
use lattice_ui::Theme;
use lattice_workspace::{
    find_text_matches, replace_all_text, replace_text_match, search_path_matches, QuickOpenIndex,
    SearchMatch, SearchQuery, TextMatch, TreeNode, TreeNodeKind, Workspace, WorkspaceEventKind,
    WorkspaceReplaceReport, WorkspaceSearchOptions, WorkspaceSearchResult, WorkspaceWatcher,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const WATCHER_REFRESH_DEBOUNCE: Duration = Duration::from_millis(250);
const WORKSPACE_SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
const WORKSPACE_SEARCH_RESULT_LIMIT: usize = 10_000;
const LARGE_FILE_WARNING_BYTES: u64 = 10 * 1024 * 1024;
/// Above this size the syntect pass is skipped: highlighting re-tokenizes the whole
/// buffer whenever the memoized cache misses, which is too slow for huge files.
const HIGHLIGHT_MAX_BYTES: usize = 1024 * 1024;
const MAX_WORKER_RESPONSES_PER_FRAME: usize = 8;
const TREE_INDENT_WIDTH: f32 = 12.0;
const TREE_ICON_SIZE: f32 = 14.0;
const ROW_HEIGHT: f32 = 24.0;
// Default pane widths are encoded as shares inside `build_tile_tree` — egui_tiles
// owns the per-tile sizing now (relative, not pixel-anchored), so we no longer keep
// SIDEBAR_WIDTH / GRAPH_WIDTH constants here.
// Colors come from `lattice_ui::Theme` (crates/lattice-ui/src/theme.rs), held on
// `LatticeApp` and cloned into a local `theme` inside each drawing method.

#[derive(Debug, Parser)]
#[command(author, version, about = "Fast native code editor")]
struct Cli {
    #[arg(long, help = "Run a headless workspace performance benchmark")]
    bench: bool,
    #[arg(
        long = "bench-query",
        help = "Quick-open query to benchmark; repeatable"
    )]
    bench_queries: Vec<String>,
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
}

fn main() -> eframe::Result {
    env_logger::init();
    std::panic::set_hook(Box::new(|panic_info| {
        log::error!("Lattice panic: {panic_info}");
    }));

    let cli = Cli::parse();
    if cli.bench {
        if let Err(error) = run_benchmark(cli.path, cli.bench_queries) {
            eprintln!("benchmark failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        // Must match the .desktop file name so the taskbar associates the window
        // with the launcher entry (icon, grouping, pinning).
        .with_app_id("lattice")
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([840.0, 520.0]);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Lattice",
        options,
        Box::new(move |cc| {
            lattice_ui::fonts::install(&cc.egui_ctx);
            Ok(Box::new(LatticeApp::new(&cc.egui_ctx, cli.path.clone())))
        }),
    )
}

fn load_window_icon() -> Option<std::sync::Arc<egui::IconData>> {
    // Embed at compile time so the binary is portable and doesn't depend on cwd.
    let bytes = include_bytes!("../../../public/lattice_icon.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes.as_slice()));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    // Normalize whatever the PNG actually is into RGBA8 — egui::IconData wants
    // unmultiplied 8-bit RGBA.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(buf.len() / 3 * 4);
            for chunk in buf.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(0xff);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(buf.len() * 2);
            for chunk in buf.chunks_exact(2) {
                let v = chunk[0];
                out.extend_from_slice(&[v, v, v, chunk[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(buf.len() * 4);
            for &v in &buf {
                out.extend_from_slice(&[v, v, v, 0xff]);
            }
            out
        }
        png::ColorType::Indexed => return None,
    };
    Some(std::sync::Arc::new(egui::IconData {
        rgba,
        width: info.width,
        height: info.height,
    }))
}

fn run_benchmark(path: Option<PathBuf>, queries: Vec<String>) -> Result<(), String> {
    let path = match path {
        Some(path) => path,
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let queries = if queries.is_empty() {
        vec!["md".to_owned(), "readme".to_owned(), "main".to_owned()]
    } else {
        queries
    };

    println!("Lattice workspace benchmark");
    println!("path={}", path.display());

    let total_start = Instant::now();

    let open_start = Instant::now();
    let workspace = Workspace::open_vault(path).map_err(|error| error.to_string())?;
    println!(
        "open_vault_ms={:.3}",
        open_start.elapsed().as_secs_f64() * 1000.0
    );
    println!("vault_name={}", workspace.vault().name);
    println!("vault_root={}", workspace.vault().root.as_path().display());

    let watch_start = Instant::now();
    let watcher_result = workspace.watch();
    println!(
        "watch_root_ms={:.3}",
        watch_start.elapsed().as_secs_f64() * 1000.0
    );
    if let Err(error) = watcher_result {
        println!("watch_root_error={error}");
    }

    let tree_start = Instant::now();
    let tree = workspace
        .list_tree(None)
        .map_err(|error| error.to_string())?;
    let dir_count = tree
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded
            )
        })
        .count();
    let file_count = tree.len().saturating_sub(dir_count);
    println!(
        "root_tree_ms={:.3}",
        tree_start.elapsed().as_secs_f64() * 1000.0
    );
    println!("root_items={}", tree.len());
    println!("root_dirs={dir_count}");
    println!("root_files={file_count}");

    let index_start = Instant::now();
    let index = workspace
        .quick_open_index()
        .map_err(|error| error.to_string())?;
    println!(
        "quick_open_index_ms={:.3}",
        index_start.elapsed().as_secs_f64() * 1000.0
    );
    println!("quick_open_files={}", index.len());

    for query in queries {
        let search_start = Instant::now();
        let matches = index.search(&query, 8);
        println!(
            "quick_open_search_ms query={:?} ms={:.3} matches={}",
            query,
            search_start.elapsed().as_secs_f64() * 1000.0,
            matches.len()
        );

        let content_start = Instant::now();
        let content = workspace
            .search_workspace(
                &WorkspaceSearchOptions {
                    query: SearchQuery {
                        text: query.clone(),
                        ..Default::default()
                    },
                    max_results: WORKSPACE_SEARCH_RESULT_LIMIT,
                    ..Default::default()
                },
                || false,
            )
            .map_err(|error| error.to_string())?;
        println!(
            "content_search_ms query={:?} ms={:.3} matches={} files_scanned={}",
            query,
            content_start.elapsed().as_secs_f64() * 1000.0,
            content.matches.len(),
            content.files_scanned
        );
    }

    println!(
        "total_ms={:.3}",
        total_start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

/// Workspace panes managed by [`egui_tiles`]. Resizing/rearranging is delegated to
/// the tile tree, which uses relative shares (not pixel widths) — that is why we no
/// longer hit the per-frame size drift `egui::Panel::left/right` had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Sidebar,
    Editor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SidebarView {
    #[default]
    Files,
    Search,
}

#[derive(Debug, Default)]
struct WorkspaceSearchUi {
    query: String,
    replacement: String,
    include_globs: String,
    exclude_globs: String,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
    show_replace: bool,
    show_filters: bool,
    focus_pending: bool,
    search_due_at: Option<Instant>,
    active_job: Option<WorkspaceJobId>,
    pending: bool,
    result: Option<WorkspaceSearchResult>,
    error: Option<String>,
    expanded_files: BTreeSet<VaultPath>,
    replace_confirmation: bool,
    replace_pending: bool,
    local_replace_count: usize,
    local_replace_files: usize,
}

impl WorkspaceSearchUi {
    fn options(&self) -> WorkspaceSearchOptions {
        WorkspaceSearchOptions {
            query: SearchQuery {
                text: self.query.clone(),
                case_sensitive: self.case_sensitive,
                whole_word: self.whole_word,
                use_regex: self.use_regex,
            },
            include_globs: parse_globs(&self.include_globs),
            exclude_globs: parse_globs(&self.exclude_globs),
            max_results: WORKSPACE_SEARCH_RESULT_LIMIT,
        }
    }
}

#[derive(Debug, Default)]
struct EditorFindUi {
    visible: bool,
    show_replace: bool,
    focus_pending: bool,
    query: String,
    replacement: String,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
    matches: Vec<TextMatch>,
    current: Option<usize>,
    error: Option<String>,
}

impl EditorFindUi {
    fn query(&self) -> SearchQuery {
        SearchQuery {
            text: self.query.clone(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            use_regex: self.use_regex,
        }
    }
}

struct LatticeApp {
    egui_ctx: egui::Context,
    theme: Theme,
    settings_path: Option<PathBuf>,
    settings: AppSettings,
    workspace: Option<Workspace>,
    watcher: Option<WorkspaceWatcher>,
    worker: Option<WorkspaceWorker>,
    workspace_generation: WorkspaceGeneration,
    next_job_id: u64,
    pending_workspace_refresh_at: Option<Instant>,
    pending_external_check_at: Option<Instant>,
    tree: Vec<TreeNode>,
    expanded_paths: BTreeSet<VaultPath>,
    quick_open: QuickOpenIndex,
    quick_open_ready: bool,
    quick_open_pending: bool,
    quick_open_overlay: bool,
    quick_query: String,
    show_sidebar: bool,
    sidebar_view: SidebarView,
    workspace_search: WorkspaceSearchUi,
    editor_find: EditorFindUi,
    pending_search_navigation: Option<(VaultPath, usize, usize)>,
    tile_tree: egui_tiles::Tree<Pane>,
    sidebar_tile: egui_tiles::TileId,
    selected_path: Option<VaultPath>,
    tabs: Vec<EditorTab>,
    active_tab: Option<usize>,
    new_file_path: String,
    /// When true the sidebar shows an inline input row for the new file path. The
    /// row is normally hidden; clicking the `+` icon button surfaces it. Escape or a
    /// successful create hides it again. Keeping this collapsed by default removes
    /// always-on chrome from the sidebar, which was one of the bigger sources of
    /// visual noise.
    creating_new_file: bool,
    /// Set when the new-file input should grab focus on the next frame. Cleared after
    /// the first frame the input is rendered, so the user can subsequently click off
    /// without us stealing focus back.
    new_file_focus_pending: bool,
    rename_target: String,
    open_error: Option<String>,
    status: String,
    /// Caret position (1-based line, column) of the active editor, refreshed every
    /// frame the editor is drawn; the status bar reads last frame's value.
    cursor_line_col: Option<(usize, usize)>,
}

struct EditorTab {
    buffer: EditorBuffer,
    last_edit: Option<Instant>,
    conflict: Option<FileConflict>,
    /// True while a `SaveFile` command is in flight for this tab, so autosave
    /// does not re-enqueue the same save every frame until the response lands.
    save_in_flight: bool,
    large_file_warning: bool,
    /// Cached line-number gutter text, rebuilt only when the line count changes.
    gutter: String,
    gutter_lines: usize,
    pending_selection: Option<(usize, usize)>,
}

impl EditorTab {
    fn path(&self) -> Option<&VaultPath> {
        self.buffer.path.as_ref()
    }

    fn display_name(&self) -> String {
        self.path()
            .and_then(|path| path.as_path().file_name().map(ToOwned::to_owned))
            .unwrap_or_else(|| "Untitled".to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileConflict {
    ModifiedOnDisk,
    DeletedOnDisk,
}

impl FileConflict {
    fn message(self) -> &'static str {
        match self {
            Self::ModifiedOnDisk => {
                "This file changed on disk. Reload to use the disk version or overwrite disk with this buffer."
            }
            Self::DeletedOnDisk => {
                "This file was deleted on disk. Reload is unavailable; overwrite disk to recreate it."
            }
        }
    }
}

type WorkspaceGeneration = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceJobId(u64);

struct WorkspaceWorker {
    sender: Sender<WorkspaceCommand>,
    receiver: Receiver<WorkspaceResponse>,
    search_revision: Arc<AtomicU64>,
}

#[derive(Debug)]
enum WorkspaceCommand {
    LoadTree {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: Option<VaultPath>,
    },
    BuildQuickOpen {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
    },
    SearchWorkspace {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        options: WorkspaceSearchOptions,
        open_files: Vec<(VaultPath, String)>,
    },
    ReplaceWorkspace {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        options: WorkspaceSearchOptions,
        replacement: String,
        include_paths: Option<BTreeSet<VaultPath>>,
        excluded_paths: BTreeSet<VaultPath>,
    },
    OpenFile {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
    },
    SaveFile {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
        contents: String,
        base_snapshot: Option<OpenFileSnapshot>,
        overwrite: bool,
    },
    CheckExternalChanges {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        tabs: Vec<ExternalTabState>,
    },
    CreateFile {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
    },
    RenamePath {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        from: VaultPath,
        to: VaultPath,
    },
    DeletePath {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
    },
    Shutdown,
}

#[derive(Debug)]
enum WorkspaceResponse {
    TreeLoaded {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: Option<VaultPath>,
        result: Result<Vec<TreeNode>, String>,
    },
    QuickOpenBuilt {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        result: Result<QuickOpenIndex, String>,
    },
    WorkspaceSearched {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        result: Result<WorkspaceSearchResult, String>,
    },
    WorkspaceReplaced {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        result: Result<WorkspaceReplaceReport, String>,
    },
    FileOpened {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
        result: Result<(String, OpenFileSnapshot), String>,
    },
    FileSaved {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
        overwrite: bool,
        result: Result<SaveWorkerResult, String>,
    },
    ExternalChangesChecked {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        changes: Vec<ExternalTabChange>,
    },
    FileCreated {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
        result: Result<(), String>,
    },
    PathRenamed {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        from: VaultPath,
        to: VaultPath,
        result: Result<(), String>,
    },
    PathDeleted {
        job_id: WorkspaceJobId,
        generation: WorkspaceGeneration,
        path: VaultPath,
        result: Result<(), String>,
    },
}

#[derive(Debug, Clone)]
struct ExternalTabState {
    index: usize,
    path: VaultPath,
    base_snapshot: OpenFileSnapshot,
    dirty: bool,
    content_hash: blake3::Hash,
}

#[derive(Debug)]
enum ExternalTabChange {
    MarkSaved {
        index: usize,
        path: VaultPath,
        snapshot: OpenFileSnapshot,
    },
    Conflict {
        index: usize,
        path: VaultPath,
        conflict: FileConflict,
    },
    Reload {
        index: usize,
        path: VaultPath,
        contents: String,
        snapshot: OpenFileSnapshot,
    },
    Close {
        index: usize,
        path: VaultPath,
    },
    Error(String),
}

#[derive(Debug)]
enum SaveWorkerResult {
    Saved(OpenFileSnapshot),
    Conflict,
    Deleted,
    MissingAfterSave,
}

impl WorkspaceWorker {
    fn start(workspace: Workspace, ctx: egui::Context) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (response_sender, response_receiver) = mpsc::channel();
        let search_revision = Arc::new(AtomicU64::new(0));
        let worker_search_revision = Arc::clone(&search_revision);
        thread::spawn(move || {
            while let Ok(command) = command_receiver.recv() {
                if matches!(command, WorkspaceCommand::Shutdown) {
                    break;
                }
                let command = match command {
                    WorkspaceCommand::SearchWorkspace {
                        job_id,
                        generation,
                        options,
                        open_files,
                    } => {
                        let Ok(search_workspace) = workspace.try_clone_for_worker() else {
                            let _ = response_sender.send(WorkspaceResponse::WorkspaceSearched {
                                job_id,
                                generation,
                                result: Err("failed to clone workspace for search".to_owned()),
                            });
                            ctx.request_repaint();
                            continue;
                        };
                        let response_sender = response_sender.clone();
                        let ctx = ctx.clone();
                        let revision = Arc::clone(&worker_search_revision);
                        thread::spawn(move || {
                            let result = search_workspace
                                .search_workspace_with_open_files(&options, &open_files, || {
                                    revision.load(Ordering::Relaxed) != job_id.0
                                })
                                .map_err(|error| error.to_string());
                            let _ = response_sender.send(WorkspaceResponse::WorkspaceSearched {
                                job_id,
                                generation,
                                result,
                            });
                            ctx.request_repaint();
                        });
                        continue;
                    }
                    WorkspaceCommand::ReplaceWorkspace {
                        job_id,
                        generation,
                        options,
                        replacement,
                        include_paths,
                        excluded_paths,
                    } => {
                        // Like search, replace fans out over the whole workspace and
                        // can run for a while; keep the worker free for saves/opens.
                        let Ok(replace_workspace) = workspace.try_clone_for_worker() else {
                            let _ = response_sender.send(WorkspaceResponse::WorkspaceReplaced {
                                job_id,
                                generation,
                                result: Err("failed to clone workspace for replace".to_owned()),
                            });
                            ctx.request_repaint();
                            continue;
                        };
                        let response_sender = response_sender.clone();
                        let ctx = ctx.clone();
                        thread::spawn(move || {
                            let result = replace_workspace
                                .replace_workspace(
                                    &options,
                                    &replacement,
                                    include_paths.as_ref(),
                                    &excluded_paths,
                                    || false,
                                )
                                .map_err(|error| error.to_string());
                            let _ = response_sender.send(WorkspaceResponse::WorkspaceReplaced {
                                job_id,
                                generation,
                                result,
                            });
                            ctx.request_repaint();
                        });
                        continue;
                    }
                    command => command,
                };
                if let Some(response) = run_workspace_command(&workspace, command) {
                    let _ = response_sender.send(response);
                    ctx.request_repaint();
                }
            }
        });
        Self {
            sender: command_sender,
            receiver: response_receiver,
            search_revision,
        }
    }

    fn send(&self, command: WorkspaceCommand) {
        match &command {
            WorkspaceCommand::SearchWorkspace { job_id, .. } => {
                self.search_revision.store(job_id.0, Ordering::Relaxed);
            }
            WorkspaceCommand::ReplaceWorkspace { .. } => {
                self.search_revision.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        let _ = self.sender.send(command);
    }

    fn try_recv(&self) -> Option<WorkspaceResponse> {
        self.receiver.try_recv().ok()
    }

    fn cancel_search(&self) {
        self.search_revision.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for WorkspaceWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(WorkspaceCommand::Shutdown);
    }
}

fn run_workspace_command(
    workspace: &Workspace,
    command: WorkspaceCommand,
) -> Option<WorkspaceResponse> {
    Some(match command {
        WorkspaceCommand::LoadTree {
            job_id,
            generation,
            path,
        } => WorkspaceResponse::TreeLoaded {
            job_id,
            generation,
            result: workspace
                .list_tree(path.as_ref())
                .map_err(|error| error.to_string()),
            path,
        },
        WorkspaceCommand::BuildQuickOpen { job_id, generation } => {
            WorkspaceResponse::QuickOpenBuilt {
                job_id,
                generation,
                result: workspace
                    .quick_open_index()
                    .map_err(|error| error.to_string()),
            }
        }
        WorkspaceCommand::OpenFile {
            job_id,
            generation,
            path,
        } => WorkspaceResponse::FileOpened {
            job_id,
            generation,
            result: workspace
                .open_file(&path)
                .map_err(|error| error.to_string()),
            path,
        },
        WorkspaceCommand::SaveFile {
            job_id,
            generation,
            path,
            contents,
            base_snapshot,
            overwrite,
        } => WorkspaceResponse::FileSaved {
            job_id,
            generation,
            result: save_file_in_worker(workspace, &path, &contents, base_snapshot, overwrite)
                .map_err(|error| error.to_string()),
            path,
            overwrite,
        },
        WorkspaceCommand::CheckExternalChanges {
            job_id,
            generation,
            tabs,
        } => WorkspaceResponse::ExternalChangesChecked {
            job_id,
            generation,
            changes: check_external_changes_in_worker(workspace, tabs),
        },
        WorkspaceCommand::CreateFile {
            job_id,
            generation,
            path,
        } => WorkspaceResponse::FileCreated {
            job_id,
            generation,
            result: workspace
                .create_file(&path, "")
                .map_err(|error| error.to_string()),
            path,
        },
        WorkspaceCommand::RenamePath {
            job_id,
            generation,
            from,
            to,
        } => WorkspaceResponse::PathRenamed {
            job_id,
            generation,
            result: workspace
                .rename_file(&from, &to)
                .map_err(|error| error.to_string()),
            from,
            to,
        },
        WorkspaceCommand::DeletePath {
            job_id,
            generation,
            path,
        } => WorkspaceResponse::PathDeleted {
            job_id,
            generation,
            result: workspace
                .delete_file(&path)
                .map_err(|error| error.to_string()),
            path,
        },
        WorkspaceCommand::SearchWorkspace { .. }
        | WorkspaceCommand::ReplaceWorkspace { .. }
        | WorkspaceCommand::Shutdown => return None,
    })
}

fn save_file_in_worker(
    workspace: &Workspace,
    path: &VaultPath,
    contents: &str,
    base_snapshot: Option<OpenFileSnapshot>,
    overwrite: bool,
) -> Result<SaveWorkerResult, String> {
    fn stringify<T>(result: Result<T, impl std::fmt::Display>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }
    if !overwrite {
        match stringify(workspace.file_snapshot(path))? {
            Some(current_snapshot) => {
                if let Some(base_snapshot) = &base_snapshot {
                    if &current_snapshot != base_snapshot {
                        if current_snapshot.content_hash == blake3::hash(contents.as_bytes()) {
                            return Ok(SaveWorkerResult::Saved(current_snapshot));
                        }
                        return Ok(SaveWorkerResult::Conflict);
                    }
                }
            }
            None => return Ok(SaveWorkerResult::Deleted),
        }
    }

    stringify(workspace.save_file(path, contents))?;
    match stringify(workspace.file_snapshot(path))? {
        Some(snapshot) => Ok(SaveWorkerResult::Saved(snapshot)),
        None => Ok(SaveWorkerResult::MissingAfterSave),
    }
}

fn check_external_changes_in_worker(
    workspace: &Workspace,
    tabs: Vec<ExternalTabState>,
) -> Vec<ExternalTabChange> {
    let mut changes = Vec::new();
    for tab in tabs {
        match workspace.file_snapshot(&tab.path) {
            Ok(Some(current_snapshot)) if current_snapshot == tab.base_snapshot => {}
            Ok(Some(current_snapshot)) if tab.dirty => {
                if current_snapshot.content_hash == tab.content_hash {
                    changes.push(ExternalTabChange::MarkSaved {
                        index: tab.index,
                        path: tab.path,
                        snapshot: current_snapshot,
                    });
                } else {
                    changes.push(ExternalTabChange::Conflict {
                        index: tab.index,
                        path: tab.path,
                        conflict: FileConflict::ModifiedOnDisk,
                    });
                }
            }
            Ok(Some(_)) => match workspace.open_file(&tab.path) {
                Ok((contents, snapshot)) => changes.push(ExternalTabChange::Reload {
                    index: tab.index,
                    path: tab.path,
                    contents,
                    snapshot,
                }),
                Err(error) => changes.push(ExternalTabChange::Error(error.to_string())),
            },
            Ok(None) if tab.dirty => changes.push(ExternalTabChange::Conflict {
                index: tab.index,
                path: tab.path,
                conflict: FileConflict::DeletedOnDisk,
            }),
            Ok(None) => changes.push(ExternalTabChange::Close {
                index: tab.index,
                path: tab.path,
            }),
            Err(error) => changes.push(ExternalTabChange::Error(error.to_string())),
        }
    }
    changes
}

impl LatticeApp {
    fn new(ctx: &egui::Context, path: Option<PathBuf>) -> Self {
        let (settings_path, settings, settings_error) = load_settings();
        let theme = match settings.theme {
            ThemeMode::Light => Theme::light(),
            _ => Theme::dark(),
        };
        theme.apply(ctx);

        let (tile_tree, sidebar_tile) = build_tile_tree();
        let mut app = Self {
            egui_ctx: ctx.clone(),
            theme,
            settings_path,
            settings,
            workspace: None,
            watcher: None,
            worker: None,
            workspace_generation: 0,
            next_job_id: 0,
            pending_workspace_refresh_at: None,
            pending_external_check_at: None,
            tree: Vec::new(),
            expanded_paths: BTreeSet::new(),
            quick_open: QuickOpenIndex::default(),
            quick_open_ready: false,
            quick_open_pending: false,
            quick_open_overlay: false,
            quick_query: String::new(),
            show_sidebar: true,
            sidebar_view: SidebarView::Files,
            workspace_search: WorkspaceSearchUi::default(),
            editor_find: EditorFindUi::default(),
            pending_search_navigation: None,
            tile_tree,
            sidebar_tile,
            selected_path: None,
            tabs: Vec::new(),
            active_tab: None,
            new_file_path: "untitled".to_owned(),
            creating_new_file: false,
            new_file_focus_pending: false,
            rename_target: String::new(),
            open_error: settings_error,
            status: "Ready".to_owned(),
            cursor_line_col: None,
        };
        if let Some(path) = path {
            app.open_path(path);
        }
        app
    }

    fn next_workspace_job_id(&mut self) -> WorkspaceJobId {
        let job_id = WorkspaceJobId(self.next_job_id);
        self.next_job_id = self.next_job_id.wrapping_add(1);
        job_id
    }

    fn send_workspace_command(&mut self, command: WorkspaceCommand) {
        if let Some(worker) = &self.worker {
            worker.send(command);
        }
    }

    fn enqueue_tree_load(&mut self, path: Option<VaultPath>) {
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.send_workspace_command(WorkspaceCommand::LoadTree {
            job_id,
            generation,
            path,
        });
    }

    fn enqueue_quick_open_index(&mut self) {
        if self.quick_open_ready || self.quick_open_pending {
            return;
        }
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.quick_open_pending = true;
        self.send_workspace_command(WorkspaceCommand::BuildQuickOpen { job_id, generation });
    }

    fn open_workspace_search(&mut self, show_replace: bool) {
        self.sidebar_view = SidebarView::Search;
        self.show_sidebar = true;
        self.workspace_search.show_replace |= show_replace;
        self.workspace_search.focus_pending = true;
        if !self.workspace_search.query.trim().is_empty() {
            self.schedule_workspace_search();
        }
    }

    fn schedule_workspace_search(&mut self) {
        self.workspace_search.replace_confirmation = false;
        if self.workspace_search.query.trim().is_empty() {
            if let Some(worker) = &self.worker {
                worker.cancel_search();
            }
            self.workspace_search.active_job = None;
            self.workspace_search.pending = false;
            self.workspace_search.result = None;
            self.workspace_search.error = None;
            self.workspace_search.search_due_at = None;
            return;
        }
        self.workspace_search.search_due_at = Some(Instant::now() + WORKSPACE_SEARCH_DEBOUNCE);
        self.egui_ctx
            .request_repaint_after(WORKSPACE_SEARCH_DEBOUNCE);
    }

    fn process_pending_workspace_search(&mut self) {
        let Some(due_at) = self.workspace_search.search_due_at else {
            return;
        };
        let now = Instant::now();
        if now < due_at {
            self.egui_ctx.request_repaint_after(due_at - now);
            return;
        }
        self.workspace_search.search_due_at = None;
        let options = self.workspace_search.options();
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.workspace_search.active_job = Some(job_id);
        self.workspace_search.pending = true;
        self.workspace_search.error = None;
        self.send_workspace_command(WorkspaceCommand::SearchWorkspace {
            job_id,
            generation,
            options,
            open_files: self
                .tabs
                .iter()
                .filter_map(|tab| {
                    tab.path()
                        .cloned()
                        .map(|path| (path, tab.buffer.text.clone()))
                })
                .collect(),
        });
    }

    fn replace_workspace_matches(&mut self, include_paths: Option<BTreeSet<VaultPath>>) {
        if self.workspace_search.replace_pending || self.workspace_search.query.is_empty() {
            return;
        }
        let options = self.workspace_search.options();
        if let Err(error) = find_text_matches("", &options.query, 1) {
            self.workspace_search.error = Some(error.to_string());
            return;
        }

        let replacement = self.workspace_search.replacement.clone();
        let mut changed_tabs = Vec::new();
        let mut local_replace_count = 0;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(path) = tab.path() else {
                continue;
            };
            let explicitly_included = include_paths
                .as_ref()
                .is_none_or(|paths| paths.contains(path));
            let matches_filters = search_path_matches(path, &options).unwrap_or(false);
            if !explicitly_included || !matches_filters {
                continue;
            }
            match replace_all_text(&tab.buffer.text, &options.query, &replacement) {
                Ok((updated, count)) if count > 0 => {
                    tab.buffer.text = updated;
                    tab.buffer.dirty = true;
                    tab.last_edit = Some(Instant::now());
                    local_replace_count += count;
                    changed_tabs.push(index);
                }
                Ok(_) => {}
                Err(error) => {
                    self.workspace_search.error = Some(error.to_string());
                    return;
                }
            }
        }

        let excluded_paths = self
            .tabs
            .iter()
            .filter_map(|tab| tab.path().cloned())
            .collect();
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.workspace_search.replace_pending = true;
        self.workspace_search.replace_confirmation = false;
        self.workspace_search.local_replace_count = local_replace_count;
        self.workspace_search.local_replace_files = changed_tabs.len();
        self.status = "Replacing matches...".to_owned();
        self.send_workspace_command(WorkspaceCommand::ReplaceWorkspace {
            job_id,
            generation,
            options,
            replacement,
            include_paths,
            excluded_paths,
        });
        for index in changed_tabs {
            self.save_tab(index);
        }
        self.refresh_editor_find_matches();
    }

    fn drain_worker_responses(&mut self) {
        let mut drained = 0;
        while drained < MAX_WORKER_RESPONSES_PER_FRAME {
            let Some(response) = self.worker.as_ref().and_then(WorkspaceWorker::try_recv) else {
                return;
            };
            self.apply_worker_response(response);
            drained += 1;
        }
        self.egui_ctx.request_repaint();
    }

    fn apply_worker_response(&mut self, response: WorkspaceResponse) {
        let (_job_id, generation) = match &response {
            WorkspaceResponse::TreeLoaded {
                job_id, generation, ..
            }
            | WorkspaceResponse::QuickOpenBuilt {
                job_id, generation, ..
            }
            | WorkspaceResponse::WorkspaceSearched {
                job_id, generation, ..
            }
            | WorkspaceResponse::WorkspaceReplaced {
                job_id, generation, ..
            }
            | WorkspaceResponse::FileOpened {
                job_id, generation, ..
            }
            | WorkspaceResponse::FileSaved {
                job_id, generation, ..
            }
            | WorkspaceResponse::ExternalChangesChecked {
                job_id, generation, ..
            }
            | WorkspaceResponse::FileCreated {
                job_id, generation, ..
            }
            | WorkspaceResponse::PathRenamed {
                job_id, generation, ..
            }
            | WorkspaceResponse::PathDeleted {
                job_id, generation, ..
            } => (*job_id, *generation),
        };
        if generation != self.workspace_generation {
            return;
        }

        match response {
            WorkspaceResponse::TreeLoaded {
                path,
                result,
                job_id: _,
                generation: _,
            } => self.apply_tree_loaded(path, result),
            WorkspaceResponse::QuickOpenBuilt {
                result,
                job_id: _,
                generation: _,
            } => {
                self.quick_open_pending = false;
                match result {
                    Ok(index) => {
                        self.status = format!("Indexed {} files for quick open", index.len());
                        self.quick_open = index;
                        self.quick_open_ready = true;
                        self.open_error = None;
                    }
                    Err(error) => {
                        self.open_error = Some(error);
                        self.status = "Quick open index failed".to_owned();
                    }
                }
            }
            WorkspaceResponse::WorkspaceSearched {
                job_id,
                result,
                generation: _,
            } => {
                if self.workspace_search.active_job != Some(job_id) {
                    return;
                }
                self.workspace_search.pending = false;
                match result {
                    Ok(result) => {
                        self.status = format!(
                            "Found {} results in {} files",
                            result.matches.len(),
                            result.files_with_matches
                        );
                        self.workspace_search.expanded_files = result
                            .matches
                            .iter()
                            .map(|matched| matched.path.clone())
                            .collect();
                        self.workspace_search.result = Some(result);
                        self.workspace_search.error = None;
                    }
                    Err(error) => {
                        self.workspace_search.result = None;
                        self.workspace_search.error = Some(error);
                        self.status = "Workspace search failed".to_owned();
                    }
                }
            }
            WorkspaceResponse::WorkspaceReplaced {
                result,
                job_id: _,
                generation: _,
            } => {
                self.workspace_search.replace_pending = false;
                match result {
                    Ok(report) => {
                        let replacements =
                            report.replacements + self.workspace_search.local_replace_count;
                        let files = report.files_changed + self.workspace_search.local_replace_files;
                        self.workspace_search.local_replace_count = 0;
                        self.workspace_search.local_replace_files = 0;
                        if report.errors.is_empty() {
                            self.status = format!("Replaced {replacements} matches in {files} files");
                            self.open_error = None;
                        } else {
                            self.status = format!(
                                "Replaced {replacements} matches with {} errors",
                                report.errors.len()
                            );
                            self.open_error = Some(report.errors.join("\n"));
                        }
                        self.refresh_workspace_data();
                    }
                    Err(error) => {
                        self.workspace_search.local_replace_count = 0;
                        self.workspace_search.local_replace_files = 0;
                        self.workspace_search.error = Some(error);
                        self.status = "Replace in files failed".to_owned();
                    }
                }
            }
            WorkspaceResponse::FileOpened {
                path,
                result,
                job_id: _,
                generation: _,
            } => self.apply_file_opened(path, result),
            WorkspaceResponse::FileSaved {
                path,
                overwrite,
                result,
                job_id: _,
                generation: _,
            } => self.apply_file_saved(path, result, overwrite),
            WorkspaceResponse::ExternalChangesChecked {
                changes,
                job_id: _,
                generation: _,
            } => self.apply_external_changes(changes),
            WorkspaceResponse::FileCreated {
                path,
                result,
                job_id: _,
                generation: _,
            } => match result {
                Ok(()) => {
                    self.selected_path = Some(path.clone());
                    self.rename_target = path.as_str().to_owned();
                    self.status = format!("Created {}", path.as_str());
                    self.open_error = None;
                    self.refresh_workspace_data();
                    self.open_editor_file(path);
                }
                Err(error) => {
                    self.open_error = Some(error);
                    self.status = "Create file failed".to_owned();
                }
            },
            WorkspaceResponse::PathRenamed {
                from,
                to,
                result,
                job_id: _,
                generation: _,
            } => match result {
                Ok(()) => {
                    self.update_tab_path_after_rename(&from, &to);
                    self.selected_path = Some(to.clone());
                    self.rename_target = to.as_str().to_owned();
                    self.status = format!("Renamed to {}", to.as_str());
                    self.open_error = None;
                    self.refresh_workspace_data();
                }
                Err(error) => {
                    self.open_error = Some(error);
                    self.status = "Rename failed".to_owned();
                }
            },
            WorkspaceResponse::PathDeleted {
                path,
                result,
                job_id: _,
                generation: _,
            } => match result {
                Ok(()) => {
                    self.status = format!("Deleted {}", path.as_str());
                    self.close_tab_for_path(&path);
                    self.selected_path = None;
                    self.rename_target.clear();
                    self.open_error = None;
                    self.refresh_workspace_data();
                }
                Err(error) => {
                    self.open_error = Some(error);
                    self.status = "Delete failed".to_owned();
                }
            },
        }
    }

    fn apply_tree_loaded(
        &mut self,
        path: Option<VaultPath>,
        result: Result<Vec<TreeNode>, String>,
    ) {
        match result {
            Ok(children) => {
                if let Some(path) = path {
                    if replace_tree_children(&mut self.tree, &path, children) {
                        self.expanded_paths.insert(path.clone());
                        self.watch_tree_path(&path);
                        self.open_error = None;
                    }
                } else {
                    self.tree = children;
                    let expanded_paths = self.sorted_expanded_paths();
                    for path in expanded_paths {
                        self.enqueue_tree_load(Some(path));
                    }
                    self.open_error = None;
                }
            }
            Err(error) => {
                self.open_error = Some(error);
                self.status = if path.is_some() {
                    "Could not load directory".to_owned()
                } else {
                    "Refresh failed".to_owned()
                };
            }
        }
    }

    fn apply_file_opened(
        &mut self,
        path: VaultPath,
        result: Result<(String, OpenFileSnapshot), String>,
    ) {
        match result {
            Ok((contents, snapshot)) => {
                if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| tab.path().is_some_and(|tab_path| tab_path == &path))
                {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.buffer.text = contents;
                        tab.buffer.mark_saved(snapshot);
                        tab.conflict = None;
                        tab.last_edit = None;
                        tab.large_file_warning =
                            tab.buffer.base_snapshot.as_ref().is_some_and(|snapshot| {
                                snapshot.size_bytes > LARGE_FILE_WARNING_BYTES
                            });
                    }
                    self.active_tab = Some(index);
                    self.selected_path = Some(path.clone());
                    self.refresh_editor_find_matches();
                    self.apply_pending_search_navigation(&path, index);
                    self.status = "Reloaded file".to_owned();
                    return;
                }
                let large_file_warning = snapshot.size_bytes > LARGE_FILE_WARNING_BYTES;
                let tab = EditorTab {
                    buffer: EditorBuffer::from_disk(path.clone(), contents, snapshot),
                    last_edit: None,
                    conflict: None,
                    save_in_flight: false,
                    large_file_warning,
                    gutter: String::new(),
                    gutter_lines: 0,
                    pending_selection: None,
                };
                self.tabs.push(tab);
                self.active_tab = Some(self.tabs.len() - 1);
                let index = self.tabs.len() - 1;
                self.selected_path = Some(path.clone());
                self.rename_target = path.as_str().to_owned();
                self.refresh_editor_find_matches();
                self.apply_pending_search_navigation(&path, index);
                self.status = format!("Opened {}", path.as_str());
                self.open_error = None;
            }
            Err(error) => {
                self.open_error = Some(error);
                self.status = "Open file failed".to_owned();
            }
        }
    }

    fn apply_file_saved(
        &mut self,
        path: VaultPath,
        result: Result<SaveWorkerResult, String>,
        overwrite: bool,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.path().is_some_and(|tab_path| tab_path == &path))
        else {
            return;
        };
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.save_in_flight = false;
        }
        match result {
            Ok(SaveWorkerResult::Saved(snapshot)) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    // The buffer may have been edited while the save was in
                    // flight; the snapshot is the new disk state either way, but
                    // only clear dirty when the buffer still matches it so those
                    // newer edits stay scheduled for the next autosave.
                    let buffer_matches_disk =
                        tab.buffer.content_hash() == snapshot.content_hash;
                    tab.buffer.base_snapshot = Some(snapshot);
                    if buffer_matches_disk {
                        tab.buffer.dirty = false;
                        tab.last_edit = None;
                    }
                    tab.conflict = None;
                }
                self.status = if overwrite {
                    format!("Overwrote {}", path.as_str())
                } else {
                    format!("Saved {}", path.as_str())
                };
                self.open_error = None;
                self.quick_open_ready = false;
            }
            Ok(SaveWorkerResult::Conflict) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.conflict = Some(FileConflict::ModifiedOnDisk);
                }
                self.status = format!("Conflict on {}", path.as_str());
            }
            Ok(SaveWorkerResult::Deleted) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.conflict = Some(FileConflict::DeletedOnDisk);
                }
                self.status = format!("File deleted on disk: {}", path.as_str());
            }
            Ok(SaveWorkerResult::MissingAfterSave) => {
                self.open_error = Some("saved file disappeared before metadata refresh".to_owned());
                self.status = "Save metadata failed".to_owned();
            }
            Err(error) => {
                self.open_error = Some(error);
                self.status = if overwrite {
                    "Overwrite failed".to_owned()
                } else {
                    "Save failed".to_owned()
                };
            }
        }
    }

    fn apply_external_changes(&mut self, changes: Vec<ExternalTabChange>) {
        let mut close_indexes = Vec::new();
        for change in changes {
            match change {
                ExternalTabChange::MarkSaved {
                    index,
                    path,
                    snapshot,
                } => {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        if tab.path().is_some_and(|tab_path| tab_path == &path) {
                            // The check ran against a buffer hash captured when it
                            // was enqueued; the user may have typed since. Adopt
                            // the disk snapshot as the save base, but keep dirty
                            // set unless the current buffer really matches disk.
                            let buffer_matches_disk =
                                tab.buffer.content_hash() == snapshot.content_hash;
                            tab.buffer.base_snapshot = Some(snapshot);
                            if buffer_matches_disk {
                                tab.buffer.dirty = false;
                                tab.conflict = None;
                            }
                        }
                    }
                }
                ExternalTabChange::Conflict {
                    index,
                    path,
                    conflict,
                } => {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        if tab.path().is_some_and(|tab_path| tab_path == &path) {
                            tab.conflict = Some(conflict);
                        }
                    }
                }
                ExternalTabChange::Reload {
                    index,
                    path,
                    contents,
                    snapshot,
                } => {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        if tab.path().is_some_and(|tab_path| tab_path == &path) && !tab.buffer.dirty
                        {
                            tab.buffer.text = contents;
                            tab.buffer.mark_saved(snapshot);
                            tab.conflict = None;
                            tab.last_edit = None;
                        }
                    }
                }
                ExternalTabChange::Close { index, path } => {
                    if self
                        .tabs
                        .get(index)
                        .is_some_and(|tab| tab.path().is_some_and(|tab_path| tab_path == &path))
                    {
                        close_indexes.push(index);
                    }
                }
                ExternalTabChange::Error(error) => {
                    self.open_error = Some(error);
                    self.status = "External change check failed".to_owned();
                }
            }
        }
        close_indexes.sort_unstable();
        close_indexes.dedup();
        for index in close_indexes.into_iter().rev() {
            self.tabs.remove(index);
            self.active_tab = adjusted_active_tab(self.active_tab, index, self.tabs.len());
        }
        self.refresh_editor_find_matches();
    }

    fn sorted_expanded_paths(&self) -> Vec<VaultPath> {
        let mut expanded_paths: Vec<_> = self.expanded_paths.iter().cloned().collect();
        expanded_paths.sort_by_key(|path| path.as_str().matches('/').count());
        expanded_paths
    }

    /// Open a folder as the workspace. A file path opens its parent folder and
    /// then the file itself (`lattice src/main.rs` behaves like VSCode).
    fn open_path(&mut self, path: PathBuf) {
        let (folder, file) = if path.is_file() {
            let folder = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            (folder, Some(path))
        } else {
            (path, None)
        };
        match Workspace::open_vault(folder) {
            Ok(workspace) => {
                self.set_workspace(workspace);
                if let Some(file) = file {
                    let root = self
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.vault().root.as_path().to_path_buf());
                    if let (Some(root), Ok(file)) = (root, file.canonicalize()) {
                        if let Some(vault_path) = vault_path_from_absolute(&root, &file) {
                            self.open_editor_file(vault_path);
                        }
                    }
                }
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Open failed".to_owned();
            }
        }
    }

    fn set_workspace(&mut self, workspace: Workspace) {
        let root = workspace.vault().root.as_path().to_path_buf();
        let worker = match workspace.try_clone_for_worker() {
            Ok(workspace) => Some(WorkspaceWorker::start(workspace, self.egui_ctx.clone())),
            Err(error) => {
                log::warn!("failed to start workspace worker: {error}");
                None
            }
        };
        self.watcher = match workspace.watch() {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                log::warn!("failed to start workspace watcher: {error}");
                None
            }
        };
        self.settings.remember_vault(root);
        self.save_settings();
        self.status = format!("Opened {}", workspace.vault().name);
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        self.worker = worker;
        self.workspace = Some(workspace);
        self.pending_workspace_refresh_at = None;
        self.pending_external_check_at = None;
        self.selected_path = None;
        self.tabs.clear();
        self.active_tab = None;
        self.rename_target.clear();
        self.expanded_paths.clear();
        self.quick_open = QuickOpenIndex::default();
        self.quick_open_ready = false;
        self.quick_open_pending = false;
        self.quick_query.clear();
        self.sidebar_view = SidebarView::Files;
        self.workspace_search = WorkspaceSearchUi::default();
        self.editor_find = EditorFindUi::default();
        self.pending_search_navigation = None;
        self.open_error = None;
        self.refresh_tree_root();
    }

    fn open_folder_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open Folder")
            .pick_folder()
        {
            self.open_path(path);
        }
    }

    fn refresh_tree_root(&mut self) {
        self.enqueue_tree_load(None);
    }

    fn refresh_workspace_data(&mut self) {
        self.refresh_tree_root();
        self.quick_open_ready = false;
        self.quick_open_pending = false;
        self.quick_open = QuickOpenIndex::default();
        if self.sidebar_view == SidebarView::Search
            && !self.workspace_search.query.trim().is_empty()
        {
            self.schedule_workspace_search();
        }
    }

    fn drain_watcher(&mut self) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let events = watcher.drain();
        if events.is_empty() {
            return;
        }

        let mut tree_changed = false;
        let mut file_changed = false;
        if let Some(workspace) = &self.workspace {
            let root = workspace.vault().root.as_path();
            for event in &events {
                // No filesystem calls here — this runs on the UI thread every
                // frame. Create/Rename can change the file set (and only a real
                // walk applies .gitignore correctly), so just mark the quick-open
                // index stale; it is rebuilt lazily on the worker thread. Remove
                // is a pure string operation, so patch the index in place.
                match event.kind {
                    WorkspaceEventKind::Create | WorkspaceEventKind::Rename => {
                        tree_changed = true;
                        self.quick_open_ready = false;
                    }
                    WorkspaceEventKind::Modify => {
                        file_changed = true;
                    }
                    WorkspaceEventKind::Remove => {
                        tree_changed = true;
                        for path in &event.paths {
                            if let Some(vault_path) = vault_path_from_absolute(root, path) {
                                self.quick_open.remove(&vault_path);
                            }
                        }
                    }
                    WorkspaceEventKind::Other => {}
                }
            }
        }

        if tree_changed {
            self.pending_workspace_refresh_at = Some(Instant::now() + WATCHER_REFRESH_DEBOUNCE);
        }
        if file_changed {
            self.pending_external_check_at = Some(Instant::now() + WATCHER_REFRESH_DEBOUNCE);
        }
    }

    fn process_pending_workspace_refresh(&mut self) {
        let Some(refresh_at) = self.pending_workspace_refresh_at else {
            return;
        };
        if Instant::now() < refresh_at {
            return;
        }
        self.pending_workspace_refresh_at = None;
        self.refresh_tree_root();
        self.check_external_changes();
    }

    fn process_pending_external_check(&mut self) {
        let Some(check_at) = self.pending_external_check_at else {
            return;
        };
        if Instant::now() < check_at {
            return;
        }
        self.pending_external_check_at = None;
        self.check_external_changes();
    }

    fn ensure_quick_open_index(&mut self) {
        self.enqueue_quick_open_index();
    }

    fn create_file(&mut self) {
        match VaultPath::try_from(self.new_file_path.as_str()) {
            Ok(path) => {
                let generation = self.workspace_generation;
                let job_id = self.next_workspace_job_id();
                self.status = format!("Creating {}", path.as_str());
                self.send_workspace_command(WorkspaceCommand::CreateFile {
                    job_id,
                    generation,
                    path,
                });
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Invalid file path".to_owned();
            }
        }
    }

    fn start_new_file(&mut self) {
        self.creating_new_file = true;
        self.new_file_path = "untitled".to_owned();
        self.new_file_focus_pending = true;
    }

    fn rename_selected(&mut self) {
        let Some(from) = self.selected_path.clone() else {
            return;
        };
        match VaultPath::try_from(self.rename_target.as_str()) {
            Ok(to) => {
                let generation = self.workspace_generation;
                let job_id = self.next_workspace_job_id();
                self.status = format!("Renaming {}", from.as_str());
                self.send_workspace_command(WorkspaceCommand::RenamePath {
                    job_id,
                    generation,
                    from,
                    to,
                });
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Invalid rename path".to_owned();
            }
        }
    }

    fn delete_selected(&mut self) {
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        if self
            .tabs
            .iter()
            .any(|tab| tab.path().is_some_and(|tab_path| tab_path == &path) && tab.buffer.dirty)
        {
            self.status = "Save or reload dirty file before deleting".to_owned();
            return;
        }
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.status = format!("Deleting {}", path.as_str());
        self.send_workspace_command(WorkspaceCommand::DeletePath {
            job_id,
            generation,
            path,
        });
    }

    fn open_editor_file(&mut self, path: VaultPath) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.path().is_some_and(|tab_path| tab_path == &path))
        {
            self.active_tab = Some(index);
            self.selected_path = Some(path);
            self.refresh_editor_find_matches();
            return;
        }

        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.status = format!("Opening {}", path.as_str());
        self.send_workspace_command(WorkspaceCommand::OpenFile {
            job_id,
            generation,
            path,
        });
    }

    fn open_search_match(&mut self, matched: SearchMatch) {
        let path = matched.path.clone();
        self.pending_search_navigation = Some((path.clone(), matched.byte_start, matched.byte_end));
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.path().is_some_and(|tab_path| tab_path == &path))
        {
            self.active_tab = Some(index);
            self.selected_path = Some(path.clone());
            self.refresh_editor_find_matches();
            self.apply_pending_search_navigation(&path, index);
        } else {
            self.open_editor_file(path);
        }
    }

    fn apply_pending_search_navigation(&mut self, path: &VaultPath, index: usize) {
        let Some((pending_path, start, end)) = self.pending_search_navigation.clone() else {
            return;
        };
        if &pending_path != path {
            return;
        }
        if let Some(tab) = self.tabs.get_mut(index) {
            let len = tab.buffer.text.len();
            tab.pending_selection = Some((start.min(len), end.min(len)));
        }
        self.pending_search_navigation = None;
    }

    fn editor_text_id(&self, index: usize) -> egui::Id {
        let key = self
            .tabs
            .get(index)
            .and_then(EditorTab::path)
            .map(|path| path.as_str())
            .unwrap_or("__untitled");
        egui::Id::new(("editor_text", key))
    }

    fn open_editor_find(&mut self, show_replace: bool) {
        let was_visible = self.editor_find.visible;
        self.editor_find.visible = true;
        self.editor_find.show_replace |= show_replace;
        self.editor_find.focus_pending = true;
        if !was_visible {
            if let Some(index) = self.active_tab {
                let id = self.editor_text_id(index);
                if let (Some(tab), Some(state)) = (
                    self.tabs.get(index),
                    egui::text_edit::TextEditState::load(&self.egui_ctx, id),
                ) {
                    if let Some(range) = state.cursor.char_range() {
                        let range = range.as_sorted_char_range();
                        if range.start != range.end {
                            if let Some(selected) = char_slice(&tab.buffer.text, range) {
                                if !selected.contains('\n') {
                                    self.editor_find.query = selected.to_owned();
                                }
                            }
                        }
                    }
                }
            }
        }
        self.refresh_editor_find_matches();
    }

    fn refresh_editor_find_matches(&mut self) {
        let previous_start = self
            .editor_find
            .current
            .and_then(|index| self.editor_find.matches.get(index))
            .map(|matched| matched.byte_start);
        self.editor_find.matches.clear();
        self.editor_find.current = None;
        self.editor_find.error = None;
        if !self.editor_find.visible || self.editor_find.query.is_empty() {
            return;
        }
        let Some(text) = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.buffer.text.as_str())
        else {
            return;
        };
        match find_text_matches(text, &self.editor_find.query(), usize::MAX) {
            Ok(matches) => {
                self.editor_find.matches = matches;
                if !self.editor_find.matches.is_empty() {
                    self.editor_find.current = Some(
                        previous_start
                            .and_then(|start| {
                                self.editor_find
                                    .matches
                                    .iter()
                                    .position(|matched| matched.byte_start >= start)
                            })
                            .unwrap_or(0),
                    );
                    if let (Some(tab_index), Some(match_index)) =
                        (self.active_tab, self.editor_find.current)
                    {
                        let matched = self.editor_find.matches[match_index].clone();
                        if let Some(tab) = self.tabs.get_mut(tab_index) {
                            tab.pending_selection = Some((matched.byte_start, matched.byte_end));
                        }
                    }
                }
            }
            Err(error) => self.editor_find.error = Some(error.to_string()),
        }
    }

    fn navigate_editor_find(&mut self, delta: isize) {
        if self.editor_find.matches.is_empty() {
            return;
        }
        let len = self.editor_find.matches.len() as isize;
        let current = self.editor_find.current.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.editor_find.current = Some(next);
        let matched = self.editor_find.matches[next].clone();
        if let Some(tab) = self.active_tab.and_then(|index| self.tabs.get_mut(index)) {
            tab.pending_selection = Some((matched.byte_start, matched.byte_end));
        }
    }

    fn select_editor_find_at_or_after(&mut self, byte_offset: usize) {
        if self.editor_find.matches.is_empty() {
            self.editor_find.current = None;
            return;
        }
        let index = self
            .editor_find
            .matches
            .iter()
            .position(|matched| matched.byte_start >= byte_offset)
            .unwrap_or(0);
        self.editor_find.current = Some(index);
        let matched = self.editor_find.matches[index].clone();
        if let Some(tab) = self.active_tab.and_then(|index| self.tabs.get_mut(index)) {
            tab.pending_selection = Some((matched.byte_start, matched.byte_end));
        }
    }

    fn replace_current_editor_match(&mut self) {
        let Some(match_index) = self.editor_find.current else {
            return;
        };
        let Some(matched) = self.editor_find.matches.get(match_index).cloned() else {
            return;
        };
        let query = self.editor_find.query();
        let replacement = self.editor_find.replacement.clone();
        let Some(tab_index) = self.active_tab else {
            return;
        };
        let result = self
            .tabs
            .get(tab_index)
            .map(|tab| replace_text_match(&tab.buffer.text, &query, &matched, &replacement));
        match result {
            Some(Ok(Some(updated))) => {
                let old_len = self.tabs[tab_index].buffer.text.len();
                let removed_len = matched.byte_end.saturating_sub(matched.byte_start);
                let inserted_len = updated.len().saturating_sub(old_len - removed_len);
                let selection_end = matched.byte_start + inserted_len;
                if let Some(tab) = self.tabs.get_mut(tab_index) {
                    tab.buffer.text = updated;
                    tab.buffer.dirty = true;
                    tab.last_edit = Some(Instant::now());
                    tab.pending_selection = Some((matched.byte_start, selection_end));
                }
                self.refresh_editor_find_matches();
                self.select_editor_find_at_or_after(selection_end);
            }
            Some(Err(error)) => self.editor_find.error = Some(error.to_string()),
            _ => self.refresh_editor_find_matches(),
        }
    }

    fn replace_all_editor_matches(&mut self) {
        let query = self.editor_find.query();
        let replacement = self.editor_find.replacement.clone();
        let Some(tab_index) = self.active_tab else {
            return;
        };
        let Some(tab) = self.tabs.get(tab_index) else {
            return;
        };
        match replace_all_text(&tab.buffer.text, &query, &replacement) {
            Ok((updated, count)) if count > 0 => {
                if let Some(tab) = self.tabs.get_mut(tab_index) {
                    tab.buffer.text = updated;
                    tab.buffer.dirty = true;
                    tab.last_edit = Some(Instant::now());
                }
                self.status = format!("Replaced {count} matches");
                self.refresh_editor_find_matches();
            }
            Ok(_) => {}
            Err(error) => self.editor_find.error = Some(error.to_string()),
        }
    }

    fn close_tab_for_path(&mut self, path: &VaultPath) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.path().is_some_and(|tab_path| tab_path == path))
        else {
            return;
        };
        self.tabs.remove(index);
        self.active_tab = adjusted_active_tab(self.active_tab, index, self.tabs.len());
        self.refresh_editor_find_matches();
    }

    fn update_tab_path_after_rename(&mut self, from: &VaultPath, to: &VaultPath) {
        for tab in &mut self.tabs {
            if tab.path().is_some_and(|path| path == from) {
                tab.buffer.path = Some(to.clone());
            }
        }
    }

    fn save_active_tab(&mut self) {
        if let Some(index) = self.active_tab {
            self.save_tab(index);
        }
    }

    fn save_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let Some(path) = tab.path().cloned() else {
            return;
        };
        let contents = tab.buffer.text.clone();
        let base_snapshot = tab.buffer.base_snapshot.clone();
        tab.save_in_flight = true;
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.status = format!("Saving {}", path.as_str());
        self.send_workspace_command(WorkspaceCommand::SaveFile {
            job_id,
            generation,
            path,
            contents,
            base_snapshot,
            overwrite: false,
        });
    }

    fn overwrite_active_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let Some(path) = tab.path().cloned() else {
            return;
        };
        let contents = tab.buffer.text.clone();
        tab.save_in_flight = true;
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.status = format!("Overwriting {}", path.as_str());
        self.send_workspace_command(WorkspaceCommand::SaveFile {
            job_id,
            generation,
            path,
            contents,
            base_snapshot: None,
            overwrite: true,
        });
    }

    fn reload_active_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        self.reload_tab(index);
    }

    fn reload_tab(&mut self, index: usize) {
        let Some(path) = self.tabs.get(index).and_then(EditorTab::path).cloned() else {
            return;
        };
        let generation = self.workspace_generation;
        let job_id = self.next_workspace_job_id();
        self.status = format!("Reloading {}", path.as_str());
        self.send_workspace_command(WorkspaceCommand::OpenFile {
            job_id,
            generation,
            path,
        });
    }

    fn run_autosave(&mut self) {
        let now = Instant::now();
        let mut indexes = Vec::new();
        let mut next_due: Option<Duration> = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            if !tab.buffer.dirty || tab.conflict.is_some() || tab.save_in_flight {
                continue;
            }
            let Some(last_edit) = tab.last_edit else {
                continue;
            };
            let elapsed = now.duration_since(last_edit);
            if elapsed >= AUTOSAVE_DEBOUNCE {
                indexes.push(index);
            } else {
                let wait = AUTOSAVE_DEBOUNCE - elapsed;
                next_due = Some(next_due.map_or(wait, |due| due.min(wait)));
            }
        }
        for index in indexes {
            self.save_tab(index);
        }
        // Make sure a frame runs when the debounce elapses, even if the window
        // is otherwise idle; egui only repaints on input by default.
        if let Some(wait) = next_due {
            self.egui_ctx.request_repaint_after(wait);
        }
    }

    fn check_external_changes(&mut self) {
        let mut tabs = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let Some(path) = tab.path().cloned() else {
                continue;
            };
            let Some(base_snapshot) = tab.buffer.base_snapshot.clone() else {
                continue;
            };
            tabs.push(ExternalTabState {
                index,
                path,
                base_snapshot,
                dirty: tab.buffer.dirty,
                content_hash: tab.buffer.content_hash(),
            });
        }
        if !tabs.is_empty() {
            let generation = self.workspace_generation;
            let job_id = self.next_workspace_job_id();
            self.send_workspace_command(WorkspaceCommand::CheckExternalChanges {
                job_id,
                generation,
                tabs,
            });
        }
    }

    fn save_settings(&self) {
        let Some(path) = &self.settings_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                log::warn!("failed to create settings directory: {error}");
                return;
            }
        }
        match toml::to_string_pretty(&self.settings) {
            Ok(contents) => {
                if let Err(error) = fs::write(path, contents) {
                    log::warn!("failed to write settings: {error}");
                }
            }
            Err(error) => log::warn!("failed to serialize settings: {error}"),
        }
    }

    fn draw_start_screen(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        // Constrain the hero to a comfortable reading width so it doesn't sprawl on
        // wide windows. Everything is laid out inside a centered column ~420 px wide.
        let column_width: f32 = 420.0;
        ui.vertical_centered(|ui| {
            ui.set_max_width(column_width);
            ui.add_space(96.0);

            // Logo + wordmark in one row. `vertical_centered` already centers each
            // child horizontally within the column, so the row's natural width
            // (logo + gap + text) ends up centered without any manual offset math.
            ui.horizontal(|ui| {
                draw_lattice_mark(ui, &theme, 20.0);
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Lattice")
                        .size(22.0)
                        .family(lattice_ui::fonts::bold_family())
                        .color(theme.text),
                );
            });

            ui.add_space(28.0);

            let opened = ui
                .add_sized(
                    egui::vec2(200.0, 34.0),
                    egui::Button::new(
                        egui::RichText::new("Open Folder")
                            .size(13.0)
                            .family(lattice_ui::fonts::bold_family())
                            .color(theme.accent_fg),
                    )
                    .fill(theme.accent)
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(egui::CornerRadius::same(6)),
                )
                .clicked();
            if opened {
                self.open_folder_dialog();
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Ctrl+O")
                    .monospace()
                    .size(10.5)
                    .color(theme.text_faint),
            );

            if !self.settings.recent_vaults.is_empty() {
                ui.add_space(40.0);
                ui.label(
                    egui::RichText::new("RECENT")
                        .monospace()
                        .size(10.0)
                        .color(theme.text_faint),
                );
                ui.add_space(6.0);
                // Take a snapshot to avoid borrowing `self.settings` while we mutate
                // `self` inside `open_path`.
                let recents: Vec<_> = self.settings.recent_vaults.clone();
                for path in recents {
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Folder")
                        .to_owned();
                    let parent_str = path
                        .parent()
                        .and_then(|parent| parent.to_str())
                        .unwrap_or("")
                        .to_owned();
                    if recent_vault_row(ui, &theme, &name, &parent_str).clicked() {
                        self.open_path(path);
                    }
                }
            }
        });
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        egui::Frame::new()
            .fill(theme.bg_elev)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.sidebar_view == SidebarView::Files, "Files")
                        .on_hover_text("Explorer")
                        .clicked()
                    {
                        self.sidebar_view = SidebarView::Files;
                    }
                    if ui
                        .selectable_label(self.sidebar_view == SidebarView::Search, "Search")
                        .on_hover_text("Search in files (Ctrl+Shift+F)")
                        .clicked()
                    {
                        self.open_workspace_search(false);
                    }
                });
            });
        match self.sidebar_view {
            SidebarView::Files => self.draw_files_sidebar(ui),
            SidebarView::Search => self.draw_workspace_search_sidebar(ui),
        }
    }

    fn draw_files_sidebar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        egui::Frame::new()
            .fill(theme.bg)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let title = self
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.vault().name.as_str())
                        .unwrap_or("Files");
                    ui.label(
                        egui::RichText::new(title.to_ascii_uppercase())
                            .size(10.5)
                            .strong()
                            .color(theme.text_faint),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, &theme, "↻", "Refresh").clicked() {
                            self.refresh_workspace_data();
                        }
                        // `+` toggles the inline new-file input. Always reset the
                        // suggestion so the user sees a fresh starting point each
                        // time, and request focus on the next frame.
                        if icon_button(ui, &theme, "+", "New file").clicked() {
                            if self.creating_new_file {
                                self.creating_new_file = false;
                            } else {
                                self.start_new_file();
                            }
                        }
                    });
                });
            });
        if self.creating_new_file {
            ui.horizontal(|ui| {
                let id = ui.make_persistent_id("sidebar_new_file_input");
                let input_width = (ui.available_width() - 8.0).max(48.0);
                let response = ui.add_sized(
                    egui::vec2(input_width, ui.spacing().interact_size.y),
                    egui::TextEdit::singleline(&mut self.new_file_path)
                        .id(id)
                        .hint_text("path/to/file.ext"),
                );
                if self.new_file_focus_pending {
                    response.request_focus();
                    self.new_file_focus_pending = false;
                }
                let enter_pressed = response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let escape_pressed = ui.input(|input| input.key_pressed(egui::Key::Escape));
                if enter_pressed && !self.new_file_path.trim().is_empty() {
                    self.create_file();
                    self.creating_new_file = false;
                } else if escape_pressed || (enter_pressed && self.new_file_path.trim().is_empty())
                {
                    self.creating_new_file = false;
                }
            });
        }
        let mut actions = Vec::new();
        let active_path = self.active_tab_path().cloned();
        let ctx = TreeRenderCtx {
            theme: &theme,
            selected_path: &self.selected_path,
            active_path: active_path.as_ref(),
            expanded_paths: &self.expanded_paths,
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            for node in &self.tree {
                draw_tree_node(ui, &ctx, node, 0, &mut actions);
            }
        });
        for action in actions {
            self.apply_tree_action(action);
        }
    }

    fn draw_workspace_search_sidebar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let mut changed = false;
        egui::Frame::new()
            .fill(theme.bg)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("SEARCH")
                            .size(10.5)
                            .strong()
                            .color(theme.text_faint),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Filters").size(10.5).color(
                                        if self.workspace_search.show_filters {
                                            theme.accent
                                        } else {
                                            theme.text_dim
                                        },
                                    ),
                                )
                                .selected(self.workspace_search.show_filters)
                                .frame(false),
                            )
                            .on_hover_text("Toggle include and exclude filters")
                            .clicked()
                        {
                            self.workspace_search.show_filters =
                                !self.workspace_search.show_filters;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Replace").size(10.5).color(
                                        if self.workspace_search.show_replace {
                                            theme.accent
                                        } else {
                                            theme.text_dim
                                        },
                                    ),
                                )
                                .selected(self.workspace_search.show_replace)
                                .frame(false),
                            )
                            .on_hover_text("Toggle replace controls")
                            .clicked()
                        {
                            self.workspace_search.show_replace =
                                !self.workspace_search.show_replace;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let toggle_width = 28.0;
                    let input_width =
                        (ui.available_width() - toggle_width * 3.0 - 12.0).max(80.0);
                    let id = ui.make_persistent_id("workspace_search_query");
                    let response = ui.add_sized(
                        egui::vec2(input_width, 26.0),
                        egui::TextEdit::singleline(&mut self.workspace_search.query)
                            .id(id)
                            .hint_text("Search workspace"),
                    );
                    if self.workspace_search.focus_pending {
                        response.request_focus();
                        self.workspace_search.focus_pending = false;
                    }
                    changed |= response.changed();
                    if search_toggle(
                        ui,
                        &theme,
                        self.workspace_search.case_sensitive,
                        "Aa",
                        "Match case",
                    ) {
                        self.workspace_search.case_sensitive =
                            !self.workspace_search.case_sensitive;
                        changed = true;
                    }
                    if search_toggle(
                        ui,
                        &theme,
                        self.workspace_search.whole_word,
                        "W",
                        "Match whole word",
                    ) {
                        self.workspace_search.whole_word = !self.workspace_search.whole_word;
                        changed = true;
                    }
                    if search_toggle(
                        ui,
                        &theme,
                        self.workspace_search.use_regex,
                        ".*",
                        "Use regular expression",
                    ) {
                        self.workspace_search.use_regex = !self.workspace_search.use_regex;
                        changed = true;
                    }
                });
                if self.workspace_search.show_replace {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let button_width = 76.0;
                        let input_width = (ui.available_width() - button_width - 4.0).max(80.0);
                        ui.add_sized(
                            egui::vec2(input_width, 26.0),
                            egui::TextEdit::singleline(&mut self.workspace_search.replacement)
                                .hint_text("Replace with"),
                        );
                        let enabled = self
                            .workspace_search
                            .result
                            .as_ref()
                            .is_some_and(|result| !result.matches.is_empty())
                            && !self.workspace_search.replace_pending;
                        if ui
                            .add_enabled(
                                enabled,
                                egui::Button::new("Replace All")
                                    .min_size(egui::vec2(button_width, 26.0)),
                            )
                            .on_hover_text("Replace all matches")
                            .clicked()
                        {
                            self.workspace_search.replace_confirmation = true;
                        }
                    });
                }
                if self.workspace_search.show_filters {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("FILES TO INCLUDE")
                            .size(9.5)
                            .color(theme.text_faint),
                    );
                    changed |= ui
                        .add_sized(
                            egui::vec2(ui.available_width(), 24.0),
                            egui::TextEdit::singleline(&mut self.workspace_search.include_globs)
                                .hint_text("e.g. *.rs, src/**"),
                        )
                        .changed();
                    ui.label(
                        egui::RichText::new("FILES TO EXCLUDE")
                            .size(9.5)
                            .color(theme.text_faint),
                    );
                    changed |= ui
                        .add_sized(
                            egui::vec2(ui.available_width(), 24.0),
                            egui::TextEdit::singleline(&mut self.workspace_search.exclude_globs)
                                .hint_text("e.g. tests/**, *.lock"),
                        )
                        .changed();
                }
            });

        if changed {
            self.schedule_workspace_search();
        }

        if self.workspace_search.replace_confirmation {
            let (matches, files, truncated) = self
                .workspace_search
                .result
                .as_ref()
                .map(|result| {
                    (
                        result.matches.len(),
                        result.files_with_matches,
                        result.truncated,
                    )
                })
                .unwrap_or_default();
            egui::Frame::new()
                .fill(theme.bg_elev_2)
                .stroke(egui::Stroke::new(1.0, theme.warn))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    let message = if truncated {
                        format!(
                            "Replace every match in the workspace? The list shows only the \
                             first {matches} matches across {files} files; replacement is \
                             not limited to those."
                        )
                    } else {
                        format!("Replace {matches} matches across {files} files?")
                    };
                    ui.label(message);
                    ui.horizontal(|ui| {
                        if ui.button("Replace All").clicked() {
                            self.replace_workspace_matches(None);
                        }
                        if ui.button("Cancel").clicked() {
                            self.workspace_search.replace_confirmation = false;
                        }
                    });
                });
        }

        if self.workspace_search.pending || self.workspace_search.replace_pending {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(13.0));
                ui.label(
                    egui::RichText::new(if self.workspace_search.replace_pending {
                        "Replacing..."
                    } else {
                        "Searching..."
                    })
                    .size(11.0)
                    .color(theme.text_dim),
                );
            });
        }
        if let Some(error) = &self.workspace_search.error {
            ui.colored_label(theme.danger, error);
        }

        let mut open_match = None;
        let mut toggle_file = None;
        let mut replace_file = None;
        if let Some(result) = &self.workspace_search.result {
            let suffix = if result.truncated { " (limited)" } else { "" };
            ui.label(
                egui::RichText::new(format!(
                    "{} matches in {} files{suffix}  ·  {} ms",
                    result.matches.len(),
                    result.files_with_matches,
                    result.elapsed_ms
                ))
                .size(10.5)
                .color(theme.text_faint),
            );
            let matches = &result.matches;
            let mut rows = Vec::with_capacity(matches.len() + result.files_with_matches);
            let mut start = 0;
            while start < matches.len() {
                let path = &matches[start].path;
                let mut end = start + 1;
                while end < matches.len() && &matches[end].path == path {
                    end += 1;
                }
                rows.push(SearchDisplayRow::File { start, end });
                if self.workspace_search.expanded_files.contains(path) {
                    rows.extend((start..end).map(SearchDisplayRow::Match));
                }
                start = end;
            }

            egui::ScrollArea::vertical().show_rows(ui, 26.0, rows.len(), |ui, visible| {
                for row in &rows[visible] {
                    match *row {
                        SearchDisplayRow::File { start, end } => {
                            let path = matches[start].path.clone();
                            let expanded = self.workspace_search.expanded_files.contains(&path);
                            let file_row = search_file_row(
                                ui,
                                &theme,
                                &path,
                                end - start,
                                expanded,
                                self.workspace_search.show_replace,
                            );
                            if file_row.toggle {
                                toggle_file = Some(path.clone());
                            }
                            if file_row.replace {
                                replace_file = Some(path);
                            }
                        }
                        SearchDisplayRow::Match(index) => {
                            let response = search_match_row(ui, &theme, &matches[index]);
                            if response.clicked() {
                                open_match = Some(matches[index].clone());
                            }
                        }
                    }
                }
            });
        } else if !self.workspace_search.query.is_empty() && !self.workspace_search.pending {
            ui.label(egui::RichText::new("No results.").color(theme.text_dim));
        }

        if let Some(path) = toggle_file {
            if !self.workspace_search.expanded_files.remove(&path) {
                self.workspace_search.expanded_files.insert(path);
            }
        }
        if let Some(path) = replace_file {
            self.replace_workspace_matches(Some(BTreeSet::from([path])));
        }
        if let Some(matched) = open_match {
            self.open_search_match(matched);
        }
    }

    fn apply_tree_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::Select(path) => {
                let is_directory = find_tree_node(&self.tree, &path).is_some_and(|node| {
                    matches!(
                        node.kind,
                        TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded
                    )
                });
                self.selected_path = Some(path.clone());
                self.rename_target = path.as_str().to_owned();
                if !is_directory {
                    self.open_editor_file(path);
                }
            }
            TreeAction::Toggle(path) => self.toggle_directory(&path),
        }
    }

    fn active_tab_path(&self) -> Option<&VaultPath> {
        self.active_tab
            .and_then(|index| self.tabs.get(index))
            .and_then(EditorTab::path)
    }

    fn close_tab(&mut self, index: usize) {
        if self
            .tabs
            .get(index)
            .is_some_and(|tab| tab.buffer.dirty || tab.conflict.is_some())
        {
            self.status = "Save, reload, or resolve conflict before closing".to_owned();
            return;
        }
        self.tabs.remove(index);
        self.active_tab = adjusted_active_tab(self.active_tab, index, self.tabs.len());
        self.refresh_editor_find_matches();
    }
}

#[derive(Debug)]
enum TreeAction {
    Select(VaultPath),
    Toggle(VaultPath),
}

#[derive(Debug)]
enum EditorTabAction {
    Select(usize),
    Close(usize),
}

#[derive(Clone, Copy)]
enum SearchDisplayRow {
    File { start: usize, end: usize },
    Match(usize),
}

struct TreeRenderCtx<'a> {
    theme: &'a Theme,
    selected_path: &'a Option<VaultPath>,
    active_path: Option<&'a VaultPath>,
    expanded_paths: &'a BTreeSet<VaultPath>,
}

fn draw_tree_node(
    ui: &mut egui::Ui,
    ctx: &TreeRenderCtx<'_>,
    node: &TreeNode,
    depth: usize,
    actions: &mut Vec<TreeAction>,
) {
    let theme = ctx.theme;
    let is_directory = matches!(
        node.kind,
        TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded
    );
    let is_expanded = ctx.expanded_paths.contains(&node.path);
    let selected = ctx.selected_path.as_ref() == Some(&node.path);
    let active = ctx.active_path == Some(&node.path);
    let mut toggle = false;
    let mut select = false;

    let row_fill = if active {
        theme.accent_soft
    } else if selected {
        theme.bg_selected
    } else {
        egui::Color32::TRANSPARENT
    };

    let row_response = egui::Frame::new()
        .fill(row_fill)
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 0))
        .show(ui, |ui| {
            ui.set_min_height(ROW_HEIGHT);
            ui.horizontal(|ui| {
                ui.add_space((depth as f32) * TREE_INDENT_WIDTH);
                if is_directory {
                    let chevron = if is_expanded { "⌄" } else { "›" };
                    if ui
                        .add_sized(
                            egui::vec2(12.0, 18.0),
                            egui::Button::new(chevron).frame(false),
                        )
                        .clicked()
                    {
                        toggle = true;
                    }
                } else {
                    ui.add_space(18.0);
                }

                let icon_response = tree_icon_ui(ui, node, is_expanded);
                if icon_response.clicked() {
                    select = true;
                }
                if icon_response.double_clicked() && is_directory {
                    toggle = true;
                }

                let name_color = if active || selected || is_directory {
                    theme.text
                } else {
                    theme.text_dim
                };
                let response = ui.add(
                    egui::Label::new(
                        egui::RichText::new(&node.name)
                            .size(13.0)
                            .color(name_color)
                            .strong(),
                    )
                    .truncate()
                    .sense(egui::Sense::click()),
                );
                if response.clicked() {
                    select = true;
                }
                if response.double_clicked() && is_directory {
                    toggle = true;
                }
                if let Some(warning) = &node.warning {
                    response.clone().on_hover_text(warning);
                    ui.colored_label(theme.warn, "!").on_hover_text(warning);
                }
            })
        })
        .response;
    if row_response.double_clicked() && is_directory {
        toggle = true;
    }

    if select {
        actions.push(TreeAction::Select(node.path.clone()));
    }
    if toggle {
        actions.push(TreeAction::Toggle(node.path.clone()));
    }
    if let TreeNodeKind::DirectoryLoaded { children } = &node.kind {
        if is_expanded {
            for child in children {
                draw_tree_node(ui, ctx, child, depth + 1, actions);
            }
        }
    }
}

impl LatticeApp {
    fn toggle_directory(&mut self, path: &VaultPath) {
        if self.expanded_paths.contains(path) {
            self.expanded_paths.remove(path);
            return;
        }
        self.status = format!("Loading {}", path.as_str());
        self.enqueue_tree_load(Some(path.clone()));
    }

    fn watch_tree_path(&mut self, path: &VaultPath) {
        let (Some(workspace), Some(watcher)) = (&self.workspace, &mut self.watcher) else {
            return;
        };
        let absolute = path.join_to(workspace.vault().root.as_path());
        if let Err(error) = watcher.watch_path(&absolute) {
            log::warn!("failed to watch expanded path {}: {error}", path.as_str());
        }
    }

    fn draw_main_area(&mut self, ui: &mut egui::Ui) {
        self.draw_editor(ui);
    }

    fn draw_editor_find(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let mut refresh = false;
        let mut navigate = None;
        let mut replace_current = false;
        let mut replace_all = false;
        egui::Frame::new()
            .fill(theme.bg_elev)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .inner_margin(egui::Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.editor_find.query)
                            .id(ui.make_persistent_id("editor_find_query"))
                            .hint_text("Find")
                            .desired_width(240.0),
                    );
                    if self.editor_find.focus_pending {
                        response.request_focus();
                        self.editor_find.focus_pending = false;
                    }
                    refresh |= response.changed();
                    if response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        navigate = Some(if ui.input(|input| input.modifiers.shift) {
                            -1
                        } else {
                            1
                        });
                    }
                    let count = match (self.editor_find.current, self.editor_find.matches.len()) {
                        (_, 0) => "No results".to_owned(),
                        (Some(current), total) => format!("{} of {total}", current + 1),
                        (None, total) => format!("{total} results"),
                    };
                    ui.label(
                        egui::RichText::new(count)
                            .size(10.5)
                            .color(theme.text_faint),
                    );
                    if search_toggle(
                        ui,
                        theme,
                        self.editor_find.case_sensitive,
                        "Aa",
                        "Match case",
                    ) {
                        self.editor_find.case_sensitive = !self.editor_find.case_sensitive;
                        refresh = true;
                    }
                    if search_toggle(
                        ui,
                        theme,
                        self.editor_find.whole_word,
                        "W",
                        "Match whole word",
                    ) {
                        self.editor_find.whole_word = !self.editor_find.whole_word;
                        refresh = true;
                    }
                    if search_toggle(
                        ui,
                        theme,
                        self.editor_find.use_regex,
                        ".*",
                        "Use regular expression",
                    ) {
                        self.editor_find.use_regex = !self.editor_find.use_regex;
                        refresh = true;
                    }
                    if icon_button(ui, theme, "↑", "Previous match (Shift+Enter)").clicked() {
                        navigate = Some(-1);
                    }
                    if icon_button(ui, theme, "↓", "Next match (Enter/F3)").clicked() {
                        navigate = Some(1);
                    }
                    if icon_button(ui, theme, "↔", "Toggle replace").clicked() {
                        self.editor_find.show_replace = !self.editor_find.show_replace;
                    }
                    if icon_button(ui, theme, "×", "Close").clicked() {
                        self.editor_find.visible = false;
                    }
                });
                if self.editor_find.show_replace {
                    ui.horizontal(|ui| {
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.editor_find.replacement)
                                .hint_text("Replace")
                                .desired_width(240.0),
                        );
                        if response.has_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        {
                            replace_current = true;
                        }
                        if ui.button("Replace").clicked() {
                            replace_current = true;
                        }
                        if ui.button("Replace All").clicked() {
                            replace_all = true;
                        }
                    });
                }
                if let Some(error) = &self.editor_find.error {
                    ui.colored_label(theme.danger, error);
                }
            });
        if refresh {
            self.refresh_editor_find_matches();
        }
        if let Some(delta) = navigate {
            self.navigate_editor_find(delta);
        }
        if replace_current {
            self.replace_current_editor_match();
        }
        if replace_all {
            self.replace_all_editor_matches();
        }
    }

    fn draw_editor(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        if self.tabs.is_empty() {
            ui.with_layout(
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    ui.label(
                        egui::RichText::new("No file open. Ctrl+P to find one.")
                            .color(theme.text_faint),
                    );
                },
            );
            return;
        }

        self.draw_tab_bar(ui, &theme);

        if self.editor_find.visible {
            self.draw_editor_find(ui, &theme);
        }

        let Some(index) = self.active_tab else {
            return;
        };
        if self.tabs.get(index).is_none() {
            return;
        }

        if self.tabs[index].large_file_warning {
            ui.colored_label(theme.warn, "Large file: editing may be slow.");
        }
        if let Some(conflict) = self.tabs[index].conflict {
            self.draw_conflict_bar(ui, &theme, conflict);
        }

        let language = self.tabs[index]
            .path()
            .map(language_for_path)
            .unwrap_or("txt")
            .to_owned();
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let code_theme = egui_extras::syntax_highlighting::CodeTheme::from_style(ui.style());

        let editor_id = self.editor_text_id(index);
        let tab = &mut self.tabs[index];
        let line_count = tab
            .buffer
            .text
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        if tab.gutter_lines != line_count {
            rebuild_gutter(&mut tab.gutter, line_count);
            tab.gutter_lines = line_count;
        }
        let highlight_enabled = tab.buffer.text.len() <= HIGHLIGHT_MAX_BYTES;

        let mut changed = false;
        let mut cursor_index = None;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.horizontal_top(|ui| {
                    // Line-number gutter: one monospace galley, same font as the
                    // editor. Rows align because wrapping is off (infinite width).
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.add_space(2.0); // mirror the TextEdit vertical margin
                        ui.label(
                            egui::RichText::new(tab.gutter.as_str())
                                .font(font_id.clone())
                                .color(theme.text_faint),
                        );
                    });
                    ui.add_space(12.0);

                    let mut layouter =
                        |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                            let mut job = egui_extras::syntax_highlighting::highlight(
                                ui.ctx(),
                                ui.style(),
                                &code_theme,
                                buf.as_str(),
                                &language,
                            );
                            job.wrap.max_width = wrap_width;
                            ui.fonts_mut(|fonts| fonts.layout_job(job))
                        };

                    let mut edit = egui::TextEdit::multiline(&mut tab.buffer.text)
                        .id(editor_id)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(40)
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::symmetric(0, 2));
                    if highlight_enabled {
                        edit = edit.layouter(&mut layouter);
                    }
                    let mut output = edit.show(ui);
                    if output.response.changed() {
                        changed = true;
                    }
                    cursor_index = output.cursor_range.map(|range| range.primary.index);
                    if let Some((byte_start, byte_end)) = tab.pending_selection.take() {
                        let start = byte_to_char_index(&tab.buffer.text, byte_start);
                        let end = byte_to_char_index(&tab.buffer.text, byte_end);
                        let range = egui::text::CCursorRange::two(
                            egui::text::CCursor::new(start),
                            egui::text::CCursor::new(end),
                        );
                        output.state.cursor.set_char_range(Some(range));
                        output.state.store(ui.ctx(), output.response.id);
                        output.response.request_focus();
                        let cursor_rect = output
                            .galley
                            .pos_from_cursor(egui::text::CCursor::new(end))
                            .translate(output.galley_pos.to_vec2());
                        ui.scroll_to_rect(cursor_rect.expand(24.0), Some(egui::Align::Center));
                        cursor_index = Some(end);
                    }
                });
            });

        let tab = &mut self.tabs[index];
        if changed {
            tab.buffer.dirty = true;
            tab.last_edit = Some(Instant::now());
        }
        self.cursor_line_col = cursor_index.map(|char_index| line_col_at(&tab.buffer.text, char_index));
        if changed {
            self.refresh_editor_find_matches();
            if self.sidebar_view == SidebarView::Search
                && !self.workspace_search.query.trim().is_empty()
            {
                self.schedule_workspace_search();
            }
        }
    }

    fn draw_tab_bar(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let mut tab_action: Option<EditorTabAction> = None;
        let mut active_rect = None;
        let bar = egui::Frame::new()
            .fill(theme.bg)
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                ui.set_height(32.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (index, tab) in self.tabs.iter().enumerate() {
                        let selected = self.active_tab == Some(index);
                        let name = tab.display_name();
                        let dirty = tab.buffer.dirty;
                        let inner = egui::Frame::new()
                            .fill(if selected {
                                theme.bg
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .inner_margin(egui::Margin {
                                left: 12,
                                right: 10,
                                top: 0,
                                bottom: 0,
                            })
                            .show(ui, |ui| {
                                ui.set_height(32.0);
                                ui.horizontal_centered(|ui| {
                                    ui.spacing_mut().item_spacing.x = 7.0;
                                    let color = if selected { theme.text } else { theme.text_dim };
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                egui::RichText::new(&name).size(12.5).color(color),
                                            )
                                            .selectable(false)
                                            .sense(egui::Sense::click()),
                                        )
                                        .clicked()
                                    {
                                        tab_action = Some(EditorTabAction::Select(index));
                                    }
                                    if dirty {
                                        let (r, _) = ui.allocate_exact_size(
                                            egui::vec2(6.0, 6.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().circle_filled(r.center(), 3.0, theme.text);
                                    } else if icon_button(ui, theme, "×", "Close tab").clicked() {
                                        tab_action = Some(EditorTabAction::Close(index));
                                    }
                                });
                            });
                        let rect = inner.response.rect;
                        if inner.response.interact(egui::Sense::click()).clicked() {
                            tab_action = Some(EditorTabAction::Select(index));
                        }
                        ui.painter().vline(
                            rect.right(),
                            rect.y_range(),
                            egui::Stroke::new(1.0, theme.border),
                        );
                        if selected {
                            active_rect = Some(rect);
                        }
                    }
                });
            });
        let bar_rect = bar.response.rect;
        ui.painter().hline(
            bar_rect.x_range(),
            bar_rect.bottom() - 0.5,
            egui::Stroke::new(1.0, theme.border),
        );
        if let Some(rect) = active_rect {
            ui.painter().hline(
                rect.x_range(),
                bar_rect.bottom() - 0.75,
                egui::Stroke::new(1.5, theme.accent),
            );
        }

        if let Some(action) = tab_action {
            match action {
                EditorTabAction::Select(index) => {
                    self.active_tab = Some(index);
                    if let Some(path) = self.tabs.get(index).and_then(EditorTab::path) {
                        self.selected_path = Some(path.clone());
                        self.rename_target = path.as_str().to_owned();
                    }
                    self.refresh_editor_find_matches();
                }
                EditorTabAction::Close(index) => self.close_tab(index),
            }
        }
    }

    fn draw_conflict_bar(&mut self, ui: &mut egui::Ui, theme: &Theme, conflict: FileConflict) {
        let w = theme.warn;
        let mut reload = false;
        let mut overwrite = false;
        egui::Frame::new()
            .fill(egui::Color32::from_rgba_unmultiplied(w.r(), w.g(), w.b(), 26))
            .stroke(egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(w.r(), w.g(), w.b(), 70),
            ))
            .inner_margin(egui::Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme.warn, "!");
                    ui.label(
                        egui::RichText::new(conflict.message())
                            .size(12.0)
                            .color(theme.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if quiet_button(ui, theme, "Overwrite disk").clicked() {
                            overwrite = true;
                        }
                        if matches!(conflict, FileConflict::ModifiedOnDisk)
                            && quiet_button(ui, theme, "Reload").clicked()
                        {
                            reload = true;
                        }
                    });
                });
            });
        if overwrite {
            self.overwrite_active_tab();
        }
        if reload {
            self.reload_active_tab();
        }
    }

    fn close_active_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        self.close_tab(index);
    }

    fn draw_status_bar(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let info = self.active_tab.and_then(|index| self.tabs.get(index)).map(|tab| {
            let path = tab
                .path()
                .map(|path| path.as_str().to_owned())
                .unwrap_or_else(|| "untitled".to_owned());
            let lines = tab.buffer.text.bytes().filter(|byte| *byte == b'\n').count() + 1;
            let language = tab.path().map(language_for_path).unwrap_or("txt").to_owned();
            (path, tab.buffer.dirty, lines, language)
        });
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.add_space(2.0);
            match &info {
                Some((path, dirty, _, _)) => {
                    status_seg(ui, path, theme.text_dim);
                    if *dirty {
                        status_seg(ui, "● modified", theme.accent);
                    } else {
                        status_seg(ui, "saved", theme.text_faint);
                    }
                }
                None => status_seg(ui, "no file", theme.text_faint),
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let sidebar = self.show_sidebar;
                if status_toggle(ui, theme, sidebar, "sidebar").clicked() {
                    self.show_sidebar = !self.show_sidebar;
                }
                if let Some((_, _, lines, language)) = &info {
                    status_seg(ui, language, theme.text_faint);
                    status_seg(ui, &format!("{lines} lines"), theme.text_faint);
                    if let Some((line, col)) = self.cursor_line_col {
                        status_seg(ui, &format!("Ln {line}, Col {col}"), theme.text_dim);
                    }
                }
            });
        });
    }

    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            draw_lattice_mark(ui, &theme, 13.0);
            ui.label(
                egui::RichText::new("Lattice")
                    .size(12.0)
                    .strong()
                    .color(theme.text),
            );
            ui.add_space(4.0);
            ui.menu_button("File", |ui| {
                if menu_item(ui, "New File", "Ctrl+N").clicked() {
                    self.start_new_file();
                    ui.close();
                }
                if menu_item(ui, "Open Folder...", "Ctrl+O").clicked() {
                    self.open_folder_dialog();
                    ui.close();
                }
                ui.separator();
                if menu_item(ui, "Save", "Ctrl+S").clicked() {
                    self.save_active_tab();
                    ui.close();
                }
                if menu_item(ui, "Reload from Disk", "").clicked() {
                    self.reload_active_tab();
                    ui.close();
                }
                if menu_item(ui, "Close Tab", "Ctrl+W").clicked() {
                    self.close_active_tab();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if menu_item(ui, "Find", "Ctrl+F").clicked() {
                    self.open_editor_find(false);
                    ui.close();
                }
                if menu_item(ui, "Replace", "Ctrl+H").clicked() {
                    self.open_editor_find(true);
                    ui.close();
                }
                if menu_item(ui, "Find in Files", "Ctrl+Shift+F").clicked() {
                    self.open_workspace_search(false);
                    ui.close();
                }
                if menu_item(ui, "Replace in Files", "Ctrl+Shift+H").clicked() {
                    self.open_workspace_search(true);
                    ui.close();
                }
                ui.separator();
                if menu_item(ui, "Rename/Move Selected", "").clicked() {
                    self.rename_selected();
                    ui.close();
                }
                if menu_item(ui, "Delete Selected", "").clicked() {
                    self.delete_selected();
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                if checked_menu_item(ui, self.show_sidebar, "Toggle Sidebar", "Ctrl+B").clicked() {
                    self.show_sidebar = !self.show_sidebar;
                    ui.close();
                }
                ui.separator();
                if menu_item(ui, "Quick Open...", "Ctrl+P").clicked() {
                    self.quick_open_overlay = true;
                    ui.close();
                }
                if menu_item(ui, "Refresh", "Ctrl+R").clicked() {
                    self.refresh_workspace_data();
                    ui.close();
                }
            });
        });
    }

    fn select_relative_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let current = self.active_tab.unwrap_or(0) as isize;
        let len = self.tabs.len() as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.active_tab = Some(next);
        if let Some(path) = self.tabs.get(next).and_then(EditorTab::path) {
            self.selected_path = Some(path.clone());
            self.rename_target = path.as_str().to_owned();
        }
        self.refresh_editor_find_matches();
    }

    fn draw_quick_open_overlay(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        self.ensure_quick_open_index();
        let query = self.quick_query.trim().to_owned();
        let matches = if self.quick_open_ready && !query.is_empty() {
            self.quick_open.search(&query, 30)
        } else {
            Vec::new()
        };

        egui::Area::new(egui::Id::new("quick_open_overlay"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 82.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme.bg_elev)
                    .stroke(egui::Stroke::new(1.0, theme.border_strong))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(540.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("⌕").size(15.0).color(theme.text_faint));
                            let id = ui.make_persistent_id("quick_open_input");
                            ui.memory_mut(|memory| memory.request_focus(id));
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.quick_query)
                                    .id(id)
                                    .hint_text("Type a file name or path...")
                                    .desired_width(f32::INFINITY),
                            );
                            if response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Escape))
                            {
                                self.quick_open_overlay = false;
                            }
                        });
                        ui.separator();
                        if self.quick_open_pending {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label(
                                    egui::RichText::new("Indexing files...").color(theme.text_dim),
                                );
                            });
                        } else if !query.is_empty() && matches.is_empty() {
                            ui.label(egui::RichText::new("No matches.").color(theme.text_dim));
                        } else if !query.is_empty() {
                            let mut picked = None;
                            egui::ScrollArea::vertical()
                                .max_height(360.0)
                                .show(ui, |ui| {
                                    for (index, item) in matches.iter().enumerate() {
                                        let name = item
                                            .path
                                            .as_path()
                                            .file_name()
                                            .unwrap_or(item.path.as_str());
                                        let dir = item
                                            .path
                                            .as_path()
                                            .parent()
                                            .map(|path| path.as_str())
                                            .unwrap_or("");
                                        let fill = if index == 0 {
                                            theme.accent_soft
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        };
                                        let clicked = egui::Frame::new()
                                            .fill(fill)
                                            .corner_radius(egui::CornerRadius::same(5))
                                            .inner_margin(egui::Margin::symmetric(10, 6))
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(name)
                                                            .strong()
                                                            .color(theme.text),
                                                    );
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.label(
                                                                egui::RichText::new(dir)
                                                                    .monospace()
                                                                    .size(10.5)
                                                                    .color(theme.text_faint),
                                                            );
                                                        },
                                                    );
                                                });
                                            })
                                            .response
                                            .interact(egui::Sense::click())
                                            .clicked();
                                        if clicked {
                                            picked = Some(item.path.clone());
                                        }
                                    }
                                });
                            if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                                if let Some(item) = matches.first() {
                                    picked = Some(item.path.clone());
                                }
                            }
                            if let Some(path) = picked {
                                self.quick_open_overlay = false;
                                self.quick_query.clear();
                                self.selected_path = Some(path.clone());
                                self.rename_target = path.as_str().to_owned();
                                self.open_editor_file(path);
                            }
                        }
                    });
            });
    }
}

fn replace_tree_children(
    nodes: &mut [TreeNode],
    path: &VaultPath,
    children: Vec<TreeNode>,
) -> bool {
    let mut pending_children = Some(children);
    replace_tree_children_inner(nodes, path, &mut pending_children)
}

fn replace_tree_children_inner(
    nodes: &mut [TreeNode],
    path: &VaultPath,
    pending_children: &mut Option<Vec<TreeNode>>,
) -> bool {
    for node in nodes {
        if &node.path == path {
            let Some(children) = pending_children.take() else {
                return false;
            };
            node.kind = TreeNodeKind::DirectoryLoaded { children };
            return true;
        }
        if let TreeNodeKind::DirectoryLoaded { children } = &mut node.kind {
            if replace_tree_children_inner(children, path, pending_children) {
                return true;
            }
        }
    }
    false
}

fn find_tree_node<'a>(nodes: &'a [TreeNode], path: &VaultPath) -> Option<&'a TreeNode> {
    let mut stack: Vec<_> = nodes.iter().collect();
    while let Some(node) = stack.pop() {
        if &node.path == path {
            return Some(node);
        }
        if let TreeNodeKind::DirectoryLoaded { children } = &node.kind {
            stack.extend(children.iter());
        }
    }
    None
}

fn tree_icon_ui(ui: &mut egui::Ui, node: &TreeNode, is_expanded: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(TREE_ICON_SIZE, TREE_ICON_SIZE),
        egui::Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(82));
    match &node.kind {
        TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded => {
            let fill = if is_expanded {
                egui::Color32::from_rgb(236, 184, 69)
            } else {
                egui::Color32::from_rgb(218, 161, 56)
            };
            let tab = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 1.5, rect.top() + 3.0),
                egui::vec2(6.0, 4.5),
            );
            let body = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.5, rect.top() + 5.5),
                egui::pos2(rect.right() - 1.0, rect.bottom() - 2.0),
            );
            painter.rect_filled(tab, 2.0, fill.gamma_multiply(0.9));
            painter.rect_filled(body, 2.0, fill);
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
        }
        TreeNodeKind::File => {
            let fill = egui::Color32::from_rgb(214, 219, 226);
            let accent = file_icon_accent(node);
            let page = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 3.0, rect.top() + 1.5),
                egui::pos2(rect.right() - 2.0, rect.bottom() - 1.5),
            );
            painter.rect_filled(page, 2.0, fill);
            painter.rect_stroke(page, 2.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    egui::pos2(page.right() - 4.0, page.top()),
                    egui::pos2(page.right(), page.top() + 4.0),
                ],
                stroke,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(page.left() + 2.0, page.bottom() - 4.0),
                    egui::pos2(page.right() - 2.0, page.bottom() - 2.0),
                ),
                1.0,
                accent,
            );
        }
    }
    response
}

fn draw_lattice_mark(ui: &mut egui::Ui, theme: &Theme, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.5, theme.accent);
    painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        stroke,
    );
}

fn status_seg(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(egui::RichText::new(text).monospace().size(10.5).color(color));
}

fn status_toggle(ui: &mut egui::Ui, theme: &Theme, on: bool, label: &str) -> egui::Response {
    let glyph = if on { "●" } else { "○" };
    ui.add(
        egui::Button::new(
            egui::RichText::new(format!("{glyph} {label}"))
                .monospace()
                .size(10.5)
                .color(theme.text_faint),
        )
        .fill(egui::Color32::TRANSPARENT)
        .frame(false),
    )
}

/// A single row in the start-screen "RECENT" list. Hover-fills, click-opens, with the
/// vault name on the left and the parent path muted on the right. Returns the row's
/// response so the caller can react to clicks.
fn recent_vault_row(
    ui: &mut egui::Ui,
    theme: &Theme,
    name: &str,
    parent_path: &str,
) -> egui::Response {
    let response = ui
        .scope(|ui| {
            // Reserve the row first so we can paint a hover background under the text.
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(5),
                    theme.bg_hover,
                );
            }
            // Inset the text by a few px so it doesn't kiss the row edge on hover.
            let inset = rect.shrink2(egui::vec2(10.0, 0.0));
            let mut text_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(inset)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            text_ui.label(egui::RichText::new(name).size(13.0).color(theme.text));
            text_ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(parent_path)
                                .monospace()
                                .size(10.5)
                                .color(theme.text_faint),
                        )
                        .truncate(),
                    );
                },
            );
            response
        })
        .inner;
    response
}

fn quiet_button(ui: &mut egui::Ui, theme: &Theme, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(12.5).color(theme.text))
            .fill(theme.bg_elev_2)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .corner_radius(egui::CornerRadius::same(5)),
    )
}

fn search_toggle(
    ui: &mut egui::Ui,
    theme: &Theme,
    selected: bool,
    text: &str,
    tooltip: &str,
) -> bool {
    ui.add_sized(
        egui::vec2(28.0, 24.0),
        egui::Button::new(
            egui::RichText::new(text)
                .monospace()
                .size(10.5)
                .color(if selected { theme.accent } else { theme.text_dim }),
        )
        .selected(selected)
        .fill(if selected {
            theme.accent_soft
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(if selected {
            egui::Stroke::new(1.0, theme.accent)
        } else {
            egui::Stroke::new(1.0, theme.border)
        }),
    )
    .on_hover_text(tooltip)
    .clicked()
}

struct SearchFileRowAction {
    toggle: bool,
    replace: bool,
}

fn search_file_row(
    ui: &mut egui::Ui,
    theme: &Theme,
    path: &VaultPath,
    match_count: usize,
    expanded: bool,
    show_replace: bool,
) -> SearchFileRowAction {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 26.0),
        egui::Sense::click(),
    );
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(4), theme.bg_hover);
    }

    let inner = rect.shrink2(egui::vec2(6.0, 0.0));
    let replace_width = if show_replace { 24.0 } else { 0.0 };
    let count_width = 34.0;
    let content_right = (inner.right() - count_width - replace_width).max(inner.left());
    let content_rect = egui::Rect::from_min_max(
        inner.min,
        egui::pos2(content_right, inner.bottom()),
    );
    let count_rect = egui::Rect::from_min_max(
        egui::pos2(content_right, inner.top()),
        egui::pos2(content_right + count_width, inner.bottom()),
    );

    let mut content_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    content_ui.spacing_mut().item_spacing.x = 4.0;
    content_ui.add_sized(
        egui::vec2(12.0, 24.0),
        egui::Label::new(
            egui::RichText::new(if expanded { "⌄" } else { "›" })
                .size(12.0)
                .color(theme.text_dim),
        ),
    );
    let path_width = content_ui.available_width().max(0.0);
    content_ui.add_sized(
        egui::vec2(path_width, 24.0),
        egui::Label::new(
            egui::RichText::new(path.as_str())
                .size(11.5)
                .strong()
                .color(theme.text),
        )
        .truncate(),
    );

    let mut count_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(count_rect)
            .layout(egui::Layout::centered_and_justified(
                egui::Direction::LeftToRight,
            )),
    );
    count_ui.label(
        egui::RichText::new(match_count.to_string())
            .monospace()
            .size(10.0)
            .color(theme.text_faint),
    );

    let replace = if show_replace {
        let replace_rect = egui::Rect::from_min_max(
            egui::pos2(count_rect.right(), inner.top()),
            inner.max,
        );
        ui.put(
            replace_rect,
            egui::Button::new(
                egui::RichText::new("R")
                    .monospace()
                    .size(10.0)
                    .color(theme.text_dim),
            )
            .frame(false),
        )
        .on_hover_text("Replace all matches in this file")
        .clicked()
    } else {
        false
    };

    response.clone().on_hover_text(path.as_str());
    SearchFileRowAction {
        toggle: response.clicked() && !replace,
        replace,
    }
}

fn search_match_row(
    ui: &mut egui::Ui,
    theme: &Theme,
    matched: &SearchMatch,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 26.0),
        egui::Sense::click(),
    );
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(4), theme.bg_hover);
    }

    let inner = rect.shrink2(egui::vec2(8.0, 0.0));
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row_ui.spacing_mut().item_spacing.x = 7.0;
    row_ui.add_sized(
        egui::vec2(52.0, 22.0),
        egui::Label::new(
            egui::RichText::new(format!("{}:{}", matched.line_number, matched.column))
                .monospace()
                .size(9.5)
                .color(theme.text_faint),
        )
        .truncate(),
    );
    row_ui.add_sized(
        egui::vec2(row_ui.available_width().max(0.0), 22.0),
        egui::Label::new(search_result_preview(matched, theme)).truncate(),
    );

    response.on_hover_text(format!(
        "{}:{}:{}",
        matched.path.as_str(),
        matched.line_number,
        matched.column
    ))
}

fn search_result_preview(matched: &SearchMatch, theme: &Theme) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let base = egui::TextFormat {
        font_id: egui::FontId::monospace(11.0),
        color: theme.text_dim,
        ..Default::default()
    };
    let highlighted = egui::TextFormat {
        color: theme.text,
        background: theme.accent_soft,
        ..base.clone()
    };
    let start = matched.preview_match_start.min(matched.line_text.len());
    let end = matched
        .preview_match_end
        .max(start)
        .min(matched.line_text.len());
    job.append(&matched.line_text[..start], 0.0, base.clone());
    job.append(&matched.line_text[start..end], 0.0, highlighted);
    job.append(&matched.line_text[end..], 0.0, base);
    job
}

fn parse_globs(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|glob| !glob.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn byte_to_char_index(text: &str, byte_index: usize) -> usize {
    let mut byte_index = byte_index.min(text.len());
    while !text.is_char_boundary(byte_index) {
        byte_index -= 1;
    }
    text[..byte_index].chars().count()
}

fn char_slice(text: &str, range: std::ops::Range<usize>) -> Option<&str> {
    let start = char_to_byte_index(text, range.start)?;
    let end = char_to_byte_index(text, range.end)?;
    text.get(start..end)
}

fn char_to_byte_index(text: &str, char_index: usize) -> Option<usize> {
    if char_index == text.chars().count() {
        return Some(text.len());
    }
    text.char_indices().nth(char_index).map(|(index, _)| index)
}

fn icon_button(ui: &mut egui::Ui, theme: &Theme, text: &str, tooltip: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2(18.0, 18.0),
        egui::Button::new(egui::RichText::new(text).size(12.0).color(theme.text_faint))
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(4)),
    )
    .on_hover_text(tooltip)
}

fn conflict_banner(ui: &mut egui::Ui, theme: &Theme, message: &str) {
    let w = theme.warn;
    egui::Frame::new()
        .fill(egui::Color32::from_rgba_unmultiplied(w.r(), w.g(), w.b(), 26))
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(w.r(), w.g(), w.b(), 70),
        ))
        .inner_margin(egui::Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(theme.warn, "!");
                ui.label(egui::RichText::new(message).size(12.0).color(theme.text));
            });
        });
}

/// Map a file path to the token handed to syntect (`find_syntax_by_token` accepts
/// extensions and names). Extensionless well-known files get an explicit mapping.
fn language_for_path(path: &VaultPath) -> &str {
    let path = path.as_path();
    match path.file_name().unwrap_or_default() {
        "Makefile" | "makefile" | "GNUmakefile" => return "Makefile",
        "Dockerfile" => return "Dockerfile",
        _ => {}
    }
    let extension = path.extension().unwrap_or_default();
    if extension.is_empty() {
        "txt"
    } else {
        extension
    }
}

fn rebuild_gutter(gutter: &mut String, line_count: usize) {
    use std::fmt::Write;
    let digits = line_count.max(1).ilog10() as usize + 1;
    gutter.clear();
    gutter.reserve(line_count * (digits + 1));
    for line in 1..=line_count {
        if line > 1 {
            gutter.push('\n');
        }
        let _ = write!(gutter, "{line:>digits$}");
    }
}

/// 1-based (line, column) for a char index, as shown in the status bar.
fn line_col_at(text: &str, char_index: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for ch in text.chars().take(char_index) {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn file_icon_accent(node: &TreeNode) -> egui::Color32 {
    match node.path.as_path().extension().unwrap_or_default() {
        "md" | "markdown" | "txt" => egui::Color32::from_rgb(55, 132, 214),
        "rs" => egui::Color32::from_rgb(209, 117, 54),
        "toml" | "ini" | "cfg" => egui::Color32::from_rgb(148, 104, 189),
        "json" | "lock" => egui::Color32::from_rgb(74, 157, 95),
        "yaml" | "yml" => egui::Color32::from_rgb(59, 153, 143),
        "js" | "jsx" | "ts" | "tsx" | "mjs" => egui::Color32::from_rgb(224, 192, 84),
        "py" => egui::Color32::from_rgb(86, 156, 214),
        "c" | "h" | "cpp" | "hpp" | "cc" | "hh" => egui::Color32::from_rgb(120, 145, 214),
        "go" => egui::Color32::from_rgb(86, 192, 214),
        "sh" | "bash" | "zsh" | "fish" => egui::Color32::from_rgb(140, 196, 116),
        "html" | "css" | "scss" => egui::Color32::from_rgb(214, 110, 84),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => {
            egui::Color32::from_rgb(198, 87, 142)
        }
        _ => egui::Color32::from_rgb(116, 125, 140),
    }
}

fn adjusted_active_tab(
    active: Option<usize>,
    removed: usize,
    remaining_len: usize,
) -> Option<usize> {
    let active = active?;
    if remaining_len == 0 {
        return None;
    }
    if active == removed {
        return Some(removed.min(remaining_len - 1));
    }
    if active > removed {
        return Some(active - 1);
    }
    Some(active)
}

fn vault_path_from_absolute(root: &std::path::Path, path: &std::path::Path) -> Option<VaultPath> {
    let relative = path.strip_prefix(root).ok()?;
    let relative = relative.to_str()?;
    VaultPath::try_from(relative).ok()
}

fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2(220.0, 26.0),
        egui::Button::new(format_menu_label(label, shortcut)).fill(egui::Color32::TRANSPARENT),
    )
}

fn checked_menu_item(
    ui: &mut egui::Ui,
    checked: bool,
    label: &str,
    shortcut: &str,
) -> egui::Response {
    let marker = if checked { "✓" } else { " " };
    ui.add_sized(
        egui::vec2(220.0, 26.0),
        egui::Button::new(format!("{marker}  {}", format_menu_label(label, shortcut)))
            .fill(egui::Color32::TRANSPARENT),
    )
}

fn format_menu_label(label: &str, shortcut: &str) -> String {
    if shortcut.is_empty() {
        label.to_owned()
    } else {
        format!("{label}    {shortcut}")
    }
}

/// Build the workspace pane tree: a single horizontal split [Sidebar | Editor] with
/// shares chosen to match a ~220 px sidebar in a ~1180 px window. Shares are
/// relative — when the window resizes, the panes scale proportionally. The user can
/// drag the separator; `egui_tiles` persists those shares across frames inside the
/// `Tree` itself, so there is no per-frame width drift.
fn build_tile_tree() -> (egui_tiles::Tree<Pane>, egui_tiles::TileId) {
    let mut tiles = egui_tiles::Tiles::default();
    let sidebar_tile = tiles.insert_pane(Pane::Sidebar);
    let editor_tile = tiles.insert_pane(Pane::Editor);

    let mut linear = egui_tiles::Linear::new(
        egui_tiles::LinearDir::Horizontal,
        vec![sidebar_tile, editor_tile],
    );
    // Shares total the child count (egui_tiles convention so inserted tiles can
    // default to share=1.0 without skewing the layout).
    linear.shares.set_share(sidebar_tile, 0.4);
    linear.shares.set_share(editor_tile, 1.6);
    let root = tiles.insert_container(linear);

    let tree = egui_tiles::Tree::new("lattice_workspace_tree", root, tiles);
    (tree, sidebar_tile)
}

struct LatticeBehavior<'a> {
    app: &'a mut LatticeApp,
}

impl<'a> egui_tiles::Behavior<Pane> for LatticeBehavior<'a> {
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        match *pane {
            Pane::Sidebar => self.app.draw_sidebar(ui),
            Pane::Editor => self.app.draw_main_area(ui),
        }
        egui_tiles::UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::Sidebar => "Files".into(),
            Pane::Editor => "Editor".into(),
        }
    }

    /// 1 px hairline gap painted in the theme border colour; matches the VSCode-style
    /// separator between editor groups.
    fn gap_width(&self, _style: &egui::Style) -> f32 {
        1.0
    }

    /// Minimum width/height for any child. Below this, drag-resize is clamped. Keeping
    /// it modest (120) lets the user collapse a pane down to almost-icon width if they
    /// want to maximise editor real estate.
    fn min_size(&self) -> f32 {
        120.0
    }

    fn resize_stroke(
        &self,
        _style: &egui::Style,
        resize_state: egui_tiles::ResizeState,
    ) -> egui::Stroke {
        let theme = &self.app.theme;
        match resize_state {
            egui_tiles::ResizeState::Idle => egui::Stroke::new(1.0, theme.border),
            egui_tiles::ResizeState::Hovering => egui::Stroke::new(1.0, theme.text_faint),
            egui_tiles::ResizeState::Dragging => egui::Stroke::new(1.0, theme.accent),
        }
    }

    /// Disable every simplification that could remove a Pane from the tree — we own
    /// the structure (3 fixed panes, toggled via `set_visible`) and don't want
    /// `egui_tiles` mutating the topology behind our back when one is hidden.
    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            prune_empty_tabs: false,
            prune_empty_containers: false,
            prune_single_child_tabs: false,
            prune_single_child_containers: false,
            all_panes_must_have_tabs: false,
            join_nested_linear_containers: false,
        }
    }
}

impl LatticeApp {
    /// Run the tile tree's UI pass. The tree lives on `LatticeApp` but the behavior
    /// borrows `&mut LatticeApp`, so we temporarily move the tree out, run, and move
    /// it back. `std::mem::replace` with `Tree::empty` is O(1) (just swaps a handful
    /// of fields) and allocates only the empty placeholder's default maps.
    fn show_tile_tree(&mut self, ui: &mut egui::Ui) {
        let placeholder =
            egui_tiles::Tree::<Pane>::empty(egui::Id::new("__lattice_tile_placeholder"));
        let mut tree = std::mem::replace(&mut self.tile_tree, placeholder);
        {
            let mut behavior = LatticeBehavior { app: self };
            tree.ui(&mut behavior, ui);
        }
        self.tile_tree = tree;
    }
}

impl eframe::App for LatticeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let workspace_find = ui.input(|input| {
            input.modifiers.ctrl && input.modifiers.shift && input.key_pressed(egui::Key::F)
        });
        let workspace_replace = ui.input(|input| {
            input.modifiers.ctrl && input.modifiers.shift && input.key_pressed(egui::Key::H)
        });
        if workspace_find {
            self.open_workspace_search(false);
        } else if ui.input(|input| {
            input.modifiers.ctrl && !input.modifiers.shift && input.key_pressed(egui::Key::F)
        }) {
            self.open_editor_find(false);
        }
        if workspace_replace {
            self.open_workspace_search(true);
        } else if ui.input(|input| {
            input.modifiers.ctrl && !input.modifiers.shift && input.key_pressed(egui::Key::H)
        }) {
            self.open_editor_find(true);
        }
        if ui.input(|input| input.key_pressed(egui::Key::F3)) {
            let delta = if ui.input(|input| input.modifiers.shift) {
                -1
            } else {
                1
            };
            self.navigate_editor_find(delta);
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::S)) {
            self.save_active_tab();
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::O)) {
            self.open_folder_dialog();
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::R)) {
            self.refresh_workspace_data();
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::P)) {
            self.quick_open_overlay = true;
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::B)) {
            self.show_sidebar = !self.show_sidebar;
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::W)) {
            self.close_active_tab();
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::N)) {
            self.start_new_file();
        }
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::Tab)) {
            let delta = if ui.input(|input| input.modifiers.shift) {
                -1
            } else {
                1
            };
            self.select_relative_tab(delta);
        }
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            if self.quick_open_overlay {
                self.quick_open_overlay = false;
            } else if self.workspace_search.replace_confirmation {
                self.workspace_search.replace_confirmation = false;
            } else if self.editor_find.visible {
                self.editor_find.visible = false;
            }
        }
        self.drain_worker_responses();
        self.process_pending_workspace_search();
        self.run_autosave();
        self.drain_watcher();
        self.process_pending_workspace_refresh();
        self.process_pending_external_check();

        let theme = self.theme.clone();
        egui::Panel::top("top_bar")
            .exact_size(30.0)
            .frame(
                egui::Frame::new()
                    .fill(theme.bg)
                    .stroke(egui::Stroke::new(1.0, theme.border)),
            )
            .show_inside(ui, |ui| self.draw_menu_bar(ui));

        if self.workspace.is_some() {
            egui::Panel::bottom("status_bar")
                .exact_size(22.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme.bg)
                        .stroke(egui::Stroke::new(1.0, theme.border)),
                )
                .show_inside(ui, |ui| self.draw_status_bar(ui, &theme));
        }

        // Sync the bool toggle (Ctrl+B / status-bar pill / View menu) to the tile
        // tree's per-tile visibility. `set_visible` is a cheap HashMap insert and
        // egui_tiles only re-runs the linear layout when child visibility actually
        // changes — no per-frame cost when nothing toggled.
        let want_sidebar = self.workspace.is_some() && self.show_sidebar;
        if self.tile_tree.is_visible(self.sidebar_tile) != want_sidebar {
            self.tile_tree.set_visible(self.sidebar_tile, want_sidebar);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme.bg))
            .show_inside(ui, |ui| {
                if let Some(error) = &self.open_error {
                    conflict_banner(ui, &theme, error);
                }
                if self.workspace.is_some() {
                    self.show_tile_tree(ui);
                } else {
                    self.draw_start_screen(ui);
                }
            });

        if self.quick_open_overlay && self.workspace.is_some() {
            self.draw_quick_open_overlay(ui.ctx());
        }
    }
}

fn load_settings() -> (Option<PathBuf>, AppSettings, Option<String>) {
    let Some(project_dirs) = directories::ProjectDirs::from("dev", "Lattice", "Lattice") else {
        return (None, AppSettings::default(), None);
    };
    let path = project_dirs.config_dir().join("settings.toml");
    let (settings, error) = match fs::read_to_string(&path) {
        Ok(contents) => match parse_settings(&contents) {
            Ok(settings) => (settings, None),
            Err(error) => {
                log::warn!("failed to parse settings {}: {error}", path.display());
                (
                    AppSettings::default(),
                    Some("Settings file is invalid; using defaults".to_owned()),
                )
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (AppSettings::default(), None)
        }
        Err(error) => {
            log::warn!("failed to read settings {}: {error}", path.display());
            (
                AppSettings::default(),
                Some("Settings file could not be read; using defaults".to_owned()),
            )
        }
    };
    (Some(path), settings, error)
}

fn parse_settings(contents: &str) -> Result<AppSettings, toml::de::Error> {
    toml::from_str(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_settings_rejects_invalid_toml() {
        assert!(parse_settings("theme = {").is_err());
    }

    #[test]
    fn parses_benchmark_mode() {
        let cli = Cli::try_parse_from([
            "lattice",
            "--bench",
            "--bench-query",
            "project",
            "/tmp/example",
        ])
        .unwrap();

        assert!(cli.bench);
        assert_eq!(cli.bench_queries, vec!["project"]);
        assert_eq!(cli.path, Some(PathBuf::from("/tmp/example")));
    }

    #[test]
    fn replace_tree_children_updates_nested_directory() {
        let parent_path = VaultPath::try_from("parent").unwrap();
        let child_path = VaultPath::try_from("parent/child").unwrap();
        let file_path = VaultPath::try_from("parent/child/note.md").unwrap();
        let mut tree = vec![TreeNode {
            path: parent_path,
            name: "parent".to_owned(),
            kind: TreeNodeKind::DirectoryLoaded {
                children: vec![TreeNode {
                    path: child_path.clone(),
                    name: "child".to_owned(),
                    kind: TreeNodeKind::DirectoryUnloaded,
                    warning: None,
                }],
            },
            warning: None,
        }];
        let replacement = vec![TreeNode {
            path: file_path,
            name: "note.md".to_owned(),
            kind: TreeNodeKind::File,
            warning: None,
        }];

        assert!(replace_tree_children(&mut tree, &child_path, replacement));

        let TreeNodeKind::DirectoryLoaded { children } = &tree[0].kind else {
            panic!("parent should stay loaded");
        };
        let TreeNodeKind::DirectoryLoaded { children } = &children[0].kind else {
            panic!("child should be loaded");
        };
        assert_eq!(children[0].path.as_str(), "parent/child/note.md");
    }

    #[test]
    fn replace_tree_children_duplicate_target_does_not_panic() {
        let duplicate = VaultPath::try_from("dup").unwrap();
        let mut tree = vec![
            TreeNode {
                path: duplicate.clone(),
                name: "dup".to_owned(),
                kind: TreeNodeKind::DirectoryUnloaded,
                warning: None,
            },
            TreeNode {
                path: duplicate.clone(),
                name: "dup-again".to_owned(),
                kind: TreeNodeKind::DirectoryUnloaded,
                warning: None,
            },
        ];

        assert!(replace_tree_children(&mut tree, &duplicate, Vec::new()));
        assert!(matches!(
            tree[0].kind,
            TreeNodeKind::DirectoryLoaded { ref children } if children.is_empty()
        ));
    }

    #[test]
    fn parses_comma_separated_search_globs() {
        assert_eq!(
            parse_globs("*.rs, src/**, ,tests/*.toml"),
            vec!["*.rs", "src/**", "tests/*.toml"]
        );
    }

    #[test]
    fn byte_and_char_search_offsets_handle_unicode() {
        let text = "aåßz";
        assert_eq!(byte_to_char_index(text, 0), 0);
        assert_eq!(byte_to_char_index(text, 3), 2);
        assert_eq!(char_slice(text, 1..3), Some("åß"));
    }
}
