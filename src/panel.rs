//! The panel under the menu bar icon: opening it there, and what closing it means.

use crate::state::Shared;
use crate::tasks::{self, Task};
use crate::view::publish;
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use tauri::{AppHandle, Manager};
use tauri::{Emitter, PhysicalPosition, Rect, WebviewWindow};

/// The panel's size, in points.
pub const PANEL: (f64, f64) = (400.0, 620.0);

/// Opens the panel under the icon, or closes it.
pub fn toggle(app: &AppHandle, icon: Rect) {
    let Some(panel) = app.get_webview_window("popover") else { return };
    let shared = app.state::<Arc<Shared>>();
    let just_hidden = Utc::now().timestamp_millis() - shared.hidden_at.load(Relaxed) < 250;
    if panel.is_visible().unwrap_or(false) || just_hidden {
        let _ = panel.hide();
        pressed(app, false);
        return;
    }
    open(app, &panel, icon);
}

/// Opens the panel under the icon, if it isn't open: when macOS brings pimbar
/// forward, say for a click on one of its notifications. Even right after it
/// closed: clicking the notification is what closed it.
pub fn show(app: &AppHandle) {
    let Some(panel) = app.get_webview_window("popover") else { return };
    if panel.is_visible().unwrap_or(false) {
        return;
    }
    if let Some(icon) = app.tray_by_id("pim").and_then(|t| t.rect().ok().flatten()) {
        open(app, &panel, icon);
    }
}

fn open(app: &AppHandle, panel: &WebviewWindow, icon: Rect) {
    place(panel, icon);
    let _ = panel.show();
    let _ = panel.set_focus();
    pressed(app, true);
}

/// Shows the menu bar icon pressed (macOS's rounded highlight behind it) while
/// the panel is open, as menu bar menus are. macOS clears it when the mouse
/// button comes up, so it's set again after the click.
fn pressed(app: &AppHandle, on: bool) {
    let Some(tray) = app.tray_by_id("pim") else { return };
    let _ = tray.with_inner_tray_icon(move |inner| {
        let (Some(item), Some(mtm)) = (inner.ns_status_item(), objc2::MainThreadMarker::new()) else { return };
        if let Some(button) = item.button(mtm) {
            button.highlight(on);
        }
    });
}

/// The panel has closed: finished tasks have been seen, so they go, and the
/// panel goes back to its main view for next time.
pub fn closed(app: &AppHandle, shared: &Arc<Shared>) {
    pressed(app, false);
    shared.hidden_at.store(Utc::now().timestamp_millis(), Relaxed);
    let _ = app.emit_to("popover", "closed", ());
    let removed = {
        let mut guard = shared.snap.lock().unwrap();
        let snap = &mut *guard;
        // Into the history before they leave the list.
        if tasks::record(&mut snap.tasks, &mut snap.history)
            && let Some(path) = shared.history.get()
        {
            tasks::save_history(path, &snap.history);
        }
        let before = snap.tasks.len();
        snap.tasks.retain(Task::open);
        snap.tasks.len() != before
    };
    if removed {
        publish(app, shared);
    }
}

/// Centres the panel under the icon, kept on the screen.
pub fn place(panel: &WebviewWindow, icon: Rect) {
    let scale = panel.scale_factor().unwrap_or(2.0);
    let pos = icon.position.to_physical::<f64>(scale);
    let size = icon.size.to_physical::<f64>(scale);
    let width = PANEL.0 * scale;
    let mut x = pos.x + size.width / 2.0 - width / 2.0;
    if let Ok(Some(m)) = panel.monitor_from_point(pos.x, pos.y) {
        let right = (m.position().x + m.size().width as i32) as f64 - width - 8.0 * scale;
        x = x.min(right).max(m.position().x as f64 + 8.0 * scale);
    }
    let _ = panel.set_position(PhysicalPosition::new(x, pos.y + size.height + 4.0 * scale));
}
