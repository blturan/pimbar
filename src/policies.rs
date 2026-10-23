//! Each eligible role's PIM policy (does activating need approval? how long
//! may it last?), cached on disk and refreshed daily, a few at a time.

use crate::state::Shared;
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use tauri::AppHandle;
use crate::pim::{self, Eligible, Pim, Policy};
use crate::view::publish;
use std::collections::HashMap;

/// Role policies hardly change: each is checked again after this long.
const POLICY_TTL: chrono::Duration = chrono::Duration::hours(24);
/// A lookup that failed (throttled, or not allowed on that scope) waits this long before the next try.
const POLICY_RETRY: chrono::Duration = chrono::Duration::minutes(15);
/// Policy lookups in flight at once.
const POLICY_BATCH: usize = 4;

pub fn policy_key(scope: &str, role_def_id: &str) -> String {
    format!("{scope}|{role_def_id}")
}

pub fn load_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// Copies what each eligible role's policy says onto it.
fn apply_policies(eligible: &mut [Eligible], cache: &HashMap<String, Policy>) {
    for e in eligible {
        if let Some(p) = cache.get(&policy_key(&e.scope, &e.role_def_id)) {
            e.approval = Some(p.approval);
            e.max_hours = p.max_hours;
        }
    }
}

/// Fills in each eligible role's policy (needs approval? longest activation?)
/// from the cache, and fetches the missing and day-old ones in the background,
/// a few at a time, colouring the panel's buttons as the answers come in.
/// Hidden roles aren't looked up at all.
pub fn ensure_policies(app: &AppHandle, shared: &Arc<Shared>) {
    let wanted: Vec<(String, String)> = {
        let cache = shared.policies.lock().unwrap();
        let failed = shared.policies_failed.lock().unwrap();
        let mut snap = shared.snap.lock().unwrap();
        apply_policies(&mut snap.eligible, &cache);
        let now = Utc::now();
        let mut wanted: Vec<_> = snap
            .eligible
            .iter()
            .filter(|e| {
                let key = policy_key(&e.scope, &e.role_def_id);
                !snap.hidden.contains(&e.subscription_id)
                    && !snap.hidden_roles.contains(&key)
                    && cache.get(&key).is_none_or(|p| now - p.fetched > POLICY_TTL)
                    && failed.get(&key).is_none_or(|t| now - *t > POLICY_RETRY)
            })
            .map(|e| (e.scope.clone(), e.role_def_id.clone()))
            .collect();
        wanted.sort();
        wanted.dedup();
        wanted
    };
    if wanted.is_empty() || shared.fetching_policies.swap(true, Relaxed) {
        return;
    }
    let (app, shared) = (app.clone(), Arc::clone(shared));
    tauri::async_runtime::spawn(async move {
        if let Ok(token) = shared.az.token().await {
            for (i, batch) in wanted.chunks(POLICY_BATCH).enumerate() {
                let lookups: Vec<_> = batch
                    .iter()
                    .cloned()
                    .map(|(scope, role)| {
                        let (http, token) = (shared.http.clone(), token.access.clone());
                        tauri::async_runtime::spawn(async move {
                            let policy = Pim { http: &http, token: &token }.policy(&scope, &role).await;
                            (policy_key(&scope, &role), policy)
                        })
                    })
                    .collect();
                let mut throttled = false;
                for lookup in lookups {
                    let Ok((key, policy)) = lookup.await else { continue };
                    match policy {
                        Ok(policy) => {
                            shared.policies_failed.lock().unwrap().remove(&key);
                            shared.policies.lock().unwrap().insert(key, policy);
                        }
                        Err(e) => {
                            throttled |= pim::throttled(&e);
                            shared.policies_failed.lock().unwrap().insert(key, Utc::now());
                        }
                    }
                }
                {
                    let cache = shared.policies.lock().unwrap();
                    apply_policies(&mut shared.snap.lock().unwrap().eligible, &cache);
                }
                publish(&app, &shared);
                // Azure wants us to slow down: the rest wait for a later round,
                // rather than eat into what the panel itself needs.
                if throttled {
                    let mut failed = shared.policies_failed.lock().unwrap();
                    for (scope, role) in wanted.iter().skip((i + 1) * POLICY_BATCH) {
                        failed.insert(policy_key(scope, role), Utc::now());
                    }
                    break;
                }
            }
            if let Some(path) = shared.policies_path.get() {
                let _ = std::fs::write(path, serde_json::to_vec(&*shared.policies.lock().unwrap()).unwrap_or_default());
            }
        }
        shared.fetching_policies.store(false, Relaxed);
    });
}
