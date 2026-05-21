use clap::Parser;
use eframe::egui;
use lattice_core::{AppSettings, ThemeMode, VaultPath};
use lattice_editor::EditorBuffer;
use lattice_workspace::{QuickOpenIndex, TreeNode, TreeNodeKind, Workspace, WorkspaceWatcher};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const LARGE_FILE_WARNING_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(author, version, about = "Native Markdown knowledge workspace")]
struct Cli {
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
}

fn main() -> eframe::Result {
    env_logger::init();
    std::panic::set_hook(Box::new(|panic_info| {
        log::error!("Lattice panic: {panic_info}");
    }));

    let cli = Cli::parse();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([840.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Lattice",
        options,
        Box::new(move |cc| {
            lattice_ui::apply_lattice_style(&cc.egui_ctx);
            Ok(Box::new(LatticeApp::new(&cc.egui_ctx, cli.path.clone())))
        }),
    )
}

struct LatticeApp {
    settings_path: Option<PathBuf>,
    settings: AppSettings,
    workspace: Option<Workspace>,
    watcher: Option<WorkspaceWatcher>,
    tree: Vec<TreeNode>,
    expanded_paths: BTreeSet<VaultPath>,
    quick_open: QuickOpenIndex,
    quick_open_ready: bool,
    quick_query: String,
    selected_path: Option<VaultPath>,
    tabs: Vec<EditorTab>,
    active_tab: Option<usize>,
    new_note_path: String,
    rename_target: String,
    open_error: Option<String>,
    status: String,
}

struct EditorTab {
    buffer: EditorBuffer,
    last_edit: Option<Instant>,
    conflict: Option<FileConflict>,
    large_file_warning: bool,
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

impl LatticeApp {
    fn new(ctx: &egui::Context, path: Option<PathBuf>) -> Self {
        let (settings_path, settings, settings_error) = load_settings();
        apply_theme(ctx, settings.theme);

        let mut app = Self {
            settings_path,
            settings,
            workspace: None,
            watcher: None,
            tree: Vec::new(),
            expanded_paths: BTreeSet::new(),
            quick_open: QuickOpenIndex::default(),
            quick_open_ready: false,
            quick_query: String::new(),
            selected_path: None,
            tabs: Vec::new(),
            active_tab: None,
            new_note_path: "Untitled.md".to_owned(),
            rename_target: String::new(),
            open_error: settings_error,
            status: "Ready".to_owned(),
        };
        if let Some(path) = path {
            app.open_path(path);
        }
        app
    }

    fn open_path(&mut self, path: PathBuf) {
        match Workspace::open_vault(path) {
            Ok(workspace) => self.set_workspace(workspace),
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Open failed".to_owned();
            }
        }
    }

    fn create_vault_at(&mut self, path: PathBuf) {
        match Workspace::create_vault(path) {
            Ok(workspace) => self.set_workspace(workspace),
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Create vault failed".to_owned();
            }
        }
    }

    fn set_workspace(&mut self, workspace: Workspace) {
        let root = workspace.vault().root.as_path().to_path_buf();
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
        self.workspace = Some(workspace);
        self.selected_path = None;
        self.tabs.clear();
        self.active_tab = None;
        self.rename_target.clear();
        self.expanded_paths.clear();
        self.quick_open = QuickOpenIndex::default();
        self.quick_open_ready = false;
        self.quick_query.clear();
        self.open_error = None;
        self.refresh_tree_root();
    }

    fn open_folder_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open Lattice Vault")
            .pick_folder()
        {
            self.open_path(path);
        }
    }

    fn create_folder_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Create or Open Lattice Vault")
            .pick_folder()
        {
            self.create_vault_at(path);
        }
    }

    fn refresh_tree_root(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        match workspace.list_tree(None) {
            Ok(tree) => {
                self.tree = tree;
                self.reload_expanded_directories();
                let root_count = self.tree.len();
                self.status = format!("Loaded {root_count} top-level items");
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Refresh failed".to_owned();
            }
        }
    }

    fn refresh_workspace_data(&mut self) {
        self.refresh_tree_root();
        self.quick_open_ready = false;
        self.quick_open = QuickOpenIndex::default();
    }

    fn reload_expanded_directories(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let mut expanded_paths: Vec<_> = self.expanded_paths.iter().cloned().collect();
        expanded_paths.sort_by_key(|path| path.as_str().matches('/').count());
        for path in expanded_paths {
            match workspace.list_tree(Some(&path)) {
                Ok(children) => {
                    replace_tree_children(&mut self.tree, &path, children);
                }
                Err(error) => {
                    log::warn!(
                        "failed to reload expanded tree path {}: {error}",
                        path.as_str()
                    );
                }
            }
        }
    }

    fn drain_watcher(&mut self) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let events = watcher.drain();
        if !events.is_empty() {
            self.refresh_workspace_data();
            self.check_external_changes();
        }
    }

    fn ensure_quick_open_index(&mut self) {
        if self.quick_open_ready {
            return;
        }
        let Some(workspace) = &self.workspace else {
            return;
        };
        match workspace.quick_open_index() {
            Ok(index) => {
                self.status = format!("Indexed {} files for quick open", index.len());
                self.quick_open = index;
                self.quick_open_ready = true;
                self.open_error = None;
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Quick open index failed".to_owned();
            }
        }
    }

    fn create_note(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        match VaultPath::try_from(self.new_note_path.as_str()) {
            Ok(path) => match workspace.create_file(&path, "") {
                Ok(()) => {
                    self.selected_path = Some(path.clone());
                    self.rename_target = path.as_str().to_owned();
                    self.status = format!("Created {}", path.as_str());
                    self.open_error = None;
                    self.refresh_workspace_data();
                    self.open_editor_file(path);
                }
                Err(error) => {
                    self.open_error = Some(error.to_string());
                    self.status = "Create note failed".to_owned();
                }
            },
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Invalid note path".to_owned();
            }
        }
    }

    fn rename_selected(&mut self) {
        let (Some(workspace), Some(from)) = (&self.workspace, self.selected_path.clone()) else {
            return;
        };
        match VaultPath::try_from(self.rename_target.as_str()) {
            Ok(to) => match workspace.rename_file(&from, &to) {
                Ok(()) => {
                    self.update_tab_path_after_rename(&from, &to);
                    self.selected_path = Some(to.clone());
                    self.rename_target = to.as_str().to_owned();
                    self.status = format!("Renamed to {}", to.as_str());
                    self.open_error = None;
                    self.refresh_workspace_data();
                }
                Err(error) => {
                    self.open_error = Some(error.to_string());
                    self.status = "Rename failed".to_owned();
                }
            },
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Invalid rename path".to_owned();
            }
        }
    }

    fn delete_selected(&mut self) {
        let (Some(workspace), Some(path)) = (&self.workspace, self.selected_path.clone()) else {
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
        match workspace.delete_file(&path) {
            Ok(()) => {
                self.status = format!("Deleted {}", path.as_str());
                self.close_tab_for_path(&path);
                self.selected_path = None;
                self.rename_target.clear();
                self.open_error = None;
                self.refresh_workspace_data();
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Delete failed".to_owned();
            }
        }
    }

    fn open_editor_file(&mut self, path: VaultPath) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.path().is_some_and(|tab_path| tab_path == &path))
        {
            self.active_tab = Some(index);
            self.selected_path = Some(path);
            return;
        }

        let Some(workspace) = &self.workspace else {
            return;
        };
        match workspace.open_file(&path) {
            Ok((contents, snapshot)) => {
                let large_file_warning = snapshot.size_bytes > LARGE_FILE_WARNING_BYTES;
                let tab = EditorTab {
                    buffer: EditorBuffer::from_disk(path.clone(), contents, snapshot),
                    last_edit: None,
                    conflict: None,
                    large_file_warning,
                };
                self.tabs.push(tab);
                self.active_tab = Some(self.tabs.len() - 1);
                self.selected_path = Some(path.clone());
                self.rename_target = path.as_str().to_owned();
                self.status = format!("Opened {}", path.as_str());
                self.open_error = None;
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Open file failed".to_owned();
            }
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
        let Some(workspace) = &self.workspace else {
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(path) = tab.path().cloned() else {
            return;
        };
        let contents = tab.buffer.text.clone();
        let base_snapshot = tab.buffer.base_snapshot.clone();

        match workspace.file_snapshot(&path) {
            Ok(Some(current_snapshot)) => {
                if let Some(base_snapshot) = &base_snapshot {
                    if &current_snapshot != base_snapshot {
                        if current_snapshot.content_hash == blake3::hash(contents.as_bytes()) {
                            if let Some(tab) = self.tabs.get_mut(index) {
                                tab.buffer.mark_saved(current_snapshot);
                                tab.conflict = None;
                                tab.last_edit = None;
                            }
                            return;
                        }
                        if let Some(tab) = self.tabs.get_mut(index) {
                            tab.conflict = Some(FileConflict::ModifiedOnDisk);
                        }
                        self.status = format!("Conflict on {}", path.as_str());
                        return;
                    }
                }
            }
            Ok(None) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.conflict = Some(FileConflict::DeletedOnDisk);
                }
                self.status = format!("File deleted on disk: {}", path.as_str());
                return;
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Save preflight failed".to_owned();
                return;
            }
        }

        match workspace.save_file(&path, &contents) {
            Ok(()) => match workspace.file_snapshot(&path) {
                Ok(Some(snapshot)) => {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.buffer.mark_saved(snapshot);
                        tab.conflict = None;
                        tab.last_edit = None;
                    }
                    self.status = format!("Saved {}", path.as_str());
                    self.open_error = None;
                    self.quick_open_ready = false;
                }
                Ok(None) => {
                    self.open_error =
                        Some("saved file disappeared before metadata refresh".to_owned());
                    self.status = "Save metadata failed".to_owned();
                }
                Err(error) => {
                    self.open_error = Some(error.to_string());
                    self.status = "Save metadata failed".to_owned();
                }
            },
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Save failed".to_owned();
            }
        }
    }

    fn overwrite_active_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        let Some(workspace) = &self.workspace else {
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(path) = tab.path().cloned() else {
            return;
        };
        let contents = tab.buffer.text.clone();
        match workspace.save_file(&path, &contents) {
            Ok(()) => match workspace.file_snapshot(&path) {
                Ok(Some(snapshot)) => {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.buffer.mark_saved(snapshot);
                        tab.conflict = None;
                        tab.last_edit = None;
                    }
                    self.status = format!("Overwrote {}", path.as_str());
                    self.open_error = None;
                }
                Ok(None) => self.open_error = Some("overwritten file disappeared".to_owned()),
                Err(error) => self.open_error = Some(error.to_string()),
            },
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Overwrite failed".to_owned();
            }
        }
    }

    fn reload_active_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        self.reload_tab(index);
    }

    fn reload_tab(&mut self, index: usize) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let Some(path) = self.tabs.get(index).and_then(EditorTab::path).cloned() else {
            return;
        };
        match workspace.open_file(&path) {
            Ok((contents, snapshot)) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.buffer.text = contents;
                    tab.buffer.mark_saved(snapshot);
                    tab.conflict = None;
                    tab.last_edit = None;
                }
                self.status = format!("Reloaded {}", path.as_str());
                self.open_error = None;
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Reload failed".to_owned();
            }
        }
    }

    fn run_autosave(&mut self) {
        let now = Instant::now();
        let indexes: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                (tab.buffer.dirty
                    && tab.conflict.is_none()
                    && tab.last_edit.is_some_and(|last_edit| {
                        now.duration_since(last_edit) >= AUTOSAVE_DEBOUNCE
                    }))
                .then_some(index)
            })
            .collect();
        for index in indexes {
            self.save_tab(index);
        }
    }

    fn check_external_changes(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let mut clean_deleted = Vec::new();
        let mut reload_clean = Vec::new();
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(path) = tab.path().cloned() else {
                continue;
            };
            let Some(base_snapshot) = tab.buffer.base_snapshot.clone() else {
                continue;
            };
            match workspace.file_snapshot(&path) {
                Ok(Some(current_snapshot)) if current_snapshot == base_snapshot => {}
                Ok(Some(current_snapshot)) if tab.buffer.dirty => {
                    if current_snapshot.content_hash == tab.buffer.content_hash() {
                        tab.buffer.mark_saved(current_snapshot);
                        tab.conflict = None;
                    } else {
                        tab.conflict = Some(FileConflict::ModifiedOnDisk);
                    }
                }
                Ok(Some(_)) => reload_clean.push(index),
                Ok(None) if tab.buffer.dirty => {
                    tab.conflict = Some(FileConflict::DeletedOnDisk);
                }
                Ok(None) => clean_deleted.push(index),
                Err(error) => {
                    self.open_error = Some(error.to_string());
                    self.status = "External change check failed".to_owned();
                }
            }
        }

        for index in reload_clean {
            self.reload_tab(index);
        }
        for index in clean_deleted.into_iter().rev() {
            self.tabs.remove(index);
            self.active_tab = adjusted_active_tab(self.active_tab, index, self.tabs.len());
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
        ui.vertical_centered_justified(|ui| {
            ui.add_space(96.0);
            ui.heading("Open a vault");
            if ui.button("Open Folder").clicked() {
                self.open_folder_dialog();
            }
            if ui.button("Create Folder").clicked() {
                self.create_folder_dialog();
            }
            ui.add_space(12.0);
            if !self.settings.recent_vaults.is_empty() {
                ui.label("Recent folders");
                for path in self.settings.recent_vaults.clone() {
                    if ui.button(path.display().to_string()).clicked() {
                        self.open_path(path);
                    }
                }
            }
        });
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Files");
            if ui.button("+").on_hover_text("New note").clicked() {
                self.create_note();
            }
        });
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.new_note_path);
        });
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for node in self.tree.clone() {
                self.draw_tree_node(ui, &node, 0);
            }
        });
    }

    fn draw_tree_node(&mut self, ui: &mut egui::Ui, node: &TreeNode, depth: usize) {
        let is_directory = matches!(
            node.kind,
            TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded
        );
        let is_expanded = self.expanded_paths.contains(&node.path);
        let selected = self.selected_path.as_ref() == Some(&node.path);
        let mut toggle = false;
        let mut select = false;

        let row_response = ui
            .horizontal(|ui| {
                ui.add_space((depth as f32) * 14.0);
                if is_directory {
                    let chevron = if is_expanded { "v" } else { ">" };
                    if ui.small_button(chevron).clicked() {
                        toggle = true;
                    }
                } else {
                    ui.add_space(22.0);
                }

                let icon = tree_icon(node);
                let label = if node.warning.is_some() {
                    format!("{icon} {}  !", node.name)
                } else {
                    format!("{icon} {}", node.name)
                };
                let response = ui.selectable_label(selected, label);
                if response.clicked() {
                    select = true;
                }
                if response.double_clicked() && is_directory {
                    toggle = true;
                }
                if let Some(warning) = &node.warning {
                    response.on_hover_text(warning);
                }
            })
            .response;
        if row_response.double_clicked() && is_directory {
            toggle = true;
        }

        if select {
            self.selected_path = Some(node.path.clone());
            self.rename_target = node.path.as_str().to_owned();
            if !is_directory {
                self.open_editor_file(node.path.clone());
            }
        }
        if toggle {
            self.toggle_directory(&node.path);
        }
        if let TreeNodeKind::DirectoryLoaded { children } = &node.kind {
            if is_expanded {
                for child in children {
                    self.draw_tree_node(ui, child, depth + 1);
                }
            }
        }
    }

    fn toggle_directory(&mut self, path: &VaultPath) {
        if self.expanded_paths.contains(path) {
            self.expanded_paths.remove(path);
            return;
        }
        let Some(workspace) = &self.workspace else {
            return;
        };
        match workspace.list_tree(Some(path)) {
            Ok(children) => {
                if replace_tree_children(&mut self.tree, path, children) {
                    self.expanded_paths.insert(path.clone());
                    self.status = format!("Loaded {}", path.as_str());
                    self.open_error = None;
                }
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = format!("Could not load {}", path.as_str());
            }
        }
    }

    fn draw_main_area(&mut self, ui: &mut egui::Ui) {
        ui.heading("Workspace");
        ui.horizontal(|ui| {
            ui.label("Quick open");
            ui.text_edit_singleline(&mut self.quick_query);
        });
        if !self.quick_query.trim().is_empty() {
            self.ensure_quick_open_index();
            for item in self.quick_open.search(&self.quick_query, 8) {
                if ui.button(item.path.as_str()).clicked() {
                    self.selected_path = Some(item.path.clone());
                    self.rename_target = item.path.as_str().to_owned();
                    self.open_editor_file(item.path);
                }
            }
            ui.separator();
        }

        if let Some(path) = &self.selected_path {
            ui.label(format!("Selected: {}", path.as_str()));
            ui.horizontal(|ui| {
                ui.label("Rename/move");
                ui.text_edit_singleline(&mut self.rename_target);
                if ui.button("Apply").clicked() {
                    self.rename_selected();
                }
                if ui.button("Delete").clicked() {
                    self.delete_selected();
                }
            });
        } else {
            ui.label("Select a file or create a new note.");
        }
        ui.separator();
        self.draw_editor(ui);
    }

    fn draw_editor(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() {
            ui.label("No file open.");
            return;
        }

        ui.horizontal_wrapped(|ui| {
            for (index, tab) in self.tabs.iter().enumerate() {
                let selected = self.active_tab == Some(index);
                let dirty = if tab.buffer.dirty { "*" } else { "" };
                let label = format!("{dirty}{}", tab.display_name());
                if ui.selectable_label(selected, label).clicked() {
                    self.active_tab = Some(index);
                    if let Some(path) = tab.path() {
                        self.selected_path = Some(path.clone());
                        self.rename_target = path.as_str().to_owned();
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                self.save_active_tab();
            }
            if ui.button("Reload").clicked() {
                self.reload_active_tab();
            }
            if ui.button("Overwrite").clicked() {
                self.overwrite_active_tab();
            }
            if ui.button("Close").clicked() {
                self.close_active_tab();
            }
        });

        let Some(index) = self.active_tab else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };

        if tab.large_file_warning {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Large file: live editor features may be limited.",
            );
        }
        if let Some(conflict) = tab.conflict {
            ui.colored_label(egui::Color32::RED, conflict.message());
        }

        let response = egui::TextEdit::multiline(&mut tab.buffer.text)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .desired_rows(28)
            .lock_focus(true)
            .show(ui)
            .response;
        if response.changed() {
            tab.buffer.dirty = true;
            tab.last_edit = Some(Instant::now());
        }
    }

    fn close_active_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        if self
            .tabs
            .get(index)
            .is_some_and(|tab| tab.buffer.dirty || tab.conflict.is_some())
        {
            self.status = "Save, reload, or resolve conflict before closing".to_owned();
            return;
        }
        self.tabs.remove(index);
        self.active_tab = adjusted_active_tab(Some(index), index, self.tabs.len());
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
            let children = pending_children
                .take()
                .expect("children should only be moved into one tree node");
            node.kind = TreeNodeKind::DirectoryLoaded { children };
            node.expanded = true;
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

fn tree_icon(node: &TreeNode) -> &'static str {
    match node.kind {
        TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded => "[DIR]",
        TreeNodeKind::File => match node.path.as_path().extension().unwrap_or_default() {
            "md" | "markdown" => "[MD]",
            "rs" => "[RS]",
            "toml" => "[TOML]",
            "json" => "[JSON]",
            "yaml" | "yml" => "[YAML]",
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => "[IMG]",
            "pdf" => "[PDF]",
            _ => "[FILE]",
        },
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

impl eframe::App for LatticeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::S)) {
            self.save_active_tab();
        }
        self.run_autosave();
        self.drain_watcher();

        egui::Panel::top("top_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Lattice");
                ui.separator();
                if ui.button("Open Folder").clicked() {
                    self.open_folder_dialog();
                }
                if ui.button("Create Folder").clicked() {
                    self.create_folder_dialog();
                }
                if ui.button("Refresh").clicked() {
                    self.refresh_workspace_data();
                }
                ui.separator();
                if let Some(workspace) = &self.workspace {
                    ui.label(&workspace.vault().name);
                } else {
                    ui.label("No vault open");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.status);
                });
            });
        });

        if self.workspace.is_some() {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(300.0)
                .show_inside(ui, |ui| self.draw_sidebar(ui));
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(error) = &self.open_error {
                ui.colored_label(egui::Color32::RED, error);
                ui.separator();
            }
            if self.workspace.is_some() {
                self.draw_main_area(ui);
            } else {
                self.draw_start_screen(ui);
            }
        });
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

fn apply_theme(ctx: &egui::Context, theme: ThemeMode) {
    match theme {
        ThemeMode::System => {}
        ThemeMode::Light => ctx.set_visuals(egui::Visuals::light()),
        ThemeMode::Dark => ctx.set_visuals(egui::Visuals::dark()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_settings_rejects_invalid_toml() {
        assert!(parse_settings("theme = {").is_err());
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
                    expanded: false,
                    git_status: None,
                    warning: None,
                }],
            },
            expanded: true,
            git_status: None,
            warning: None,
        }];
        let replacement = vec![TreeNode {
            path: file_path,
            name: "note.md".to_owned(),
            kind: TreeNodeKind::File,
            expanded: false,
            git_status: None,
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
}
