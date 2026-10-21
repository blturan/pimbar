//! Updates. Once a day pimbar asks GitHub for the latest release of
//! `blturan/pimbar`; when it's newer, the panel says so and offers to update:
//! through Homebrew when Homebrew installed it (so Homebrew keeps track), or
//! by itself otherwise — download the .dmg, check it against the checksum
//! GitHub publishes, swap the app, start the new one.
//!
//! The swap runs in a small detached shell script, because the app can't
//! replace itself while running: pimbar starts it and quits.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const REPO: &str = "blturan/pimbar";
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
    /// The update has started; pimbar is about to quit.
    pub updating: bool,
    pub error: Option<String>,
}

/// The latest release: its version, and its .dmg with the checksum GitHub publishes.
#[derive(Clone)]
pub struct Release {
    pub version: String,
    pub dmg_url: Option<String>,
    pub dmg_sha256: Option<String>,
}

/// The latest release, or None while the repository has none.
pub async fn latest(http: &reqwest::Client) -> Result<Option<Release>> {
    let resp = http
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("User-Agent", "pimbar")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("couldn't reach GitHub")?;
    if resp.status() == 404 {
        return Ok(None);
    }
    let v: serde_json::Value = resp.error_for_status().context("GitHub said no")?.json().await?;
    let tag = v["tag_name"].as_str().context("release without a tag")?;
    let dmg = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("_aarch64.dmg")));
    Ok(Some(Release {
        version: tag.trim_start_matches('v').to_owned(),
        dmg_url: dmg.and_then(|a| a["browser_download_url"].as_str()).map(str::to_owned),
        dmg_sha256: dmg
            .and_then(|a| a["digest"].as_str())
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_owned),
    }))
}

/// Whether `latest` is a higher version than `current`, by semver: `0.2.0` is
/// newer than `0.2.0-rc.1`. Anything that isn't a version is never newer.
pub fn newer(latest: &str, current: &str) -> bool {
    match (semver::Version::parse(latest), semver::Version::parse(current)) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => false,
    }
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

/// Downloads the release's .dmg, checks it, and starts the swap: mount, move
/// the old app aside, copy the new one in (putting the old one back if that
/// fails), and open it. pimbar should quit right after.
pub async fn update_self(http: &reqwest::Client, release: &Release) -> Result<()> {
    let url = release.dmg_url.as_deref().context("the release has no .dmg")?;
    let want = release.dmg_sha256.as_deref().context("GitHub gave no checksum for the .dmg")?;
    let bytes = http.get(url).header("User-Agent", "pimbar").send().await?.error_for_status()?.bytes().await?;
    let got = sha256_hex(&bytes)?;
    if !got.eq_ignore_ascii_case(want) {
        bail!("the download doesn't match GitHub's checksum");
    }
    let dmg = std::env::temp_dir().join(format!("pimbar-{}.dmg", release.version));
    std::fs::write(&dmg, &bytes)?;
    let app = app_path()?;
    detach(
        r#"set -u
sleep 1
mnt=$(mktemp -d) || exit 1
hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$1" || exit 1
rm -rf "$2.old" && mv "$2" "$2.old" || { hdiutil detach "$mnt" -quiet; exit 1; }
if ditto "$mnt/pimbar.app" "$2"; then rm -rf "$2.old"; else rm -rf "$2"; mv "$2.old" "$2"; fi
hdiutil detach "$mnt" -quiet; rm -f "$1"
open "$2""#,
        &[&dmg.to_string_lossy(), &app.to_string_lossy()],
    )
}

/// SHA-256 with macOS's own `shasum`, rather than another crate for one hash.
fn sha256_hex(bytes: &[u8]) -> Result<String> {
    use std::io::Write;
    let mut child = Command::new("/usr/bin/shasum").args(["-a", "256"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    child.stdin.take().context("shasum")?.write_all(bytes)?;
    let out = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or_default().to_owned())
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn a_release_is_newer_than_its_prereleases() {
        assert!(newer("0.2.0", "0.2.0-rc.1"));
        assert!(!newer("0.2.0-rc.1", "0.2.0"));
        assert!(newer("0.2.0-rc.2", "0.2.0-rc.1"));
    }

    #[test]
    fn nonsense_is_never_newer() {
        assert!(!newer("latest", "0.1.0"));
        assert!(!newer("0.2.0", "dev"));
    }
}
