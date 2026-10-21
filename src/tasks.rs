//! Background tasks. Activating, deactivating and reviewing run here, so the
//! panel answers at once; PIM itself can take from seconds (provisioning) to
//! hours (an approver). The panel lists tasks while they run and wait; once
//! finished, a task stays until you've seen it (the panel closes), or for a
//! while if you never look, and goes into the history for good.

use crate::pim::{Active, Request};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Gives up on a task PIM hasn't settled this long after it was sent; the request may still go through.
const STUCK: i64 = 5;
/// Done tasks go after this long even unseen; failed ones wait to be seen.
const KEEP_DONE: i64 = 10;
/// How many finished tasks the history keeps.
const HISTORY: usize = 50;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Sending, or PIM setting up or removing access.
    Running,
    /// Waiting on an approver.
    Waiting,
    Done,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Activate,
    Deactivate,
    Approve,
    Deny,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub kind: Kind,
    pub role: String,
    pub scope_name: String,
    /// The role it changes.
    pub scope: String,
    pub role_def_id: String,
    #[serde(default)]
    pub requester: String,
    /// The request: the one a review is for, or an activation's own (to cancel it by).
    #[serde(default)]
    pub request_id: String,
    /// For an activation an approver has to decide on: their approval.
    #[serde(default)]
    pub approval_id: String,
    /// The reason the user gave: for an activation, or for their review.
    #[serde(default)]
    pub justification: String,
    pub phase: Phase,
    pub message: String,
    /// For an activation that went through: when it ends.
    pub until: Option<DateTime<Utc>>,
    pub started: DateTime<Utc>,
    pub finished: Option<DateTime<Utc>>,
    /// PIM has the request; until then the polls can't tell us anything about it.
    #[serde(skip)]
    pub sent: bool,
    /// Since when PIM has been working on it: sent, or approved after a wait.
    /// `expire` times out from here, so an approver's time doesn't count.
    #[serde(skip)]
    pub sent_at: Option<DateTime<Utc>>,
    /// An approver just decided: worth finding out who, and telling the user.
    #[serde(skip)]
    pub decided: bool,
    /// Already in the history.
    #[serde(skip)]
    pub logged: bool,
}

impl Task {
    pub fn new(kind: Kind, role: &str, scope_name: &str, scope: &str, role_def_id: &str) -> Self {
        Task {
            id: 0,
            kind,
            role: role.into(),
            scope_name: scope_name.into(),
            scope: scope.into(),
            role_def_id: role_def_id.into(),
            requester: String::new(),
            request_id: String::new(),
            approval_id: String::new(),
            justification: String::new(),
            phase: Phase::Running,
            message: "Sending request…".into(),
            until: None,
            started: Utc::now(),
            finished: None,
            sent: false,
            sent_at: None,
            decided: false,
            logged: false,
        }
    }

    pub fn open(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Waiting)
    }

    pub fn is_for(&self, scope: &str, role_def_id: &str) -> bool {
        self.scope == scope && self.role_def_id == role_def_id
    }

    pub fn finish(&mut self, phase: Phase, message: impl Into<String>) {
        self.phase = phase;
        self.message = message.into();
        self.finished = Some(Utc::now());
    }

    /// Cancelled, here or elsewhere: it ended, but nothing went wrong.
    pub fn cancelled(&self) -> bool {
        self.phase == Phase::Failed && self.message.starts_with("Cancelled")
    }

    pub fn title(&self) -> String {
        let verb = match self.kind {
            Kind::Activate => "Activate",
            Kind::Deactivate => "Stop",
            Kind::Approve => "Approve",
            Kind::Deny => "Deny",
        };
        format!("{verb} {}", self.role)
    }
}

/// Moves tasks on from what the latest poll saw: my active roles, and the
/// latest activation request for each role, which says what became of it.
/// Tasks an approver just decided on are marked `decided`.
pub fn reconcile(tasks: &mut Vec<Task>, active: &[Active], latest: &[Request]) {
    let now = Utc::now();
    for t in tasks.iter_mut().filter(|t| t.open() && t.sent) {
        let was_waiting = t.phase == Phase::Waiting;
        match t.kind {
            Kind::Activate => {
                if let Some(a) = active.iter().find(|a| t.is_for(&a.scope, &a.role_def_id)) {
                    t.until = a.end;
                    t.finish(Phase::Done, if was_waiting { "Approved" } else { "Active" });
                    t.decided = was_waiting;
                    continue;
                }
                // This task's request: the latest for its role, not one from before it.
                let request = latest
                    .iter()
                    .find(|r| t.is_for(&r.scope, &r.role_def_id) && r.created.is_none_or(|c| c >= t.started - Duration::minutes(2)));
                let Some(r) = request else { continue };
                if !r.approval_id.is_empty() {
                    t.approval_id = r.approval_id.clone();
                }
                match r.status.as_str() {
                    "PendingApproval" | "PendingAdminDecision" => {
                        t.phase = Phase::Waiting;
                        t.message = "Pending".into();
                    }
                    "Denied" | "AdminDenied" => {
                        t.finish(Phase::Failed, "Denied");
                        t.decided = true;
                    }
                    "Canceled" => t.finish(Phase::Failed, "Cancelled"),
                    "TimedOut" => {
                        t.finish(Phase::Failed, "Expired: nobody approved it in time");
                        t.decided = true;
                    }
                    "Failed" | "FailedAsResourceIsLocked" | "Invalid" => {
                        t.finish(Phase::Failed, format!("PIM couldn't do it ({})", r.status));
                    }
                    // Approved or provisioning: active any moment now.
                    _ => {
                        if was_waiting {
                            t.sent_at = Some(now);
                        }
                        t.phase = Phase::Running;
                        t.message = if was_waiting { "Approved, setting up access…" } else { "Setting up access…" }.into();
                    }
                }
            }
            Kind::Deactivate => {
                if !active.iter().any(|a| t.is_for(&a.scope, &a.role_def_id)) {
                    t.finish(Phase::Done, "Access removed");
                }
            }
            Kind::Approve | Kind::Deny => {}
        }
    }
    tasks.retain(|t| t.phase != Phase::Done || t.finished.is_none_or(|f| now - f < Duration::minutes(KEEP_DONE)));
}

/// Stops tasks PIM hasn't settled within `STUCK` minutes of being sent, so
/// nothing spins forever. Runs every cycle, whether or not Azure answered;
/// waiting on an approver isn't timed out here (PIM expires those itself).
/// True if anything changed.
pub fn expire(tasks: &mut [Task]) -> bool {
    let now = Utc::now();
    let mut changed = false;
    for t in tasks.iter_mut().filter(|t| t.phase == Phase::Running && t.sent) {
        if t.sent_at.is_some_and(|at| now - at > Duration::minutes(STUCK)) {
            let message = match t.kind {
                Kind::Deactivate => "Couldn't confirm it was removed; check the Azure portal",
                _ => "PIM hasn't confirmed it yet; check the Azure portal",
            };
            t.finish(Phase::Failed, message);
            changed = true;
        }
    }
    changed
}

/// Starts cancelling my request `id`: its waiting activation, or, for a
/// request found in Azure, a new task for it (with the id `next` gives), so
/// the panel shows how it goes. Until `end_cancel`, the polls leave it alone.
/// Returns the task's id and whether it's new; None if `id` isn't waiting.
pub fn begin_cancel(tasks: &mut Vec<Task>, pending: &[Request], id: &str, next: impl FnOnce() -> u64) -> Option<(u64, bool)> {
    let waiting = |t: &Task| t.kind == Kind::Activate && t.phase == Phase::Waiting && t.request_id == id;
    let (index, adopted) = match tasks.iter().position(waiting) {
        Some(i) => (i, false),
        None => {
            let r = pending.iter().find(|r| r.id == id)?;
            let mut t = Task::new(Kind::Activate, &r.role, &r.scope_name, &r.scope, &r.role_def_id);
            t.id = next();
            t.request_id = r.id.clone();
            t.approval_id = r.approval_id.clone();
            t.justification = r.justification.clone();
            t.started = r.created.unwrap_or(t.started);
            tasks.insert(0, t);
            (0, true)
        }
    };
    let t = &mut tasks[index];
    t.sent = false;
    t.phase = Phase::Running;
    t.message = "Cancelling…".into();
    Some((t.id, adopted))
}

/// Ends what `begin_cancel` started. Cancelled, the task ends so; if Azure
/// said no, the request is still waiting: back to Pending, or, for a task
/// made just for this, gone again.
pub fn end_cancel(tasks: &mut Vec<Task>, task: u64, adopted: bool, done: bool) {
    if !done && adopted {
        tasks.retain(|t| t.id != task);
        return;
    }
    let Some(t) = tasks.iter_mut().find(|t| t.id == task) else { return };
    t.sent = true;
    t.sent_at = Some(Utc::now());
    if done {
        t.finish(Phase::Failed, "Cancelled");
    } else {
        t.phase = Phase::Waiting;
        t.message = "Pending".into();
    }
}

/// Copies newly finished tasks into the history, newest first. True if it changed.
pub fn record(tasks: &mut [Task], history: &mut Vec<Task>) -> bool {
    let mut changed = false;
    for t in tasks.iter_mut().filter(|t| !t.open() && !t.logged) {
        t.logged = true;
        history.insert(0, t.clone());
        changed = true;
    }
    history.truncate(HISTORY);
    changed
}

pub fn load_history(path: &Path) -> Vec<Task> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_history(path: &Path, history: &[Task]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, serde_json::to_vec(history).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(status: &str, created: DateTime<Utc>) -> Request {
        Request {
            id: "request".into(),
            status: status.into(),
            scope: "/subscriptions/s".into(),
            role_def_id: "role".into(),
            requester: String::new(),
            role: "Reader".into(),
            scope_name: "S".into(),
            subscription_id: "s".into(),
            justification: String::new(),
            duration: "PT1H".into(),
            created: Some(created),
            approval_id: "approval".into(),
        }
    }

    /// Sent half an hour ago, and waiting on an approver since.
    fn waited_half_an_hour() -> Task {
        let then = Utc::now() - Duration::minutes(30);
        let mut t = Task::new(Kind::Activate, "Reader", "S", "/subscriptions/s", "role");
        t.started = then;
        t.sent = true;
        t.sent_at = Some(then);
        t.phase = Phase::Waiting;
        t
    }

    #[test]
    fn time_waiting_on_an_approver_does_not_count_towards_stuck() {
        let mut tasks = vec![waited_half_an_hour()];
        let approved = request("Granted", tasks[0].started);
        reconcile(&mut tasks, &[], &[approved]);
        assert!(tasks[0].phase == Phase::Running);
        assert!(!expire(&mut tasks));
        assert!(tasks[0].phase == Phase::Running);
    }

    #[test]
    fn still_waiting_on_an_approver_is_never_stuck() {
        let mut tasks = vec![waited_half_an_hour()];
        let pending = request("PendingApproval", tasks[0].started);
        reconcile(&mut tasks, &[], &[pending]);
        assert!(!expire(&mut tasks));
        assert!(tasks[0].phase == Phase::Waiting);
    }

    #[test]
    fn running_too_long_is_stuck() {
        let mut t = waited_half_an_hour();
        t.phase = Phase::Running;
        let mut tasks = vec![t];
        assert!(expire(&mut tasks));
        assert!(tasks[0].phase == Phase::Failed);
    }

    #[test]
    fn cancels_a_waiting_activation() {
        let mut t = waited_half_an_hour();
        t.request_id = "request".into();
        let mut tasks = vec![t];
        let (id, adopted) = begin_cancel(&mut tasks, &[], "request", || unreachable!()).unwrap();
        assert!(!adopted);
        assert!(tasks[0].phase == Phase::Running && !tasks[0].sent);
        // In flight: a poll that still sees it pending doesn't touch it.
        let still_pending = request("PendingApproval", tasks[0].started);
        reconcile(&mut tasks, &[], &[still_pending]);
        assert!(tasks[0].phase == Phase::Running);
        end_cancel(&mut tasks, id, adopted, true);
        assert!(tasks[0].cancelled() && tasks[0].finished.is_some());
    }

    #[test]
    fn a_refused_cancel_leaves_it_pending() {
        let mut t = waited_half_an_hour();
        t.request_id = "request".into();
        let mut tasks = vec![t];
        let (id, adopted) = begin_cancel(&mut tasks, &[], "request", || unreachable!()).unwrap();
        end_cancel(&mut tasks, id, adopted, false);
        assert!(tasks[0].phase == Phase::Waiting && tasks[0].sent);
        assert!(!tasks[0].cancelled());
    }

    #[test]
    fn cancels_a_request_found_in_azure_as_a_new_task() {
        let created = Utc::now() - Duration::hours(1);
        let found = request("PendingApproval", created);
        let mut tasks = Vec::new();
        let (id, adopted) = begin_cancel(&mut tasks, std::slice::from_ref(&found), "request", || 7).unwrap();
        assert!(adopted && id == 7);
        assert!(tasks[0].request_id == "request" && tasks[0].started == created);
        end_cancel(&mut tasks, id, adopted, true);
        assert!(tasks[0].cancelled());

        // Refused: the task made for it goes again.
        let mut tasks = Vec::new();
        let (id, adopted) = begin_cancel(&mut tasks, std::slice::from_ref(&found), "request", || 8).unwrap();
        end_cancel(&mut tasks, id, adopted, false);
        assert!(tasks.is_empty());
    }

    #[test]
    fn only_waiting_requests_can_be_cancelled() {
        let mut t = waited_half_an_hour();
        t.request_id = "request".into();
        t.phase = Phase::Running;
        let mut tasks = vec![t];
        assert!(begin_cancel(&mut tasks, &[], "request", || 1).is_none());
        assert!(begin_cancel(&mut tasks, &[], "unknown", || 1).is_none());
    }
}
