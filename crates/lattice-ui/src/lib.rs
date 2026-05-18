pub fn apply_lattice_style(ctx: &egui::Context) {
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    });
}
