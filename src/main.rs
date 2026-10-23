//! pimbar: Azure PIM in the menu bar. A lock with what's active and what's waiting for
//! your approval, and a panel under it to activate roles and to approve or
//! deny other people's requests. It signs in with the Azure CLI's token, so
//! there is nothing to register in Entra.
//!
//! PIM has no webhooks (Graph change notifications don't cover it), so it
//! polls: requests and approvals every 30 seconds, every few seconds while a
//! task is running, and the eligible roles, which rarely change and are the
//! big call, every four hours. Both intervals can be changed in Settings, or
//! polling left to the refresh button (manual).
//! Actions answer at once and carry on as tasks (see `tasks`).
//!
//! Here: starting up (the menu bar icon, the panel, window events). The rest:
//! `state` what's shared, `sync` keeping up with Azure, `commands` what the
//! panel can ask, `view` what the user sees, `panel` the panel itself,
//! `policies` approval rules per role, and below those `pim` (the Azure API),
//! `tasks`, `icon`, `az`, `login`, `settings` and `update`.

mod az;
mod commands;
mod icon;
mod install;
mod login;
mod panel;
mod pim;
mod policies;
mod settings;
mod state;
mod sync;
mod tasks;
mod update;
mod view;

use panel::{PANEL, closed, toggle};
use settings::Settings;
use state::{Shared, Snapshot};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tauri::image::Image;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, Theme, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use policies::load_json;
use sync::poll;
use view::{badge, set_icon};

fn main() {
    fix_path();
    let shared = Shared::new();

    tauri::Builder::default()
        // Two copies would poll and notify twice.
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::refresh,
            commands::activate,
            commands::deactivate,
            commands::cancel,
            commands::cancel_request,
            commands::review,
            commands::dismiss,
            commands::set_login,
            commands::skip_login,
            commands::set_appearance,
            commands::set_hidden,
            commands::hide_menu,
            commands::check_for_update,
            commands::install_update,
            commands::move_to_applications,
            commands::set_sync,
            commands::test_notification,
            commands::open_notification_settings,
            commands::clear_history,
            commands::hide,
            commands::quit,
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let panel = WebviewWindowBuilder::new(app, "popover", WebviewUrl::App("index.html".into()))
                .title("pimbar")
                .inner_size(PANEL.0, PANEL.1)
                .resizable(false)
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                // See-through, so the page can round its own corners (rounded.css);
                // square styles fill it edge to edge and look as before.
                .transparent(true)
                .visible(false)
                .build()?;
            shared.dark.store(matches!(panel.theme(), Ok(Theme::Dark)), Relaxed);

            let (rgba, w, h) = icon::render(&badge(&shared, &Snapshot::default()));
            TrayIconBuilder::with_id("pim")
                .icon(Image::new_owned(rgba, w, h))
                .icon_as_template(false)
                // Either click opens the panel; quitting is in the panel, behind a question.
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left | MouseButton::Right, button_state: MouseButtonState::Up, rect, .. } = e {
                        toggle(tray.app_handle(), rect);
                    }
                })
                .build(app)?;

            // Open the panel by itself the first time, to ask about opening at
            // login, or to offer moving to Applications.
            let dir = app.path().app_config_dir()?;
            let settings = Settings::load(&dir.join("settings.json"));
            shared.sync_seconds.store(settings.sync_seconds, Relaxed);
            shared.roles_seconds.store(settings.roles_seconds, Relaxed);
            shared.manual.store(settings.manual, Relaxed);
            let _ = shared.settings.set(dir.join("settings.json"));
            let _ = shared.history.set(dir.join("history.json"));
            *shared.policies.lock().unwrap() = load_json(&dir.join("policies.json")).unwrap_or_default();
            let _ = shared.policies_path.set(dir.join("policies.json"));
            let ask = {
                let mut snap = shared.snap.lock().unwrap();
                snap.history = tasks::load_history(&dir.join("history.json"));
                let next = snap.history.iter().map(|t| t.id).max().unwrap_or(0) + 1;
                shared.next_task.store(next, Relaxed);
                snap.login = login::status();
                snap.version = app.package_info().version.to_string();
                snap.sync_seconds = settings.sync_seconds;
                snap.roles_seconds = settings.roles_seconds;
                snap.manual = settings.manual;
                snap.appearance = settings.appearance.clone();
                snap.hidden = settings.hidden.clone();
                snap.hidden_roles = settings.hidden_roles.clone();
                snap.ask_login = !settings.asked_login && snap.login == login::Login::Off;
                // From its installer, moving to Applications comes first: no
                // other question, and nothing from Azure until it has.
                if let Some(image) = install::check() {
                    let _ = shared.installer.set(image);
                    snap.misplaced = true;
                    snap.ask_login = false;
                    snap.loading = false;
                }
                snap.ask_login || snap.misplaced
            };
            let misplaced = shared.installer.get().is_some();
            if ask {
                let app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    // Give the icon a moment to take its place in the menu bar.
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    if let Some(rect) = app.tray_by_id("pim").and_then(|t| t.rect().ok().flatten()) {
                        toggle(&app, rect);
                    }
                });
            }

            if !misplaced {
                tauri::async_runtime::spawn(poll(app.handle().clone(), shared.clone()));
            }
            Ok(())
        })
        // The two-finger-click menu's Hide Subscription or Hide Role: `hide:<kind>:<key>`.
        .on_menu_event(|app, event| {
            if let Some((kind, key)) = event.id().as_ref().strip_prefix("hide:").and_then(|rest| rest.split_once(':')) {
                commands::hide_item(app, app.state::<Arc<Shared>>().inner(), kind, key, true);
            }
        })
        .on_window_event(|window, event| {
            let shared = window.state::<Arc<Shared>>();
            match event {
                // Like a menu: clicking anywhere else closes it.
                WindowEvent::Focused(false) => {
                    let _ = window.hide();
                    closed(window.app_handle(), &shared);
                }
                WindowEvent::ThemeChanged(theme) => {
                    shared.dark.store(*theme == Theme::Dark, Relaxed);
                    let b = badge(&shared, &shared.snap.lock().unwrap());
                    set_icon(window.app_handle(), &b);
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("pimbar could not start");
}

/// An app started from Finder doesn't get the shell's `PATH`, so it wouldn't
/// find `az`: take the login shell's.
#[cfg(target_os = "macos")]
fn fix_path() {
    const START: &str = "__PIMBAR_PATH_START__";
    const END: &str = "__PIMBAR_PATH_END__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let from_shell = Command::new(shell)
        .args(["-ilc", &format!("printf '{START}%s{END}' \"$PATH\"")])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| Some(text.split_once(START)?.1.split_once(END)?.0.to_string()));
    let usual = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
    let path = [from_shell.as_deref(), std::env::var("PATH").ok().as_deref(), Some(usual)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(":");
    // Before any other thread exists, so nothing reads the environment while it changes.
    unsafe { std::env::set_var("PATH", path) };
}

#[cfg(not(target_os = "macos"))]
fn fix_path() {}

