//! Opening at login, through macOS's own Login Items (`SMAppService`), so
//! pimbar is listed in System Settings → General → Login Items like any app,
//! and can be switched off there too.

use objc2_service_management::{SMAppService, SMAppServiceStatus};
use serde::Serialize;

#[derive(Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Login {
    On,
    #[default]
    Off,
    /// Registered, but switched off in Login Items: only the user can turn it on there.
    Approval,
    /// Not possible from here, e.g. when run from `cargo run` rather than as an app.
    Unavailable,
}

pub fn status() -> Login {
    // SAFETY: plain calls into ServiceManagement with no arguments to uphold.
    let status = unsafe { SMAppService::mainAppService().status() };
    if status == SMAppServiceStatus::Enabled {
        Login::On
    } else if status == SMAppServiceStatus::RequiresApproval {
        Login::Approval
    } else if status == SMAppServiceStatus::NotRegistered {
        Login::Off
    } else {
        Login::Unavailable
    }
}

/// Turns opening at login on or off. If macOS wants the user to approve it,
/// opens Login Items so they can.
pub fn set(on: bool) -> Result<Login, String> {
    // SAFETY: as above.
    let service = unsafe { SMAppService::mainAppService() };
    let result = unsafe { if on { service.registerAndReturnError() } else { service.unregisterAndReturnError() } };
    result.map_err(|e| e.localizedDescription().to_string())?;
    let now = status();
    if on && now == Login::Approval {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
    Ok(now)
}
