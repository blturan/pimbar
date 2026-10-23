//! Keeping up with Azure: the polling loop, what each round loads, and the
//! daily look at GitHub for a newer pimbar.

use crate::state::{Shared, Snapshot};
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use tauri::AppHandle;
use crate::pim::{self, Pim};
use crate::policies::ensure_policies;
use crate::settings::Settings;
use crate::tasks::{self, Kind, Phase};
use crate::{login, update};
use crate::view::{notify, publish};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// While a task is running, Azure is checked this often.
const POLL_BUSY: Duration = Duration::from_secs(4);

/// Keeps the snapshot fresh, and tells the panel, the icon and the user what changed.
pub async fn poll(app: AppHandle, shared: Arc<Shared>) {
    let mut last_full: Option<Instant> = None;
    let mut seen_reviews: Option<HashSet<String>> = None;
    let mut warned_expiry: HashSet<String> = HashSet::new();

    loop {
        let full = shared.want_full.swap(false, Relaxed) || last_full.is_none_or(|t| t.elapsed() >= Duration::from_secs(60 * shared.roles_minutes.load(Relaxed)));
        if full {
            shared.snap.lock().unwrap().loading = true;
            publish(&app, &shared);
        }
        let (ok, notes) = load(&shared, full).await;
        if full && ok {
            last_full = Some(Instant::now());
        }
        ensure_policies(&app, &shared);
        check_update(&app, &shared, false);
        for (title, body) in notes {
            notify(&app, &title, &body);
        }
        // Time out what PIM never settled, even when Azure didn't answer this round.
        tasks::expire(&mut shared.snap.lock().unwrap().tasks);
        let snap = shared.snap.lock().unwrap().clone();

        // Each new request for my approval gets its own notification; the ones
        // already waiting at start get one between them.
        let ids: HashSet<String> = snap.to_review.iter().map(|r| r.id.clone()).collect();
        match &seen_reviews {
            Some(seen) => {
                for r in snap.to_review.iter().filter(|r| !seen.contains(&r.id)) {
                    notify(&app, "PIM approval needed", &format!("{} wants {} on {}", r.requester, r.role, r.scope_name));
                }
            }
            None if !snap.to_review.is_empty() => {
                let n = snap.to_review.len();
                notify(&app, "PIM approvals waiting", &format!("{n} request{} waiting for your approval", if n == 1 { " is" } else { "s are" }));
            }
            None => {}
        }
        if ok {
            seen_reviews = Some(ids);
        }

        for a in &snap.active {
            let soon = a.end.is_some_and(|end| (end - Utc::now()).num_minutes() <= 10);
            if soon && warned_expiry.insert(a.id.clone()) {
                notify(&app, "PIM role expiring", &format!("{} on {} ends in under 10 minutes", a.role, a.scope_name));
            }
        }
        warned_expiry.retain(|id| snap.active.iter().any(|a| a.id == *id));

        publish(&app, &shared);
        // A running task is followed closely; otherwise the set interval, or in
        // manual, nothing until the refresh button (or an action) wakes it.
        let busy = snap.tasks.iter().any(|t| t.phase == Phase::Running);
        let manual = shared.manual.load(Relaxed);
        let every = Duration::from_secs(60 * shared.sync_minutes.load(Relaxed));
        let wait = async {
            match (busy, manual) {
                (true, _) => tokio::time::sleep(POLL_BUSY).await,
                (false, true) => std::future::pending::<()>().await,
                (false, false) => tokio::time::sleep(every).await,
            }
        };
        tokio::select! {
            _ = wait => {}
            _ = shared.wake.notified() => {}
        }
    }
}

/// Fetches into the snapshot; `full` includes the eligible roles and subscription names.
/// Each part fails on its own, so one 403 doesn't blank the rest. Returns whether
/// anything loaded, and notifications from tasks that moved on.
pub async fn load(shared: &Shared, full: bool) -> (bool, Vec<(String, String)>) {
    // Whether macOS lets pimbar notify. The first time, macOS asks the user
    // ("pimbar would like to send you notifications"); after that it answers
    // from what they chose, which can change in System Settings at any time.
    if full {
        let allowed = notify_rust::request_auth().await.map_err(|e| eprintln!("pimbar: notifications: {e}")).ok();
        shared.snap.lock().unwrap().notifications = allowed;
    }
    let token = match shared.az.token().await {
        Ok(t) => t,
        Err(e) => {
            let mut snap = shared.snap.lock().unwrap();
            snap.errors = vec![e.to_string()];
            snap.loading = false;
            return (false, Vec::new());
        }
    };
    let p = Pim { http: &shared.http, token: &token.access };
    let (to_review, mine) = tokio::join!(p.to_review(), p.mine());
    let (eligible, subs) = if full {
        let (e, s) = tokio::join!(p.eligible(), p.subscriptions());
        (Some(e), Some(s))
    } else {
        (None, None)
    };

    let mut errors = Vec::new();
    let unauthorized = [to_review.as_ref().err(), mine.as_ref().err()]
        .into_iter()
        .chain([eligible.as_ref().and_then(|r| r.as_ref().err()), subs.as_ref().and_then(|r| r.as_ref().err())])
        .flatten()
        .any(pim::unauthorized);
    if unauthorized {
        shared.az.invalidate();
    }
    // Locked only here: what follows awaits.
    let decided: Vec<(u64, String)> = {
        let mut guard = shared.snap.lock().unwrap();
        let snap = &mut *guard;
        if let Some(v) = take(&mut errors, "Approvals", to_review) {
            snap.to_review = v;
        }
        if let Some(mine) = take(&mut errors, "Your roles", mine) {
            snap.active = mine.active;
            snap.my_pending = mine.pending;
            tasks::reconcile(&mut snap.tasks, &snap.active, &mine.latest);
        }
        if let Some(v) = eligible.and_then(|r| take(&mut errors, "Eligible roles", r)) {
            snap.eligible = v;
        }
        if let Some(v) = subs.and_then(|r| take(&mut errors, "Subscriptions", r)) {
            snap.subscriptions = v;
        }
        // Subscriptions you're only eligible on aren't listed; their roles carry the name.
        let names: Vec<_> = snap
            .eligible
            .iter()
            .filter(|e| e.scope_type == "subscription")
            .map(|e| (e.subscription_id.clone(), e.scope_name.clone()))
            .collect();
        for (id, name) in names {
            snap.subscriptions.entry(id).or_insert(name);
        }
        hide_handled(snap);

        // Can change in System Settings at any time.
        snap.login = login::status();
        snap.user = token.upn;
        snap.errors = errors;
        snap.refreshed_at = Some(Utc::now());
        snap.loading = false;
        snap.tasks
            .iter_mut()
            .filter(|t| t.decided)
            .map(|t| {
                t.decided = false;
                (t.id, t.approval_id.clone())
            })
            .collect()
    };

    // An approver decided on one of my requests: say who, and why, on the task and in a notification.
    let mut notes = Vec::new();
    for (id, approval) in decided {
        let decision = if approval.is_empty() { None } else { p.decision(&approval).await.ok().flatten() };
        let mut snap = shared.snap.lock().unwrap();
        let Some(t) = snap.tasks.iter_mut().find(|t| t.id == id) else { continue };
        // Requesters may not read the approval ("doesn't allow Self-Approval"): then the panel says where the reason is.
        if let Some(d) = decision.filter(|d| !d.by.is_empty()) {
            t.message = format!("{} by {}", t.message, d.by);
            if !d.justification.is_empty() {
                t.message = format!("{}: {}", t.message, d.justification);
            }
        }
        let title = if t.phase == Phase::Done { "PIM request approved" } else { "PIM request not approved" };
        notes.push((title.to_owned(), format!("{} on {}: {}", t.role, t.scope_name, t.message)));
    }
    (true, notes)
}

/// Requests a task already covers show as that task, not twice.
pub fn hide_handled(snap: &mut Snapshot) {
    let tasks = &snap.tasks;
    snap.to_review.retain(|r| !tasks.iter().any(|t| t.request_id == r.id && t.phase != Phase::Failed));
    snap.my_pending.retain(|r| !tasks.iter().any(|t| t.kind == Kind::Activate && t.open() && t.is_for(&r.scope, &r.role_def_id)));
}

/// The value, or None with the error noted under `label`.
pub fn take<T>(errors: &mut Vec<String>, label: &str, r: anyhow::Result<T>) -> Option<T> {
    r.map_err(|e| errors.push(format!("{label}: {e}"))).ok()
}

/// Asks GitHub for the latest release, once a day (or now, if `force`), in the
/// background; a newer one is announced once, by notification.
pub fn check_update(app: &AppHandle, shared: &Arc<Shared>, force: bool) {
    let due = force || shared.snap.lock().unwrap().update.checked_at.is_none_or(|t| Utc::now() - t >= update::CHECK_EVERY);
    if !due || shared.checking_update.swap(true, Relaxed) {
        return;
    }
    shared.snap.lock().unwrap().update.checking = true;
    publish(app, shared);
    let (app, shared) = (app.clone(), Arc::clone(shared));
    tauri::async_runtime::spawn(async move {
        let result = update::latest(&app).await;
        let current = app.package_info().version.to_string();
        let mut announce = None;
        {
            let mut snap = shared.snap.lock().unwrap();
            let u = &mut snap.update;
            u.checking = false;
            u.checked_at = Some(Utc::now());
            u.via = if update::via_homebrew() { "homebrew" } else { "self" }.into();
            match result {
                // Up to date, or no release with an update in it yet.
                Ok(None) => {
                    u.available = false;
                    u.latest = Some(current);
                    u.error = None;
                    *shared.release.lock().unwrap() = None;
                }
                Ok(Some(release)) => {
                    u.available = true;
                    u.latest = Some(release.version.clone());
                    u.error = None;
                    announce = Some(release.version.clone());
                    *shared.release.lock().unwrap() = Some(release);
                }
                Err(e) => u.error = Some(e.to_string()),
            }
        }
        if let (Some(version), Some(path)) = (announce, shared.settings.get()) {
            let new = Settings::edit(path, |s| {
                let new = s.update_notified != version;
                s.update_notified = version.clone();
                new
            });
            if new {
                notify(&app, "pimbar update", &format!("pimbar {version} is available: update it in Settings"));
            }
        }
        shared.checking_update.store(false, Relaxed);
        publish(&app, &shared);
    });
}
