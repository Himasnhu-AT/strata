//! The sidebar (240 pt): Files (size, a live dot when following), Bookmarks (colour, name,
//! line), Saved filters and Recent. ashlar's `Sidebar`: folding sections, one Tab stop,
//! type-ahead, rename in place (bookmarks).

use ashlar::fs::RecentFiles;
use ashlar::prelude::*;
use ashlar::ui::sidebar::{ItemRef, Sidebar, SidebarItem, SidebarSection, SidebarState};

use super::fmt;
use super::theme;
use crate::commands::ids;
use crate::doc::{BookmarkColor, StrataDoc, view_keys};

/// Settings key of the recent log files.
pub const RECENT: &str = "recent_logs";
/// Recent files kept.
pub const RECENT_MAX: usize = 10;

const FILES: usize = 0;
const BOOKMARKS: usize = 1;
const SAVED: usize = 2;
const RECENT_SECTION: usize = 3;

/// What the sidebar keeps between frames.
#[derive(Default)]
pub struct SidebarUi {
    pub state: SidebarState,
}

/// Draw the sidebar.
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, st: &mut SidebarUi, active: Option<strata_core::FileId>) {
    let t = cx.tokens();
    let doc = cx.snapshot();
    let live = doc.live.clone();
    // Files
    let names: Vec<String> = doc.files.iter().map(|f| f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).collect();
    let sizes: Vec<String> = doc.files.iter().map(|f| live.get(f.id).map(|l| fmt::bytes(l.len())).unwrap_or_default()).collect();
    let following: Vec<bool> = doc.files.iter().map(|f| f.follow).collect();
    let ok = t.status.success;
    let dot = move |p: &egui::Painter, r: egui::Rect, _c: egui::Color32| {
        p.circle_filled(r.center(), 3.5, ok);
    };
    let blank = |_: &egui::Painter, _: egui::Rect, _: egui::Color32| {};
    let files: Vec<SidebarItem<'_>> = names
        .iter()
        .zip(&sizes)
        .zip(&following)
        .map(|((n, s), live)| {
            let item = SidebarItem::new(n).detail(s);
            if *live { item.icon(&dot).badge("live") } else { item.icon(&blank) }
        })
        .collect();
    // Bookmarks of the active file
    let active_file = active.and_then(|id| doc.file(id));
    let marks: Vec<(String, String, BookmarkColor, u64, u64)> = active_file
        .map(|f| f.bookmarks.iter().map(|b| (b.name.clone(), fmt::thousands(b.line + 1), b.color, b.id, b.line)).collect())
        .unwrap_or_default();
    let painters: Vec<Box<dyn Fn(&egui::Painter, egui::Rect, egui::Color32)>> = marks
        .iter()
        .map(|(_, _, c, ..)| {
            let color = theme::bookmark_color(&t, *c);
            Box::new(move |p: &egui::Painter, r: egui::Rect, _: egui::Color32| {
                p.rect_filled(egui::Rect::from_center_size(r.center(), egui::vec2(8.0, 8.0)), 2.0, color);
            }) as Box<dyn Fn(&egui::Painter, egui::Rect, egui::Color32)>
        })
        .collect();
    let bookmarks: Vec<SidebarItem<'_>> = marks.iter().zip(&painters).map(|((n, l, ..), p)| SidebarItem::new(n).detail(l).icon(p.as_ref())).collect();
    // Saved filters
    let saved: Vec<SidebarItem<'_>> = doc.saved_filters.iter().map(|f| SidebarItem::new(&f.name).detail(&f.text)).collect();
    // Recent (files not open now)
    let recent_paths: Vec<std::path::PathBuf> =
        RecentFiles::load(cx.settings, RECENT, RECENT_MAX).paths().iter().filter(|p| !doc.files.iter().any(|f| &f.path == *p)).cloned().collect();
    let recent_names: Vec<(String, String)> = recent_paths
        .iter()
        .map(|p| (p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), p.parent().map(fmt::tilde).unwrap_or_default()))
        .collect();
    let recent: Vec<SidebarItem<'_>> = recent_names.iter().map(|(n, d)| SidebarItem::new(n).detail(d)).collect();

    let sections = [
        SidebarSection::new("Files", &files).count(files.len()),
        SidebarSection::new("Bookmarks", &bookmarks).count(bookmarks.len()),
        SidebarSection::new("Saved filters", &saved).count(saved.len()),
        SidebarSection::new("Recent", &recent),
    ];
    // keep the active file selected
    if let Some(i) = active.and_then(|id| doc.files.iter().position(|f| f.id == id))
        && st.state.selected.is_none_or(|s| s.section == FILES)
    {
        st.state.selected = Some(ItemRef { section: FILES, item: i });
    }
    if cx.view::<String>(view_keys::FOCUS).as_deref() == Some("bookmarks") {
        st.state.set_collapsed(BOOKMARKS, false);
        cx.set_view(view_keys::FOCUS, &Value::Null);
        let id = ui.make_persistent_id(("ashlar_ui::sidebar", "strata.sidebar"));
        ui.memory_mut(|m| m.request_focus(id));
    }
    ui.add_space(t.space.md);
    let out = Sidebar::new("strata.sidebar", "Files, bookmarks and filters", &mut st.state, &sections).show(ui);
    let pick = out.activated.or(out.select);
    if let Some(at) = pick {
        match at.section {
            FILES => {
                if let Some(f) = doc.files.get(at.item) {
                    cx.set_view(view_keys::ACTIVE_FILE, &f.id.0);
                }
            }
            BOOKMARKS => {
                if let (Some(f), Some((.., line))) = (active_file, marks.get(at.item)) {
                    cx.dispatch(ids::VIEW_SELECT, json!({"file": f.id.0, "line": line + 1}));
                }
            }
            SAVED if out.activated.is_some() => {
                if let (Some(f), Some(s)) = (active_file, doc.saved_filters.get(at.item)) {
                    cx.dispatch(ids::FILTER_APPLY_SAVED, json!({"file": f.id.0, "name": s.name}));
                }
            }
            RECENT_SECTION if out.activated.is_some() => {
                if let Some(p) = recent_paths.get(at.item) {
                    cx.dispatch(ids::LOG_OPEN_DIALOG, json!({"path": p.display().to_string()}));
                }
            }
            _ => {}
        }
    }
    if let Some((at, name)) = out.renamed
        && at.section == BOOKMARKS
        && let (Some(f), Some((.., id, _))) = (active_file, marks.get(at.item))
    {
        cx.dispatch(ids::BOOKMARK_RENAME, json!({"file": f.id.0, "id": id, "name": name}));
    }
    if let Some(at) = out.context {
        match at.section {
            BOOKMARKS => {
                if let Some((name, ..)) = marks.get(at.item) {
                    st.state.start_rename(at, name);
                }
            }
            SAVED => {
                if let Some(s) = doc.saved_filters.get(at.item) {
                    cx.dispatch(ids::FILTER_DELETE_SAVED, json!({"name": s.name}));
                }
            }
            _ => {}
        }
    }
}
