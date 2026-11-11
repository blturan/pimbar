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

use panel::{closed, show, toggle};
use state::Shared;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tauri::{Manager, RunEvent, Theme, WindowEvent};
use sync::{poll, stay_awake};
use view::{badge, set_icon};

fn main() {
    az::fix_env();
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
            commands::deactivate_all,
            commands::cancel,
            commands::cancel_request,
            commands::review,
            commands::dismiss,
            commands::dismiss_finished,
            commands::set_login,
            commands::skip_login,
            commands::set_appearance,
            commands::set_hidden,
            commands::item_menu,
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
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            stay_awake();

            panel::create(app, &shared)?;

            shared.restore(&app.path().app_config_dir()?, &app.package_info().version.to_string());
            // Open the panel by itself the first time, to ask about opening at
            // login, or to offer moving to Applications.
            let ask = {
                let mut snap = shared.snap.lock().unwrap();
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
        // The two-finger-click menu: Hide Subscription or Hide Role (`hide:<kind>:<key>`),
        // and Pin or Unpin Subscription (`pin:<id>`, `unpin:<id>`).
        .on_menu_event(|app, event| {
            let shared = app.state::<Arc<Shared>>();
            let id = event.id().as_ref();
            if let Some((kind, key)) = id.strip_prefix("hide:").and_then(|rest| rest.split_once(':')) {
                commands::hide_item(app, shared.inner(), kind, key, true);
            } else if let Some(key) = id.strip_prefix("pin:") {
                commands::pin_item(app, shared.inner(), key, true);
            } else if let Some(key) = id.strip_prefix("unpin:") {
                commands::pin_item(app, shared.inner(), key, false);
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
        .build(tauri::generate_context!())
        .expect("pimbar could not start")
        .run(|app, event| {
            // macOS brings pimbar forward (a click on one of its notifications, or
            // opening it again from Finder): with nothing to show it would sit
            // there frontmost, so show the panel.
            if let RunEvent::Reopen { .. } = event {
                show(app);
            }
        });
}
