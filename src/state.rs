//! The app's state: what the panel is shown (`Snapshot`), and everything the
//! background work shares (`Shared`), handed to commands as `Ctx`.

use crate::pim::{Active, Eligible, Policy, Request};
use crate::tasks::Task;
use crate::{az, login, update};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::State;

/// What the panel shows; sent whole on every change.
#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub user: String,
    pub eligible: Vec<Eligible>,
    pub active: Vec<Active>,
    /// Requests waiting for *my* approval, less those I've just reviewed.
    pub to_review: Vec<Request>,
    /// My own requests waiting for someone else's, less those a task already shows.
    pub my_pending: Vec<Request>,
    /// Newest first.
    pub tasks: Vec<Task>,
    /// Finished tasks, kept on disk; newest first.
    pub history: Vec<Task>,
    /// Subscription id → name.
    pub subscriptions: BTreeMap<String, String>,
    pub errors: Vec<String>,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub loading: bool,
    /// Whether macOS lets pimbar notify; None until it has answered.
    pub notifications: Option<bool>,
    /// Why the last notification didn't go out, if it didn't.
    pub notify_error: Option<String>,
    pub login: login::Login,
    /// First run: ask whether to open at login.
    pub ask_login: bool,
    /// Running from its installer (or a hidden copy of a download): offer to
    /// move to Applications, and nothing else.
    pub misplaced: bool,
    pub version: String,
    /// How often pimbar checks requests and approvals, and reloads the eligible roles, in seconds.
    pub sync_seconds: u64,
    pub roles_seconds: u64,
    /// Only checks when asked, and while a task runs.
    pub manual: bool,
    /// The panel's look: `system`, `light` or `dark`.
    pub appearance: String,
    /// Subscriptions left out of the eligible list, by id.
    pub hidden: Vec<String>,
    /// Single roles left out of it, as `scope|role definition id`.
    pub hidden_roles: Vec<String>,
    pub update: update::Update,
}

pub struct Shared {
    pub http: reqwest::Client,
    pub az: az::Az,
    pub snap: Mutex<Snapshot>,
    pub wake: tokio::sync::Notify,
    pub want_full: AtomicBool,
    pub dark: AtomicBool,
    /// When the panel last hid, in ms: a click on the icon that closed it shouldn't reopen it.
    pub hidden_at: AtomicI64,
    pub next_task: AtomicU64,
    /// The spinner's frame, and whether something is turning it.
    pub frame: AtomicU32,
    pub animating: AtomicBool,
    pub settings: OnceLock<PathBuf>,
    pub history: OnceLock<PathBuf>,
    /// How often to poll when nothing is running, and to reload the eligible roles, in seconds.
    pub sync_seconds: AtomicU64,
    pub roles_seconds: AtomicU64,
    pub manual: AtomicBool,
    /// Each eligible role's PIM policy, by `policy_key`; kept on disk.
    pub policies: Mutex<HashMap<String, Policy>>,
    pub policies_path: OnceLock<PathBuf>,
    pub fetching_policies: AtomicBool,
    /// When each policy lookup that didn't answer last failed, by `policy_key`: not tried again for a while.
    pub policies_failed: Mutex<HashMap<String, DateTime<Utc>>>,
    /// The latest release GitHub told us about, for the Update button.
    pub release: Mutex<Option<update::Release>>,
    pub checking_update: AtomicBool,
    /// Set when pimbar runs from somewhere it can't stay: the installer to
    /// eject once it has moved to Applications, if it's on one.
    pub installer: OnceLock<Option<PathBuf>>,
}

pub type Ctx<'a> = State<'a, Arc<Shared>>;

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Shared {
            // PIM's writes can take a while to answer.
            http: reqwest::Client::builder().timeout(Duration::from_secs(120)).build().expect("http client"),
            az: az::Az::default(),
            snap: Mutex::new(Snapshot { loading: true, ..Default::default() }),
            wake: tokio::sync::Notify::new(),
            want_full: AtomicBool::new(false),
            dark: AtomicBool::new(false),
            hidden_at: AtomicI64::new(0),
            next_task: AtomicU64::new(1),
            frame: AtomicU32::new(0),
            animating: AtomicBool::new(false),
            settings: OnceLock::new(),
            history: OnceLock::new(),
            sync_seconds: AtomicU64::new(30),
            roles_seconds: AtomicU64::new(4 * 3600),
            manual: AtomicBool::new(false),
            policies: Mutex::new(HashMap::new()),
            policies_path: OnceLock::new(),
            fetching_policies: AtomicBool::new(false),
            policies_failed: Mutex::new(HashMap::new()),
            release: Mutex::new(None),
            checking_update: AtomicBool::new(false),
            installer: OnceLock::new(),
            })
    }
}
