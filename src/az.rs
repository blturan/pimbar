//! Borrows an ARM access token from the Azure CLI (`az account get-access-token`).

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

#[derive(Clone)]
pub struct Token {
    pub access: String,
    /// Object id of the signed-in user, taken from the token's `oid` claim.
    pub oid: String,
    pub upn: String,
    expires_at: i64,
}

#[derive(Default)]
pub struct Az {
    cached: Mutex<Option<Token>>,
}

impl Az {
    /// A cached token, or a fresh one from `az` (which blocks, so this runs off the async threads).
    pub async fn token(&self) -> Result<Token> {
        let now = chrono::Utc::now().timestamp();
        if let Some(t) = self.cached.lock().unwrap().as_ref().filter(|t| t.expires_at - 300 > now) {
            return Ok(t.clone());
        }
        let t = tauri::async_runtime::spawn_blocking(fetch).await??;
        *self.cached.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    pub fn invalidate(&self) {
        *self.cached.lock().unwrap() = None;
    }
}

/// How long `az` may take. It can hang for good on a connection that died
/// under it (after sleep, or a change of network), and the sync waits on it.
const TIMEOUT: Duration = Duration::from_secs(60);

fn fetch() -> Result<Token> {
    let child = Command::new("az")
        .args(["account", "get-access-token", "--resource", "https://management.azure.com", "-o", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("Unable to run az. Is the Azure CLI installed?")?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(TIMEOUT) {
        Ok(out) => out.context("az failed")?,
        Err(_) => {
            // The whole group: az is a shell script that runs Python.
            let _ = Command::new("/bin/kill").args(["-KILL", "--", &format!("-{pid}")]).status();
            bail!("az didn't answer within {} seconds", TIMEOUT.as_secs());
        }
    };
    if !out.status.success() {
        // az's own first line, e.g. "ERROR: Please run 'az login' to setup account."
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("az failed");
        bail!("FAIL: {}", line.strip_prefix("ERROR:").map(str::trim).unwrap_or(line));
    }

    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "accessToken")]
        access_token: String,
        expires_on: Option<i64>,
    }
    let r: Resp = serde_json::from_slice(&out.stdout).context("unexpected `az` output")?;
    let claims = claims(&r.access_token).context("could not read the token")?;
    let claim = |name: &str| claims.get(name).and_then(|v| v.as_str()).map(str::to_owned);
    Ok(Token {
        oid: claim("oid").context("token has no `oid` claim")?,
        upn: claim("upn").or_else(|| claim("unique_name")).unwrap_or_default(),
        expires_at: r.expires_on.unwrap_or_else(|| chrono::Utc::now().timestamp() + 600),
        access: r.access_token,
    })
}

fn claims(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}
