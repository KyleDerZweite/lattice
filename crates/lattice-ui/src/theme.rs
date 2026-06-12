//! Design tokens and egui theming, derived from the reference `styles-v2.css`.
//!
//! All translucent overlays use [`Color32::from_rgba_unmultiplied`] so a low alpha
//! over `--bg` reads as a *subtle* tint (the reference intent), rather than the
//! blown-out result of the previous `from_rgba_premultiplied(255,255,255,a)` idiom.

use egui::{Color32, CornerRadius, Margin, Shadow, Stroke};

// ── Layout sizes (CSS px → egui points) ──────────────────────────────────────
pub const MENUBAR_H: f32 = 30.0;
pub const STATUSBAR_H: f32 = 22.0;
pub const SIDEBAR_W: f32 = 220.0;
pub const GRAPH_W: f32 = 320.0;
pub const ROW_H: f32 = 24.0;
pub const TAB_H: f32 = 32.0;

// ── Corner radii ─────────────────────────────────────────────────────────────
pub const RADIUS_SM: u8 = 4;
pub const RADIUS_MD: u8 = 5;
pub const RADIUS_LG: u8 = 8;

/// Type scale (px), matching the reference CSS.
pub mod fs {
    pub const UI: f32 = 13.0; // base
    pub const MENU: f32 = 12.5;
    pub const META: f32 = 10.5; // sidebar head, status bar, kbd hints, mono meta
    pub const TAB: f32 = 12.5;
    pub const BODY: f32 = 15.0; // rendered markdown body
    pub const H1: f32 = 26.0;
    pub const H2: f32 = 18.0;
    pub const H3: f32 = 15.0;
    pub const CODE: f32 = 13.0; // ~0.88em of body
    pub const PRE: f32 = 12.5;
    pub const RAW: f32 = 13.5; // raw source textarea
    pub const QO_INPUT: f32 = 14.0;
    pub const QO_NAME: f32 = 13.0;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeKind {
    Dark,
    Light,
}

/// A resolved palette of design tokens.
#[derive(Clone, Debug)]
pub struct Theme {
    pub kind: ThemeKind,
    // surfaces
    pub bg: Color32,
    pub bg_elev: Color32,
    pub bg_elev_2: Color32,
    pub bg_hover: Color32,
    pub bg_active: Color32,
    pub bg_selected: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    // text
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    // accent + status
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_fg: Color32,
    pub warn: Color32,
    pub danger: Color32,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            bg: Color32::from_rgb(0x1d, 0x1d, 0x20),
            bg_elev: Color32::from_rgb(0x23, 0x23, 0x26),
            bg_elev_2: Color32::from_rgb(0x2a, 0x2a, 0x2e),
            bg_hover: Color32::from_rgba_unmultiplied(255, 255, 255, 10), // 0.04
            bg_active: Color32::from_rgba_unmultiplied(255, 255, 255, 18), // 0.07
            bg_selected: Color32::from_rgba_unmultiplied(255, 255, 255, 13), // 0.05
            border: Color32::from_rgba_unmultiplied(255, 255, 255, 15),   // 0.06
            border_strong: Color32::from_rgba_unmultiplied(255, 255, 255, 28), // 0.11
            text: Color32::from_rgb(0xe3, 0xe3, 0xe6),
            text_dim: Color32::from_rgb(0x8a, 0x8a, 0x92),
            text_faint: Color32::from_rgb(0x5d, 0x5d, 0x65),
            accent: Color32::from_rgb(173, 141, 253), // oklch(0.72 0.16 295)
            accent_soft: Color32::from_rgba_unmultiplied(173, 141, 253, 41), // 0.16
            accent_fg: Color32::from_rgb(0x0b, 0x0b, 0x0d),
            warn: Color32::from_rgb(232, 170, 78),  // oklch(0.78 0.13 75)
            danger: Color32::from_rgb(234, 106, 100), // oklch(0.68 0.16 25)
        }
    }

    pub fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            bg: Color32::from_rgb(0xfb, 0xfa, 0xf7),
            bg_elev: Color32::from_rgb(0xff, 0xff, 0xff),
            bg_elev_2: Color32::from_rgb(0xf0, 0xee, 0xe9),
            bg_hover: Color32::from_rgba_unmultiplied(0, 0, 0, 9),    // 0.035
            bg_active: Color32::from_rgba_unmultiplied(0, 0, 0, 14),  // 0.055
            bg_selected: Color32::from_rgba_unmultiplied(0, 0, 0, 11), // 0.045
            border: Color32::from_rgba_unmultiplied(0, 0, 0, 18),     // 0.07
            border_strong: Color32::from_rgba_unmultiplied(0, 0, 0, 36), // 0.14
            text: Color32::from_rgb(0x1d, 0x1d, 0x20),
            text_dim: Color32::from_rgb(0x6a, 0x6a, 0x72),
            text_faint: Color32::from_rgb(0x9c, 0x9c, 0xa3),
            accent: Color32::from_rgb(173, 141, 253),
            accent_soft: Color32::from_rgba_unmultiplied(173, 141, 253, 41),
            accent_fg: Color32::from_rgb(0xff, 0xff, 0xff),
            warn: Color32::from_rgb(232, 170, 78),
            danger: Color32::from_rgb(234, 106, 100),
        }
    }

    /// Apply this palette to the egui context, building `Style`/`Visuals` from tokens.
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.kind {
            ThemeKind::Dark => egui::ThemePreference::Dark,
            ThemeKind::Light => egui::ThemePreference::Light,
        });
        ctx.global_style_mut(|style| self.write_style(style));
    }

    fn write_style(&self, style: &mut egui::Style) {
        let v = &mut style.visuals;
        v.dark_mode = self.kind == ThemeKind::Dark;
        v.panel_fill = self.bg;
        v.window_fill = self.bg_elev;
        v.window_stroke = Stroke::new(1.0, self.border_strong);
        v.window_corner_radius = CornerRadius::same(RADIUS_LG);
        v.menu_corner_radius = CornerRadius::same(7);
        v.extreme_bg_color = self.bg_elev; // single-line text edits
        v.faint_bg_color = self.bg_elev_2;
        v.code_bg_color = self.bg_elev;
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.warn;
        v.error_fg_color = self.danger;

        let shadow = Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(80),
        };
        v.window_shadow = shadow;
        v.popup_shadow = shadow;

        v.selection.bg_fill = self.accent_soft;
        v.selection.stroke = Stroke::new(1.0, self.accent);
        v.text_cursor.stroke = Stroke::new(1.0, self.accent); // caret-color: accent

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.bg;
        w.noninteractive.weak_bg_fill = self.bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        w.noninteractive.corner_radius = CornerRadius::same(RADIUS_MD);

        // Quiet by default: transparent until hovered (GTK/Obsidian-leaning chrome).
        w.inactive.bg_fill = self.bg_elev_2;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, self.text_dim);
        w.inactive.corner_radius = CornerRadius::same(RADIUS_MD);
        w.inactive.expansion = 0.0;

        w.hovered.bg_fill = self.bg_active;
        w.hovered.weak_bg_fill = self.bg_active;
        w.hovered.bg_stroke = Stroke::new(1.0, self.border_strong);
        w.hovered.fg_stroke = Stroke::new(1.0, self.text);
        w.hovered.corner_radius = CornerRadius::same(RADIUS_MD);
        w.hovered.expansion = 0.0;

        w.active.bg_fill = self.bg_active;
        w.active.weak_bg_fill = self.bg_active;
        w.active.bg_stroke = Stroke::new(1.0, self.border_strong);
        w.active.fg_stroke = Stroke::new(1.0, self.text);
        w.active.corner_radius = CornerRadius::same(RADIUS_MD);
        w.active.expansion = 0.0;

        w.open.bg_fill = self.bg_active;
        w.open.weak_bg_fill = self.bg_active;
        w.open.bg_stroke = Stroke::new(1.0, self.border);
        w.open.fg_stroke = Stroke::new(1.0, self.text);
        w.open.corner_radius = CornerRadius::same(RADIUS_MD);

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(6.0, 4.0);
        s.button_padding = egui::vec2(9.0, 3.0);
        s.menu_margin = Margin::same(4);
        s.indent = 12.0;
        s.scroll.bar_width = 8.0;
        s.scroll.floating = false;
    }
}
