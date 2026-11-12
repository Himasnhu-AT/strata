//! Strata's look (docs/DESIGN.md, "Visual language") mapped onto ashlar's design tokens, so every
//! ashlar component draws in it, plus the few colours tokens have no slot for (log levels).
//! Views read colours from `cx.tokens()` / [`tokens::current`], never from literals.
//!
//! Dark first: the dark set is the design's palette; the light set (the OS in light mode) is
//! ashlar's neutral light set with Strata's amber accent, darkened for contrast on white.

use ashlar::egui::{self, Color32};
use ashlar::ui::fonts::FontInstall;
use ashlar::ui::tokens::{self, Tokens};
use strata_core::Level;

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The design's palette (dark).
pub mod palette {
    use super::{Color32, rgb};
    pub const BG: Color32 = rgb(0x0F1115);
    pub const PANEL: Color32 = rgb(0x161A20);
    pub const RAISED: Color32 = rgb(0x1D222A);
    pub const BORDER: Color32 = rgb(0x2A303A);
    pub const TEXT: Color32 = rgb(0xE6E8EB);
    pub const MUTED: Color32 = rgb(0x9AA3AE);
    pub const ACCENT: Color32 = rgb(0xF2A93B);
    pub const WARN: Color32 = rgb(0xFFD36E);
    pub const ERROR: Color32 = rgb(0xF2555A);
    pub const INFO: Color32 = rgb(0x5B9DFF);
    pub const OK: Color32 = rgb(0x3CC8A0);
}

/// Row height of the log table and type sizes (docs/DESIGN.md).
pub const ROW_HEIGHT: f32 = 28.0;
pub const MONO_SIZE: f32 = 12.0;
pub const UI_SIZE: f32 = 13.0;
pub const TOOLBAR_HEIGHT: f32 = 44.0;
pub const SIDEBAR_WIDTH: f32 = 240.0;
pub const HISTOGRAM_HEIGHT: f32 = 56.0;
pub const DETAIL_HEIGHT: f32 = 220.0;

/// The dark set: the design's palette in ashlar's slots.
pub fn dark() -> Tokens {
    let mut t = Tokens::dark();
    t.surface.app = palette::BG;
    t.surface.panel = palette::PANEL;
    t.surface.raised = palette::RAISED;
    t.surface.sunken = palette::BG;
    t.surface.row_alt = rgb(0x12151A);
    t.surface.hover = rgb(0x222833);
    t.surface.pressed = palette::BORDER;
    t.text.primary = palette::TEXT;
    t.text.secondary = palette::MUTED;
    t.text.disabled = rgb(0x6B7480);
    t.text.on_accent = palette::BG;
    t.status.info = palette::INFO;
    t.status.success = palette::OK;
    t.status.warning = palette::WARN;
    t.status.danger = palette::ERROR;
    t.accent = palette::ACCENT;
    t.accent_hover = rgb(0xFFBE5C);
    t.selection = rgb(0x3A3020);
    t.selection_text = palette::TEXT;
    t.focus = palette::ACCENT;
    t.border = palette::BORDER;
    t.border_strong = rgb(0x3A4250);
    t.separator = palette::BORDER;
    shared(t)
}

/// The light set: ashlar's light neutrals with the amber accent.
pub fn light() -> Tokens {
    let mut t = Tokens::light();
    t.accent = rgb(0xA35F00);
    t.accent_hover = rgb(0x8A5000);
    t.focus = rgb(0xA35F00);
    t.selection = rgb(0xFBE7C6);
    t.selection_text = rgb(0x1A1C20);
    t.text.on_accent = Color32::WHITE;
    t.status.danger = rgb(0xC62828);
    t.status.warning = rgb(0x8A6100);
    t.status.info = rgb(0x1F5FBF);
    t.status.success = rgb(0x137A5C);
    shared(t)
}

fn shared(mut t: Tokens) -> Tokens {
    t.type_scale.body = UI_SIZE;
    t.type_scale.button = 12.0;
    t.type_scale.small = 11.0;
    t.type_scale.mono = MONO_SIZE;
    t.radius.md = 6;
    t.control_height = 28.0;
    t
}

/// The colour of a level's word (and its chip): never the only sign of the level.
pub fn level_color(t: &Tokens, level: Option<Level>) -> Color32 {
    match level {
        Some(Level::Error | Level::Fatal) => t.status.danger,
        Some(Level::Warn) => t.status.warning,
        Some(Level::Info) => {
            if t.dark {
                rgb(0x9CC3FF)
            } else {
                t.status.info
            }
        }
        _ => t.text.secondary,
    }
}

/// `color` at `alpha` (0–255) over whatever is behind.
pub fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// The colour of a bookmark.
pub fn bookmark_color(t: &Tokens, c: crate::doc::BookmarkColor) -> Color32 {
    use crate::doc::BookmarkColor as B;
    match c {
        B::Amber => t.accent,
        B::Red => t.status.danger,
        B::Yellow => t.status.warning,
        B::Blue => t.status.info,
        B::Green => t.status.success,
        B::Grey => t.text.secondary,
    }
}

static INTER: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
static MONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

/// Install the fonts (Inter for the interface, JetBrains Mono for log text; Mono also backs up
/// Inter's missing symbols such as `▸`) and the token sets. The theme mode stays the user's.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontInstall::new();
    let ok = fonts
        .add("inter", INTER)
        .and_then(|f| f.add("mono", MONO))
        .and_then(|f| f.primary(egui::FontFamily::Proportional, "inter"))
        .and_then(|f| f.primary(egui::FontFamily::Monospace, "mono"))
        .and_then(|f| f.fallback("mono-fallback", MONO))
        .is_ok();
    if ok {
        fonts.install(ctx);
    }
    tokens::apply(ctx, &light());
    tokens::apply(ctx, &dark());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_readable_on_every_surface() {
        for t in [dark(), light()] {
            for bg in [t.surface.app, t.surface.panel, t.surface.raised] {
                assert!(tokens::contrast_ratio(t.text.primary, bg) >= 4.5, "text on {bg:?}");
                assert!(tokens::contrast_ratio(t.text.secondary, bg) >= 4.5, "muted on {bg:?}");
            }
            for level in [Level::Error, Level::Warn, Level::Info, Level::Debug] {
                let c = level_color(&t, Some(level));
                assert!(tokens::contrast_ratio(c, t.surface.panel) >= 4.5, "{level:?} on panel ({} theme)", if t.dark { "dark" } else { "light" });
            }
        }
    }
}
