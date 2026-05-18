use clap::Parser;
use eframe::egui;
use lattice_workspace::Workspace;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(author, version, about = "Native Markdown knowledge workspace")]
struct Cli {
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
}

fn main() -> eframe::Result {
    env_logger::init();
    let cli = Cli::parse();
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Lattice",
        options,
        Box::new(move |cc| {
            lattice_ui::apply_lattice_style(&cc.egui_ctx);
            Ok(Box::new(LatticeApp::new(cli.path.clone())))
        }),
    )
}

struct LatticeApp {
    workspace: Option<Workspace>,
    open_error: Option<String>,
}

impl LatticeApp {
    fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            workspace: None,
            open_error: None,
        };
        if let Some(path) = path {
            app.open_path(path);
        }
        app
    }

    fn open_path(&mut self, path: PathBuf) {
        match Workspace::open_vault(path) {
            Ok(workspace) => {
                self.workspace = Some(workspace);
                self.open_error = None;
            }
            Err(error) => self.open_error = Some(error.to_string()),
        }
    }
}

impl eframe::App for LatticeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Lattice");
                ui.separator();
                if let Some(workspace) = &self.workspace {
                    ui.label(&workspace.vault().name);
                } else {
                    ui.label("No vault open");
                }
            });
        });

        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.heading("Files");
                if let Some(workspace) = &self.workspace {
                    match workspace.list_files() {
                        Ok(files) => {
                            for file in files.iter().take(500) {
                                ui.label(file.path.as_str());
                            }
                        }
                        Err(error) => {
                            ui.colored_label(egui::Color32::RED, error.to_string());
                        }
                    }
                } else {
                    ui.label("Open a folder from the command line to start.");
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(error) = &self.open_error {
                ui.colored_label(egui::Color32::RED, error);
            }
            ui.heading("Editor");
            ui.label("Native Rust workspace skeleton. Editor, preview, history, and diff views land in the next phases.");
        });
    }
}
