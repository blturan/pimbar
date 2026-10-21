//! Where pimbar runs from. Started from its installer (.dmg) or from wherever
//! macOS hid a downloaded copy (App Translocation), it would stop working the
//! moment that place went away (ejecting the installer freezes it), and Open
//! at login and updates would point at the wrong copy. So it offers to move
//! itself to Applications: copy, quit, and open the copy, which then ejects
//! the installer.

use crate::update::{app_path, detach};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Somewhere pimbar can't stay: a mounted disk image (one of `images`, their
/// mount points) or App Translocation's hidden copy. The disk image's mount
/// point, if it's one, so it can be ejected afterwards.
pub fn misplaced(app: &Path, images: &[PathBuf]) -> Option<Option<PathBuf>> {
    if app.components().any(|c| c.as_os_str() == "AppTranslocation") {
        return Some(None);
    }
    images.iter().find(|image| app.starts_with(image)).map(|image| Some(image.clone()))
}

/// Mount points of the disk images mounted now, from `hdiutil info`.
fn mounted_images() -> Vec<PathBuf> {
    let Ok(out) = Command::new("/usr/bin/hdiutil").arg("info").output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.rsplit('\t').next())
        .filter(|field| field.starts_with("/Volumes/"))
        .map(PathBuf::from)
        .collect()
}

/// Whether the running app needs moving, and the disk image to eject after.
pub fn check() -> Option<Option<PathBuf>> {
    let app = app_path().ok()?;
    misplaced(&app, &mounted_images())
}

/// Where to move to: /Applications, or ~/Applications where it isn't writable
/// (a Mac without admin rights).
fn destination(home: &Path, system_writable: bool) -> PathBuf {
    let dir = if system_writable { PathBuf::from("/Applications") } else { home.join("Applications") };
    dir.join("pimbar.app")
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(".pimbar-write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(probe);
    ok
}

/// Copies the running app to Applications (replacing an older pimbar there),
/// then starts a small detached script that, once this copy has quit, opens
/// the new one and ejects `image`. The caller quits right after. Blocking.
pub fn move_to_applications(image: Option<&Path>) -> Result<PathBuf> {
    let app = app_path()?;
    let home = PathBuf::from(std::env::var("HOME").context("no home folder")?);
    let dest = destination(&home, writable(Path::new("/Applications")));
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    }
    // Copied beside it first, so a failed copy never leaves a broken app.
    let staged = dest.with_extension("app.new");
    let _ = std::fs::remove_dir_all(&staged);
    let copied = Command::new("/usr/bin/ditto").arg(&app).arg(&staged).status().context("couldn't run ditto")?;
    if !copied.success() {
        let _ = std::fs::remove_dir_all(&staged);
        bail!("couldn't copy pimbar to {}", dest.parent().unwrap_or(&dest).display());
    }
    // The user already chose to open pimbar; the copy shouldn't make macOS ask again.
    let _ = Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"]).arg(&staged).status();
    if dest.exists() {
        std::fs::remove_dir_all(&dest).with_context(|| format!("couldn't replace {}", dest.display()))?;
    }
    std::fs::rename(&staged, &dest).with_context(|| format!("couldn't move pimbar to {}", dest.display()))?;

    let image = image.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    detach(
        r#"sleep 1; open "$1"; [ -n "$2" ] && sleep 2 && hdiutil detach "$2" -quiet"#,
        &[&dest.to_string_lossy(), &image],
    )?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn images() -> Vec<PathBuf> {
        vec![PathBuf::from("/Volumes/pimbar"), PathBuf::from("/Volumes/Other Disk")]
    }

    #[test]
    fn in_applications_it_stays() {
        assert!(misplaced(Path::new("/Applications/pimbar.app"), &images()).is_none());
        assert!(misplaced(Path::new("/Users/me/Applications/pimbar.app"), &images()).is_none());
    }

    #[test]
    fn on_its_installer_it_moves_and_ejects_it() {
        let found = misplaced(Path::new("/Volumes/pimbar/pimbar.app"), &images());
        assert_eq!(found, Some(Some(PathBuf::from("/Volumes/pimbar"))));
    }

    #[test]
    fn on_an_external_disk_it_stays() {
        assert!(misplaced(Path::new("/Volumes/Backup/Applications/pimbar.app"), &images()).is_none());
        // Not fooled by a name that only starts the same.
        assert!(misplaced(Path::new("/Volumes/pimbar 2/pimbar.app"), &images()).is_none());
    }

    #[test]
    fn translocated_it_moves_without_ejecting() {
        let app = Path::new("/private/var/folders/xy/T/AppTranslocation/1234-ABCD/d/pimbar.app");
        assert_eq!(misplaced(app, &images()), Some(None));
    }

    #[test]
    fn without_admin_rights_it_goes_to_your_own_applications() {
        let home = Path::new("/Users/me");
        assert_eq!(destination(home, true), PathBuf::from("/Applications/pimbar.app"));
        assert_eq!(destination(home, false), PathBuf::from("/Users/me/Applications/pimbar.app"));
    }
}
