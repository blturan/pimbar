//! What pimbar remembers between runs, in
//! `~/Library/Application Support/dev.pimbar.desktop/settings.json`.

use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::Mutex;

/// How often pimbar may check requests and approvals, in minutes.
pub const REQUESTS: RangeInclusive<u64> = 1..=60;
/// How often pimbar may reload the eligible roles, in minutes.
pub const ROLES: RangeInclusive<u64> = 1..=240;

#[derive(Deserialize, Serialize)]
pub struct Settings {
    /// Asked once whether to open at login; never again, whatever the answer.
    #[serde(default)]
    pub asked_login: bool,
    /// How often to check requests and approvals (two small calls), in minutes.
    #[serde(default = "default_sync")]
    pub sync_minutes: u64,
    /// How often to reload the eligible roles and subscription names (the two big calls), in minutes.
    #[serde(default = "default_roles")]
    pub roles_minutes: u64,
    /// Only check when asked (the refresh button), and while a task runs.
    #[serde(default)]
    pub manual: bool,
    /// The panel's look: `system` (follow macOS), `light` or `dark`.
    #[serde(default = "default_appearance")]
    pub appearance: String,
    /// The release the user was last told about, so each is announced once.
    #[serde(default)]
    pub update_notified: String,
    /// Subscriptions left out of the eligible list, by id: access the user has but never uses.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Single roles left out of the eligible list, as `scope|role definition id`.
    #[serde(default)]
    pub hidden_roles: Vec<String>,
}

/// The panel's looks; `system` follows macOS.
pub const APPEARANCES: [&str; 3] = ["system", "light", "dark"];

fn default_appearance() -> String {
    "system".into()
}

fn default_sync() -> u64 {
    1
}

fn default_roles() -> u64 {
    15
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            asked_login: false,
            sync_minutes: default_sync(),
            roles_minutes: default_roles(),
            manual: false,
            appearance: default_appearance(),
            update_notified: String::new(),
            hidden: Vec::new(),
            hidden_roles: Vec::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut s: Settings = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if !REQUESTS.contains(&s.sync_minutes) {
            s.sync_minutes = default_sync();
        }
        if !ROLES.contains(&s.roles_minutes) {
            s.roles_minutes = default_roles();
        }
        if !APPEARANCES.contains(&s.appearance.as_str()) {
            s.appearance = default_appearance();
        }
        s
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Written aside and renamed over, so a crash never leaves half a file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Loads, changes and saves the settings as one step, so two changes at
    /// once (a command and the update check) can't undo each other.
    pub fn edit<R>(path: &Path, change: impl FnOnce(&mut Settings) -> R) -> R {
        static EDITING: Mutex<()> = Mutex::new(());
        let _guard = EDITING.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = Settings::load(path);
        let result = change(&mut settings);
        if let Err(e) = settings.save(path) {
            eprintln!("pimbar: couldn't save settings: {e}");
        }
        result
    }
}
