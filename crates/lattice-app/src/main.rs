use clap::Parser;
use eframe::egui;
use lattice_core::{AppSettings, ThemeMode};
use lattice_workspace::Workspace;
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
    files: Vec<String>,
    open_error: Option<String>,
    status: String,
}

impl LatticeApp {
    fn new(ctx: &egui::Context, path: Option<PathBuf>) -> Self {
        let (settings_path, settings) = load_settings();
        apply_theme(ctx, settings.theme);

        let mut app = Self {
            settings_path,
            settings,
            workspace: None,
            files: Vec::new(),
            open_error: None,
            status: "Ready".to_owned(),
        };
        if let Some(path) = path {
            app.open_path(path);
        }
        app
    }

    fn open_path(&mut self, path: PathBuf) {
        match Workspace::open_vault(path) {
            Ok(workspace) => {
                let root = workspace.vault().root.as_path().to_path_buf();
                let files = workspace
                    .list_files()
                    .map(|files| {
                        files
                            .into_iter()
                            .map(|file| file.path.as_str().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                self.settings.remember_vault(root);
                self.save_settings();
                self.files = files;
                self.status = format!("Opened {}", workspace.vault().name);
                self.workspace = Some(workspace);
                self.open_error = None;
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                self.status = "Open failed".to_owned();
            }
        }
    }

    fn open_folder_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open Lattice Vault")
            .pick_folder()
        {
            self.open_path(path);
        }
    }

    fn refresh_files(&mut self) {
        if let Some(workspace) = &self.workspace {
            match workspace.list_files() {
                Ok(files) => {
                    self.files = files
                        .into_iter()
                        .map(|file| file.path.as_str().to_owned())
                        .collect();
                    self.status = format!("Indexed {} files", self.files.len());
                }
                Err(error) => {
                    self.open_error = Some(error.to_string());
                    self.status = "Refresh failed".to_owned();
                }
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
}

impl eframe::App for LatticeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Lattice");
                ui.separator();
                if ui.button("Open Folder").clicked() {
                    self.open_folder_dialog();
                }
                if ui.button("Refresh").clicked() {
                    self.refresh_files();
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
            egui::SidePanel::left("sidebar")
                .resizable(true)
                .default_width(260.0)
                .show(ctx, |ui| {
                    ui.heading("Files");
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for file in self.files.iter().take(1_000) {
                            ui.label(file);
                        }
                    });
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(error) = &self.open_error {
                ui.colored_label(egui::Color32::RED, error);
            }
            if self.workspace.is_some() {
                ui.heading("Editor");
                ui.label("Select a Markdown file from the tree once the editor surface lands in Phase 3.");
            } else {
                ui.vertical_centered_justified(|ui| {
                    ui.add_space(96.0);
                    ui.heading("Open a vault");
                    if ui.button("Open Folder").clicked() {
                        self.open_folder_dialog();
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
        });
    }
}

fn load_settings() -> (Option<PathBuf>, AppSettings) {
    let Some(project_dirs) = directories::ProjectDirs::from("dev", "Lattice", "Lattice") else {
        return (None, AppSettings::default());
    };
    let path = project_dirs.config_dir().join("settings.toml");
    let settings = fs::read_to_string(&path)
        .ok()
        .and_then(|contents| toml::from_str(&contents).ok())
        .unwrap_or_default();
    (Some(path), settings)
}

fn apply_theme(ctx: &egui::Context, theme: ThemeMode) {
    match theme {
        ThemeMode::System => {}
        ThemeMode::Light => ctx.set_visuals(egui::Visuals::light()),
        ThemeMode::Dark => ctx.set_visuals(egui::Visuals::dark()),
    }
}
