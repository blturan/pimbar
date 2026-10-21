//! What the user sees: the snapshot sent to the panel, the menu bar icon (and
//! its spinner), and macOS notifications.

use crate::state::{Shared, Snapshot};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use tauri::{AppHandle, Manager};
use crate::icon;
use crate::tasks::{self, Phase};
use std::time::Duration;
use tauri::Emitter;
use tauri::image::Image;

/// The spinner's frame time: about 12 frames a second.
const FRAME: Duration = Duration::from_millis(80);

pub fn publish(app: &AppHandle, shared: &Arc<Shared>) {
    let snap = {
        let mut guard = shared.snap.lock().unwrap();
        let snap = &mut *guard;
        if tasks::record(&mut snap.tasks, &mut snap.history)
            && let Some(path) = shared.history.get()
        {
            tasks::save_history(path, &snap.history);
        }
        guard.clone()
    };
    let b = badge(shared, &snap);
    set_icon(app, &b);
    if b.running > 0 {
        animate(app, shared);
    }
    let _ = app.emit_to("popover", "state", &snap);
}

pub fn badge(shared: &Shared, snap: &Snapshot) -> icon::Badge {
    let count = |phase| snap.tasks.iter().filter(|t| t.phase == phase).count();
    icon::Badge {
        running: count(Phase::Running),
        // Waiting on someone: tasks from this run, and requests found in Azure
        // (after a restart, or made in the portal; never the same one twice,
        // see `hide_handled`).
        waiting: count(Phase::Waiting) + snap.my_pending.len(),
        active: snap.active.len(),
        pending: snap.to_review.len(),
        error: !snap.errors.is_empty() || snap.tasks.iter().any(|t| t.phase == Phase::Failed && !t.cancelled()),
        dark: shared.dark.load(Relaxed),
        frame: shared.frame.load(Relaxed),
    }
}

pub fn set_icon(app: &AppHandle, b: &icon::Badge) {
    let Some(tray) = app.tray_by_id("pim") else { return };
    let (rgba, w, h) = icon::render(b);
    let _ = tray.set_icon(Some(Image::new_owned(rgba, w, h)));
}

/// Turns the menu bar spinner while any task is running.
fn animate(app: &AppHandle, shared: &Arc<Shared>) {
    if shared.animating.swap(true, Relaxed) {
        return;
    }
    let (app, shared) = (app.clone(), Arc::clone(shared));
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(FRAME).await;
            shared.frame.fetch_add(1, Relaxed);
            let b = {
                let snap = shared.snap.lock().unwrap();
                let b = badge(&shared, &snap);
                // Stop under the lock, so a task added right now starts a new spinner.
                if b.running == 0 {
                    shared.animating.store(false, Relaxed);
                }
                b
            };
            set_icon(&app, &b);
            if b.running == 0 {
                return;
            }
        }
    });
}

/// Posts a macOS notification. Async all the way: the blocking route has to
/// catch the main thread idle, and quietly drops the notification when it
/// doesn't. A failure is kept, so Settings can say why nothing appeared.
pub fn notify(app: &AppHandle, title: &str, body: &str) {
    let (app, title, body) = (app.clone(), title.to_owned(), body.to_owned());
    tauri::async_runtime::spawn(async move {
        let result = notify_rust::Notification::new().summary(&title).body(&body).show_async().await;
        let error = result.err().map(|e| e.to_string());
        if let Some(e) = &error {
            eprintln!("pimbar: notification: {e}");
        }
        let shared = Arc::clone(app.state::<Arc<Shared>>().inner());
        let changed = {
            let mut snap = shared.snap.lock().unwrap();
            std::mem::replace(&mut snap.notify_error, error.clone()) != error
        };
        if changed {
            publish(&app, &shared);
        }
    });
}
