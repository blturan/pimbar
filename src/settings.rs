//! What pimbar remembers between runs, in
//! `~/Library/Application Support/dev.pimbar.desktop/settings.json`.
//! Intervals are in seconds.

use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::Mutex;

/// How often pimbar may check requests and approvals.
pub const REQUESTS: RangeInclusive<u64> = 15..=3600;
/// How often pimbar may reload the eligible roles.
pub const ROLES: RangeInclusive<u64> = 60..=14400;

#[derive(Deserialize, Serialize)]
pub struct Settings {
    /// Asked once whether to open at login; never again, whatever the answer.
    #[serde(default)]
    pub asked_login: bool,
    /// How often to check requests and approvals (two small calls).
    #[serde(default = "default_sync")]
    pub sync_seconds: u64,
    /// How often to reload the eligible roles and subscription names (the two big calls).
    #[serde(default = "default_roles")]
    pub roles_seconds: u64,
    /// Both in minutes, as pimbar 0.2 saved them: read once, then saved as seconds.
    #[serde(default, skip_serializing)]
    sync_minutes: Option<u64>,
    #[serde(default, skip_serializing)]
    roles_minutes: Option<u64>,
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
    /// Subscriptions listed under Pinned, above the rest, by id.
    #[serde(default)]
    pub pinned: Vec<String>,
}

/// The panel's looks; `system` follows macOS.
pub const APPEARANCES: [&str; 3] = ["system", "light", "dark"];

fn default_appearance() -> String {
    "system".into()
}

fn default_sync() -> u64 {
    30
}

fn default_roles() -> u64 {
    4 * 3600
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            asked_login: false,
            sync_seconds: default_sync(),
            roles_seconds: default_roles(),
            sync_minutes: None,
            roles_minutes: None,
            manual: false,
            appearance: default_appearance(),
            update_notified: String::new(),
            hidden: Vec::new(),
            hidden_roles: Vec::new(),
            pinned: Vec::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut s: Settings = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if let Some(minutes) = s.sync_minutes.take() {
            s.sync_seconds = minutes * 60;
        }
        if let Some(minutes) = s.roles_minutes.take() {
            s.roles_seconds = minutes * 60;
        }
        if !REQUESTS.contains(&s.sync_seconds) {
            s.sync_seconds = default_sync();
        }
        if !ROLES.contains(&s.roles_seconds) {
            s.roles_seconds = default_roles();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_from_0_2_become_seconds() {
        let path = std::env::temp_dir().join(format!("pimbar-settings-{}.json", std::process::id()));
        std::fs::write(&path, r#"{ "sync_minutes": 5, "roles_minutes": 15 }"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!((s.sync_seconds, s.roles_seconds), (300, 900));
        Settings::edit(&path, |_| {});
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(r#""sync_seconds": 300"#) && saved.contains(r#""roles_seconds": 900"#));
        assert!(!saved.contains("_minutes"));
        std::fs::remove_file(&path).unwrap();
    }
}
