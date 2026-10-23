//! Updates, with Tauri's updater. Once a day pimbar reads `latest.json` from
//! GitHub's latest release (a plain download, not the API, which allows 60
//! calls an hour per IP address that a shared office or VPN address runs out
//! of). When it lists a newer version, the panel says so and offers to update:
//! through Homebrew when Homebrew installed it (so Homebrew keeps track), or
//! with the updater otherwise, which only installs an update signed with the
//! key whose public half is in `tauri.conf.json`.
//!
//! Homebrew's upgrade runs in a small detached shell script, because the app
//! can't be replaced while running: pimbar starts it and quits.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

pub use tauri_plugin_updater::Update as Release;

/// How often to ask GitHub, at most.
pub const CHECK_EVERY: chrono::Duration = chrono::Duration::hours(24);

/// What the panel shows about updates.
#[derive(Clone, Default, Serialize)]
pub struct Update {
    /// The latest release, without the `v`; None until GitHub has answered.
    pub latest: Option<String>,
    pub available: bool,
    pub checked_at: Option<DateTime<Utc>>,
    /// `homebrew` or `self`: who installs the update.
    pub via: String,
    /// Asking GitHub right now.
    pub checking: bool,
    /// The update has started; pimbar is about to quit.
    pub updating: bool,
    pub error: Option<String>,
}

/// The latest release, when it's newer than this one.
pub async fn latest(app: &AppHandle) -> Result<Option<Release>> {
    app.updater()?.check().await.context("couldn't read the latest release from GitHub")
}

fn brew() -> Option<&'static str> {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"].into_iter().find(|p| Path::new(p).exists())
}

/// Homebrew installed this copy if it keeps pimbar in its Caskroom.
pub fn via_homebrew() -> bool {
    brew().is_some() && ["/opt/homebrew/Caskroom/pimbar", "/usr/local/Caskroom/pimbar"].iter().any(|p| Path::new(p).exists())
}

/// The running app's bundle, e.g. `/Applications/pimbar.app`.
pub fn app_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let app = exe.ancestors().nth(3).context("not running from an app bundle")?;
    if app.extension().is_none_or(|e| e != "app") {
        bail!("not running from an app bundle");
    }
    Ok(app.to_path_buf())
}

fn log_path() -> PathBuf {
    dirs_home().join("Library/Logs/pimbar-update.log")
}

fn dirs_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

/// Runs `script` in its own process group, so it outlives pimbar quitting: an
/// update, or a move to Applications, finishing after pimbar has quit. Its
/// output goes to `~/Library/Logs/pimbar-update.log`.
pub fn detach(script: &str, args: &[&str]) -> Result<()> {
    let log = std::fs::File::create(log_path())?;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("pimbar-update")
        .args(args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .context("couldn't start the script that finishes after pimbar quits")?;
    Ok(())
}

/// Starts `brew upgrade --cask pimbar` and opens pimbar again when it's done.
/// pimbar should quit right after, so Homebrew can swap the app.
pub fn upgrade_with_homebrew() -> Result<()> {
    let brew = brew().context("Homebrew not found")?;
    let app = app_path()?;
    detach(
        r#"sleep 1; "$1" upgrade --cask pimbar; open "$2""#,
        &[brew, &app.to_string_lossy()],
    )
}

/// Downloads the release, checks its signature and puts it in place of this
/// app. pimbar should restart right after.
pub async fn update_self(release: &Release) -> Result<()> {
    release.download_and_install(|_, _| {}, || {}).await.context("couldn't install the update")
}
