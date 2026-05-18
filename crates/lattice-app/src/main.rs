use clap::Parser;
use eframe::egui;
use lattice_core::{AppSettings, ThemeMode, VaultPath};
use lattice_workspace::{QuickOpenIndex, TreeNode, TreeNodeKind, Workspace, WorkspaceWatcher};
use std::fs;
use std::path::PathBuf;

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
    quick_open: QuickOpenIndex,
    quick_query: String,
    selected_path: Option<VaultPath>,
    new_note_path: String,
    rename_target: String,
    open_error: Option<String>,
    status: String,
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
            quick_open: QuickOpenIndex::default(),
            quick_query: String::new(),
            selected_path: None,
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
        self.rename_target.clear();
        self.open_error = None;
        self.refresh_workspace_data();
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

    fn refresh_workspace_data(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        match (workspace.list_tree(None), workspace.quick_open_index()) {
            (Ok(tree), Ok(index)) => {
                self.tree = tree;
                self.quick_open = index;
                self.status = format!("Indexed {} files", self.quick_open.len());
            }
            (Err(error), _) | (_, Err(error)) => {
                self.open_error = Some(error.to_string());
                self.status = "Refresh failed".to_owned();
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
        let (Some(workspace), Some(from)) = (&self.workspace, &self.selected_path) else {
            return;
        };
        match VaultPath::try_from(self.rename_target.as_str()) {
            Ok(to) => match workspace.rename_file(from, &to) {
                Ok(()) => {
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
        let (Some(workspace), Some(path)) = (&self.workspace, &self.selected_path) else {
            return;
        };
        match workspace.delete_file(path) {
            Ok(()) => {
                self.status = format!("Deleted {}", path.as_str());
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
        ui.horizontal(|ui| {
            ui.add_space((depth as f32) * 14.0);
            let icon = match node.kind {
                TreeNodeKind::File => "md",
                TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded => "dir",
            };
            let selected = self.selected_path.as_ref() == Some(&node.path);
            if ui
                .selectable_label(selected, format!("{icon} {}", node.name))
                .clicked()
            {
                self.selected_path = Some(node.path.clone());
                self.rename_target = node.path.as_str().to_owned();
            }
        });
        if let TreeNodeKind::DirectoryLoaded { children } = &node.kind {
            for child in children {
                self.draw_tree_node(ui, child, depth + 1);
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
            for item in self.quick_open.search(&self.quick_query, 8) {
                if ui.button(item.path.as_str()).clicked() {
                    self.selected_path = Some(item.path.clone());
                    self.rename_target = item.path.as_str().to_owned();
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
    }
}

impl eframe::App for LatticeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
}
