//! What the panel can ask of the app. Actions (activate, stop, review) answer
//! at once and carry on as tasks; the rest change settings or the panel.

use crate::state::{Ctx, Shared, Snapshot};
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use tauri::{AppHandle, Manager};
use crate::panel::closed;
use crate::policies::ensure_policies;
use crate::pim::Pim;
use crate::settings::{self, Settings};
use crate::sync::{check_update, hide_handled};
use crate::tasks::{self, Kind, Phase, Task};
use crate::view::{notify, publish};
use crate::{install, login, pim, update};
use chrono::Local;
use std::future::Future;
use std::process::Command;
use std::time::Duration;
use tauri::WebviewWindow;
use tauri::menu::{Menu, MenuItem};

/// The activation lengths offered, in hours.
const HOURS: [u32; 4] = [1, 2, 4, 8];
/// PIM refuses to deactivate a role this soon after activating it (and says so
/// as either `ActiveDurationTooShort` or, misleadingly, `RoleAssignmentDoesNotExist`).
const MIN_ACTIVE: chrono::Duration = chrono::Duration::seconds(5 * 60 + 10);

/// Refreshes now, and again shortly: a new activation is `Granted` for a few seconds before it's `Provisioned`.
fn settle(shared: &Arc<Shared>) {
    shared.wake.notify_one();
    let shared = Arc::clone(shared);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        shared.wake.notify_one();
    });
}

/// Lists `task` and runs `job` for it in the background, so the command
/// returns at once. The job says where the task stands once PIM has answered;
/// polling takes it from there. The job gets the task's id, to see whether
/// it's been cancelled; a cancelled task keeps its end, whatever the job says.
fn start<Fut>(app: &AppHandle, ctx: &Ctx, mut task: Task, job: impl FnOnce(Arc<Shared>, u64) -> Fut + Send + 'static)
where
    Fut: Future<Output = Result<(Phase, String), String>> + Send + 'static,
{
    let shared = Arc::clone(ctx.inner());
    let id = shared.next_task.fetch_add(1, Relaxed);
    task.id = id;
    {
        let mut snap = shared.snap.lock().unwrap();
        snap.tasks.insert(0, task);
        hide_handled(&mut snap);
    }
    publish(app, &shared);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let outcome = job(Arc::clone(&shared), id).await;
        let failed = {
            let mut snap = shared.snap.lock().unwrap();
            let Some(t) = snap.tasks.iter_mut().find(|t| t.id == id && t.open()) else { return };
            t.sent = true;
            t.sent_at = Some(Utc::now());
            match outcome {
                Ok((Phase::Done, message)) => {
                    t.finish(Phase::Done, message);
                    None
                }
                Ok((phase, message)) => {
                    t.phase = phase;
                    t.message = message;
                    None
                }
                Err(e) => {
                    t.finish(Phase::Failed, e.clone());
                    Some((format!("{} failed", t.title()), e))
                }
            }
        };
        if let Some((title, body)) = failed {
            notify(&app, &title, &body);
        }
        settle(&shared);
        publish(&app, &shared);
    });
}

/// Whether the user cancelled task `id` (or it's gone).
fn cancelled(shared: &Shared, id: u64) -> bool {
    !shared.snap.lock().unwrap().tasks.iter().any(|t| t.id == id && t.open())
}

fn justified(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() { Err("A justification is required.".into()) } else { Ok(text.to_owned()) }
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn snapshot(ctx: Ctx) -> Snapshot {
    ctx.snap.lock().unwrap().clone()
}

#[tauri::command]
pub fn refresh(ctx: Ctx) {
    ctx.want_full.store(true, Relaxed);
    ctx.wake.notify_one();
}

#[tauri::command]
pub fn activate(app: AppHandle, ctx: Ctx, id: String, hours: u32, justification: String) -> Result<(), String> {
    let justification = justified(&justification)?;
    if !HOURS.contains(&hours) {
        return Err(format!("{hours}h isn't one of the offered durations."));
    }
    let e = {
        let snap = ctx.snap.lock().unwrap();
        let e = snap.eligible.iter().find(|e| e.id == id).cloned().ok_or("That role is no longer eligible.")?;
        if snap.tasks.iter().any(|t| t.open() && t.is_for(&e.scope, &e.role_def_id)) {
            return Err("That role already has a request in progress.".into());
        }
        if let Some(max) = e.max_hours.filter(|max| hours > *max) {
            return Err(format!("This role can be active for at most {max}h."));
        }
        e
    };
    let mut task = Task::new(Kind::Activate, &e.role, &e.scope_name, &e.scope, &e.role_def_id);
    task.message = format!("Requesting {hours}h…");
    task.justification = justification.clone();
    start(&app, &ctx, task, move |s, id| async move {
        let token = s.az.token().await.map_err(error)?;
        let scheduled = Pim { http: &s.http, token: &token.access }.activate(&token.oid, &e, hours, &justification).await.map_err(error)?;
        // Kept, so it can be cancelled while it waits for an approver.
        if let Some(t) = s.snap.lock().unwrap().tasks.iter_mut().find(|t| t.id == id) {
            t.request_id = scheduled.id;
        }
        Ok(if scheduled.status == "PendingApproval" {
            (Phase::Waiting, "Pending".into())
        } else {
            (Phase::Running, "Setting up access…".into())
        })
    });
    Ok(())
}

#[tauri::command]
pub fn deactivate(app: AppHandle, ctx: Ctx, id: String) -> Result<(), String> {
    let a = {
        let snap = ctx.snap.lock().unwrap();
        let a = snap.active.iter().find(|a| a.id == id).cloned().ok_or("That role is no longer active.")?;
        if snap.tasks.iter().any(|t| t.open() && t.is_for(&a.scope, &a.role_def_id)) {
            return Err("That role already has a request in progress.".into());
        }
        a
    };
    // Too soon after activating: wait until PIM allows it, rather than fail.
    let earliest = a.start.map(|t| t + MIN_ACTIVE).filter(|t| *t > Utc::now());
    let mut task = Task::new(Kind::Deactivate, &a.role, &a.scope_name, &a.scope, &a.role_def_id);
    if let Some(t) = earliest {
        // Waiting, not busy, until it's sent; it can be cancelled until then.
        task.phase = Phase::Waiting;
        task.until = Some(t);
        task.message = format!("Stops at {}: PIM won't stop a role in its first 5 minutes", t.with_timezone(&Local).format("%H:%M"));
    }
    let handle = app.clone();
    start(&app, &ctx, task, move |s, id| async move {
        if let Some(wait) = earliest.and_then(|t| (t - Utc::now()).to_std().ok()) {
            tokio::time::sleep(wait).await;
        }
        // From here the stop is going out: it can't be undone any more, and
        // `cancel` sees that under the same lock.
        {
            let mut snap = s.snap.lock().unwrap();
            let Some(t) = snap.tasks.iter_mut().find(|t| t.id == id && t.open()) else {
                return Err("Cancelled".into());
            };
            t.phase = Phase::Running;
            t.message = "Sending request…".into();
        }
        publish(&handle, &s);
        // Around the five-minute mark PIM can still say no; give it a little longer.
        let mut tries = 0;
        loop {
            if cancelled(&s, id) {
                return Err("Cancelled".into());
            }
            let token = s.az.token().await.map_err(error)?;
            let result = Pim { http: &s.http, token: &token.access }.deactivate(&token.oid, &a).await;
            match result {
                Ok(_) => return Ok((Phase::Running, "Removing access…".into())),
                Err(e) if tries < 3 && matches!(pim::code(&e), "ActiveDurationTooShort" | "RoleAssignmentDoesNotExist") => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                Err(e) => return Err(error(e)),
            }
        }
    });
    Ok(())
}

/// Undoes a stop that's waiting for PIM's five minutes: the role stays active,
/// and the stop goes without a trace (it never happened).
#[tauri::command]
pub fn cancel(app: AppHandle, ctx: Ctx, id: u64) -> Result<(), String> {
    {
        let mut snap = ctx.snap.lock().unwrap();
        if !snap.tasks.iter().any(|t| t.id == id && t.kind == Kind::Deactivate && t.phase == Phase::Waiting && !t.sent) {
            return Err("That stop has already been sent.".into());
        }
        snap.tasks.retain(|t| t.id != id);
    }
    publish(&app, ctx.inner());
    Ok(())
}

/// Withdraws one of my requests while it waits for an approver: a task's, or
/// one found in Azure (after a restart, or made in the portal). Answers once
/// Azure has, so a refusal shows where the user asked.
#[tauri::command]
pub async fn cancel_request(app: AppHandle, ctx: Ctx<'_>, id: String) -> Result<(), String> {
    let (task, adopted) = {
        let mut guard = ctx.snap.lock().unwrap();
        let snap = &mut *guard;
        let next = || ctx.next_task.fetch_add(1, Relaxed);
        let started = tasks::begin_cancel(&mut snap.tasks, &snap.my_pending, &id, next).ok_or("That request is no longer waiting.")?;
        hide_handled(snap);
        started
    };
    publish(&app, ctx.inner());

    let result = async {
        let token = ctx.az.token().await?;
        Pim { http: &ctx.http, token: &token.access }.cancel(&id).await
    }
    .await;
    {
        let mut snap = ctx.snap.lock().unwrap();
        tasks::end_cancel(&mut snap.tasks, task, adopted, result.is_ok());
        if result.is_ok() {
            snap.my_pending.retain(|r| r.id != id);
        }
    }
    settle(ctx.inner());
    publish(&app, ctx.inner());
    result.map_err(error)
}

#[tauri::command]
pub fn review(app: AppHandle, ctx: Ctx, id: String, approve: bool, justification: String) -> Result<(), String> {
    let justification = justified(&justification)?;
    let r = ctx.snap.lock().unwrap().to_review.iter().find(|r| r.id == id).cloned().ok_or("That request is no longer waiting.")?;
    let mut task = Task::new(if approve { Kind::Approve } else { Kind::Deny }, &r.role, &r.scope_name, &r.scope, &r.role_def_id);
    task.requester = r.requester.clone();
    task.request_id = r.id.clone();
    task.justification = justification.clone();
    start(&app, &ctx, task, move |s, _| async move {
        let token = s.az.token().await.map_err(error)?;
        Pim { http: &s.http, token: &token.access }.review(&r, approve, &justification).await.map_err(error)?;
        Ok((Phase::Done, if approve { "Approved" } else { "Denied" }.into()))
    });
    Ok(())
}

/// Turns opening at login on or off (the first-run question or the panel's switch).
#[tauri::command]
pub fn set_login(app: AppHandle, ctx: Ctx, on: bool) -> Result<(), String> {
    let result = login::set(on);
    answered_login(&ctx);
    if let Ok(now) = result {
        ctx.snap.lock().unwrap().login = now;
    }
    publish(&app, ctx.inner());
    result.map(|_| ())
}

/// "Not now" to the first-run question.
#[tauri::command]
pub fn skip_login(app: AppHandle, ctx: Ctx) {
    answered_login(&ctx);
    publish(&app, ctx.inner());
}

fn answered_login(shared: &Shared) {
    shared.snap.lock().unwrap().ask_login = false;
    if let Some(path) = shared.settings.get() {
        Settings::edit(path, |s| s.asked_login = true);
    }
}

#[tauri::command]
pub fn check_for_update(app: AppHandle, ctx: Ctx) {
    check_update(&app, ctx.inner(), true);
}

/// Moves pimbar to Applications (it's running from its installer), opens that
/// copy and quits. Answers only if it couldn't.
#[tauri::command]
pub async fn move_to_applications(app: AppHandle, ctx: Ctx<'_>) -> Result<(), String> {
    let image = ctx.installer.get().cloned().flatten();
    tauri::async_runtime::spawn_blocking(move || install::move_to_applications(image.as_deref()))
        .await
        .map_err(error)?
        .map_err(error)?;
    app.exit(0);
    Ok(())
}

/// Installs the latest release: through Homebrew if it installed pimbar,
/// otherwise by itself. Either way pimbar quits and the new one opens.
#[tauri::command]
pub async fn install_update(app: AppHandle, ctx: Ctx<'_>) -> Result<(), String> {
    let release = ctx.release.lock().unwrap().clone().ok_or("Check for updates first.")?;
    ctx.snap.lock().unwrap().update.updating = true;
    publish(&app, ctx.inner());
    let homebrew = update::via_homebrew();
    let started = if homebrew {
        update::upgrade_with_homebrew()
    } else {
        update::update_self(&release).await
    };
    match started {
        // Homebrew swaps the app once pimbar has quit, and opens it again; the
        // updater has already swapped it, so pimbar restarts into the new one.
        Ok(()) if homebrew => {
            app.exit(0);
            Ok(())
        }
        Ok(()) => app.restart(),
        Err(e) => {
            {
                let mut snap = ctx.snap.lock().unwrap();
                snap.update.updating = false;
                snap.update.error = Some(e.to_string());
            }
            publish(&app, ctx.inner());
            Err(e.to_string())
        }
    }
}

/// Sets the panel's look. Only the panel: the menu bar icon keeps following the
/// menu bar, which macOS draws light or dark whatever pimbar prefers.
#[tauri::command]
pub fn set_appearance(app: AppHandle, ctx: Ctx, appearance: String) -> Result<(), String> {
    if !settings::APPEARANCES.contains(&appearance.as_str()) {
        return Err(format!("“{appearance}” isn't a look pimbar has."));
    }
    ctx.snap.lock().unwrap().appearance = appearance.clone();
    if let Some(path) = ctx.settings.get() {
        Settings::edit(path, |s| s.appearance = appearance);
    }
    publish(&app, ctx.inner());
    Ok(())
}

/// Hides a subscription or a single role (`kind`) from the eligible list, or
/// shows it again. Only the list: active roles, approvals and notifications
/// carry on as before.
#[tauri::command]
pub fn set_hidden(app: AppHandle, ctx: Ctx, kind: String, key: String, hidden: bool) {
    hide_item(&app, ctx.inner(), &kind, &key, hidden);
}

/// Puts `key` in `list`, or takes it out; the list as it then is.
fn mark(list: &mut Vec<String>, key: &str, on: bool) -> Vec<String> {
    list.retain(|k| k != key);
    if on {
        list.push(key.to_owned());
    }
    list.clone()
}

/// `kind` is `subscription` (key: its id) or `role` (key: `scope|role definition id`).
pub fn hide_item(app: &AppHandle, shared: &Arc<Shared>, kind: &str, key: &str, hidden: bool) {
    let list = {
        let mut snap = shared.snap.lock().unwrap();
        match kind {
            "subscription" => mark(&mut snap.hidden, key, hidden),
            "role" => mark(&mut snap.hidden_roles, key, hidden),
            _ => return,
        }
    };
    if let Some(path) = shared.settings.get() {
        Settings::edit(path, |s| if kind == "subscription" { s.hidden = list } else { s.hidden_roles = list });
    }
    // Hidden roles' policies aren't looked up: a role shown again may need its own.
    if !hidden {
        ensure_policies(app, shared);
    }
    publish(app, shared);
}

/// Pins a subscription (by id): it's listed under Pinned, above the rest. Or unpins it.
pub fn pin_item(app: &AppHandle, shared: &Arc<Shared>, key: &str, pinned: bool) {
    let list = mark(&mut shared.snap.lock().unwrap().pinned, key, pinned);
    if let Some(path) = shared.settings.get() {
        Settings::edit(path, |s| s.pinned = list);
    }
    publish(app, shared);
}

/// The menu a two-finger click on a subscription or role opens: Pin or Unpin
/// Subscription, and Hide. Its items' ids say what to do; main.rs's menu handler does it.
#[tauri::command]
pub fn item_menu(window: WebviewWindow, ctx: Ctx, kind: String, key: String) -> Result<(), String> {
    let hide = if kind == "role" { "Hide Role" } else { "Hide Subscription" };
    let hide = MenuItem::with_id(&window, format!("hide:{kind}:{key}"), hide, true, None::<&str>).map_err(error)?;
    let menu = Menu::with_items(&window, &[&hide]).map_err(error)?;
    if kind == "subscription" {
        let pinned = ctx.snap.lock().unwrap().pinned.contains(&key);
        let (id, label) = if pinned { ("unpin", "Unpin Subscription") } else { ("pin", "Pin Subscription") };
        let pin = MenuItem::with_id(&window, format!("{id}:{key}"), label, true, None::<&str>).map_err(error)?;
        menu.insert(&pin, 0).map_err(error)?;
    }
    window.popup_menu(&menu).map_err(error)
}

/// An interval as people say it: seconds under a minute, minutes from there.
fn span(seconds: u64) -> String {
    if seconds < 60 { format!("{seconds} seconds") } else { format!("{} minutes", seconds / 60) }
}

/// Sets how often pimbar checks requests and approvals, and reloads the eligible
/// roles (both in seconds), or that it only checks when asked; checks now.
#[tauri::command]
pub fn set_sync(app: AppHandle, ctx: Ctx, requests: u64, roles: u64, manual: bool) -> Result<(), String> {
    let (r, e) = (settings::REQUESTS, settings::ROLES);
    if !r.contains(&requests) {
        return Err(format!("Requests and approvals: {} to {}.", span(*r.start()), span(*r.end())));
    }
    if !e.contains(&roles) {
        return Err(format!("Eligible roles: {} to {}.", span(*e.start()), span(*e.end())));
    }
    ctx.sync_seconds.store(requests, Relaxed);
    ctx.roles_seconds.store(roles, Relaxed);
    ctx.manual.store(manual, Relaxed);
    {
        let mut snap = ctx.snap.lock().unwrap();
        snap.sync_seconds = requests;
        snap.roles_seconds = roles;
        snap.manual = manual;
    }
    if let Some(path) = ctx.settings.get() {
        Settings::edit(path, |s| {
            s.sync_seconds = requests;
            s.roles_seconds = roles;
            s.manual = manual;
        });
    }
    ctx.wake.notify_one();
    publish(&app, ctx.inner());
    Ok(())
}

/// Clears a finished task from the list now rather than when the panel closes;
/// it's in the history all the same.
#[tauri::command]
pub fn dismiss(app: AppHandle, ctx: Ctx, id: u64) {
    {
        let mut guard = ctx.snap.lock().unwrap();
        let snap = &mut *guard;
        if tasks::record(&mut snap.tasks, &mut snap.history)
            && let Some(path) = ctx.history.get()
        {
            tasks::save_history(path, &snap.history);
        }
        snap.tasks.retain(|t| t.id != id || t.open());
    }
    publish(&app, ctx.inner());
}

#[tauri::command]
pub fn clear_history(app: AppHandle, ctx: Ctx) {
    ctx.snap.lock().unwrap().history.clear();
    if let Some(path) = ctx.history.get() {
        tasks::save_history(path, &[]);
    }
    publish(&app, ctx.inner());
}

#[tauri::command]
pub fn test_notification(app: AppHandle) {
    notify(&app, "pimbar notifications work", "You'll see one like this when someone asks for your approval.");
}

#[tauri::command]
pub fn open_notification_settings() {
    let _ = Command::new("open").arg("x-apple.systempreferences:com.apple.Notifications-Settings.extension").spawn();
}

#[tauri::command]
pub fn hide(app: AppHandle, window: WebviewWindow) {
    let _ = window.hide();
    closed(&app, app.state::<Arc<Shared>>().inner());
}

#[tauri::command]
pub fn quit(app: AppHandle) {
    app.exit(0);
}
