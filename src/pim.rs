//! PIM for Azure resource roles via the ARM `Microsoft.Authorization` API.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const ARM: &str = "https://management.azure.com";
const API: &str = "2020-10-01";
const API_APPROVALS: &str = "2021-01-01-preview";
const API_SUBSCRIPTIONS: &str = "2022-12-01";
/// How often a throttled (429) request is sent again, and the longest wait between, in seconds.
const THROTTLE_RETRIES: u32 = 2;
const MAX_RETRY_AFTER: u64 = 10;

#[derive(Clone, Serialize)]
pub struct Eligible {
    pub id: String,
    pub role: String,
    pub scope: String,
    pub scope_name: String,
    /// `subscription`, `resourcegroup`, `managementgroup`, …
    pub scope_type: String,
    /// Empty for scopes above a subscription.
    pub subscription_id: String,
    pub role_def_id: String,
    pub eligibility_schedule_id: String,
    /// From the role's PIM policy, once known: whether activating needs an approver.
    pub approval: Option<bool>,
    /// From the role's PIM policy, once known: the longest activation allowed, in hours.
    pub max_hours: Option<u32>,
}

/// What a role's PIM policy says about activating it.
#[derive(Clone, Serialize, Deserialize)]
pub struct Policy {
    pub approval: bool,
    pub max_hours: Option<u32>,
    pub fetched: DateTime<Utc>,
}

#[derive(Clone, Serialize)]
pub struct Active {
    pub id: String,
    pub start: Option<DateTime<Utc>>,
    pub role: String,
    pub scope: String,
    pub scope_name: String,
    pub subscription_id: String,
    pub role_def_id: String,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Clone, Serialize)]
pub struct Request {
    pub id: String,
    /// `PendingApproval`, `Provisioned`, `Denied`, `Canceled`, `TimedOut`, …
    pub status: String,
    pub scope: String,
    pub role_def_id: String,
    pub requester: String,
    pub role: String,
    pub scope_name: String,
    pub subscription_id: String,
    pub justification: String,
    pub duration: String,
    pub created: Option<DateTime<Utc>>,
    pub approval_id: String,
}

/// My requests, as `mine` sorts them out.
pub struct Mine {
    pub active: Vec<Active>,
    /// Waiting for someone's approval.
    pub pending: Vec<Request>,
    /// The latest activation request for each role, whatever became of it.
    pub latest: Vec<Request>,
}

/// Who decided on a request, and why.
pub struct Decision {
    pub by: String,
    pub justification: String,
}

/// A request PIM took: its id (to cancel it by), and its status.
pub struct Scheduled {
    pub id: String,
    pub status: String,
}

fn pending(rows: &[Value]) -> Vec<Request> {
    rows.iter().filter(|v| s(v, "/properties/status") == "PendingApproval").map(request).collect()
}

fn request(v: &Value) -> Request {
    let name = s(v, "/properties/expandedProperties/principal/displayName");
    let email = s(v, "/properties/expandedProperties/principal/email");
    Request {
        id: s(v, "/id"),
        status: s(v, "/properties/status"),
        scope: s(v, "/properties/scope"),
        role_def_id: s(v, "/properties/roleDefinitionId"),
        requester: if email.is_empty() { name } else { format!("{name} <{email}>") },
        role: s(v, "/properties/expandedProperties/roleDefinition/displayName"),
        scope_name: s(v, "/properties/expandedProperties/scope/displayName"),
        subscription_id: subscription_of(&s(v, "/properties/scope")),
        justification: s(v, "/properties/justification"),
        duration: s(v, "/properties/scheduleInfo/expiration/duration"),
        created: s(v, "/properties/createdOn").parse().ok(),
        approval_id: s(v, "/properties/approvalId"),
    }
}

/// The ISO 8601 durations PIM uses: `PT8H`, `PT30M`, `P1D`, `P1DT2H`.
fn iso_duration(text: &str) -> Option<chrono::Duration> {
    let rest = text.strip_prefix('P')?;
    let (days, time) = rest.split_once('T').unwrap_or((rest, ""));
    let mut total = chrono::Duration::zero();
    if let Some(d) = days.strip_suffix('D') {
        total += chrono::Duration::days(d.parse().ok()?);
    }
    let mut num = String::new();
    for c in time.chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'H' | 'M' | 'S' => {
                let n: f64 = std::mem::take(&mut num).parse().ok()?;
                let secs = n * match c { 'H' => 3600.0, 'M' => 60.0, _ => 1.0 };
                total += chrono::Duration::seconds(secs as i64);
            }
            _ => return None,
        }
    }
    Some(total)
}

/// An error from ARM, with its code for callers that handle some of them.
#[derive(Debug)]
pub struct ArmError {
    /// The HTTP status, e.g. 401 when the token is no longer accepted.
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ArmError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ArmError {}

/// The ARM error code behind `e`, if it is one.
pub fn code(e: &anyhow::Error) -> &str {
    e.downcast_ref::<ArmError>().map_or("", |e| e.code.as_str())
}

/// Whether ARM is throttling us (429).
pub fn throttled(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ArmError>().is_some_and(|e| e.status == 429)
}

/// Whether ARM turned the token down (401), so a fresh one is worth getting.
pub fn unauthorized(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ArmError>().is_some_and(|e| e.status == 401)
}

pub struct Pim<'a> {
    pub http: &'a Client,
    pub token: &'a str,
}

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn subscription_of(scope: &str) -> String {
    scope.strip_prefix("/subscriptions/").and_then(|r| r.split('/').next()).unwrap_or_default().to_owned()
}

impl Pim<'_> {
    async fn get_all(&self, url: String) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut next = Some(url);
        while let Some(url) = next {
            let v = self.send(self.http.get(&url)).await?;
            // Every ARM list has `value`, empty or not: without it this isn't the answer.
            let items = v["value"].as_array().context("unexpected answer from ARM (no `value` list)")?;
            out.extend(items.iter().cloned());
            next = v["nextLink"].as_str().map(str::to_owned);
        }
        Ok(out)
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let mut req = req.bearer_auth(self.token);
        let mut tries = 0;
        let resp = loop {
            let retry = req.try_clone();
            let resp = req.send().await.context("request to ARM failed")?;
            // Throttled: ARM didn't act on it, so it's safe to send again once it says when.
            match retry {
                Some(again) if resp.status() == 429 && tries < THROTTLE_RETRIES => {
                    let wait = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()?.parse().ok())
                        .unwrap_or(2u64 << tries)
                        .min(MAX_RETRY_AFTER);
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    tries += 1;
                    req = again;
                }
                _ => break resp,
            }
        };
        let status = resp.status();
        if !status.is_success() {
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            let message = body.pointer("/error/message").and_then(Value::as_str).map_or_else(|| status.to_string(), str::to_owned);
            return Err(ArmError { status: status.as_u16(), code: s(&body, "/error/code"), message }.into());
        }
        // A success that isn't JSON (a proxy's page, say) mustn't read as "nothing
        // there"; an empty one (some writes answer with none) is fine.
        let bytes = resp.bytes().await.context("request to ARM failed")?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).context("unexpected answer from ARM (not JSON)")
    }

    /// Names of the subscriptions this user can see; eligible-only ones may be missing.
    pub async fn subscriptions(&self) -> Result<BTreeMap<String, String>> {
        let url = format!("{ARM}/subscriptions?api-version={API_SUBSCRIPTIONS}");
        Ok(self
            .get_all(url)
            .await?
            .iter()
            .map(|v| (s(v, "/subscriptionId"), s(v, "/displayName")))
            .collect())
    }

    pub async fn eligible(&self) -> Result<Vec<Eligible>> {
        let url = format!(
            "{ARM}/providers/Microsoft.Authorization/roleEligibilityScheduleInstances?api-version={API}&$filter=asTarget()"
        );
        Ok(self
            .get_all(url)
            .await?
            .iter()
            .map(|v| {
                let scope = s(v, "/properties/scope");
                Eligible {
                    id: s(v, "/id"),
                    role: s(v, "/properties/expandedProperties/roleDefinition/displayName"),
                    scope_name: s(v, "/properties/expandedProperties/scope/displayName"),
                    scope_type: s(v, "/properties/expandedProperties/scope/type"),
                    subscription_id: subscription_of(&scope),
                    role_def_id: s(v, "/properties/roleDefinitionId"),
                    eligibility_schedule_id: s(v, "/properties/roleEligibilityScheduleId"),
                    approval: None,
                    max_hours: None,
                    scope,
                }
            })
            .collect())
    }

    async fn requests(&self, filter: &str) -> Result<Vec<Value>> {
        let url = format!(
            "{ARM}/providers/Microsoft.Authorization/roleAssignmentScheduleRequests?api-version={API}&$filter={filter}"
        );
        self.get_all(url).await
    }

    /// Requests waiting for my approval.
    pub async fn to_review(&self) -> Result<Vec<Request>> {
        Ok(pending(&self.requests("asApprover()").await?))
    }

    /// My active roles and my requests still waiting for approval, both from my own requests.
    ///
    /// The obvious source, `roleAssignmentScheduleInstances`, can't be filtered to
    /// activations and returns every standing assignment too: hundreds of rows and
    /// 10–15 seconds. My requests are a few dozen rows in well under a second, and
    /// each activation in them says when it starts and how long it lasts.
    pub async fn mine(&self) -> Result<Mine> {
        let mut rows = self.requests("asTarget()").await?;
        rows.sort_by_key(|v| s(v, "/properties/createdOn"));
        let now = Utc::now();
        let mut active: Vec<Active> = Vec::new();
        let mut latest: Vec<Request> = Vec::new();
        for v in &rows {
            let scope = s(v, "/properties/scope");
            let role_def_id = s(v, "/properties/roleDefinitionId");
            let status = s(v, "/properties/status");
            let kind = s(v, "/properties/requestType");
            if matches!(kind.as_str(), "SelfActivate" | "SelfExtend" | "SelfRenew") {
                latest.retain(|r| !(r.scope == scope && r.role_def_id == role_def_id));
                latest.push(request(v));
            }
            match kind.as_str() {
                "SelfActivate" | "SelfExtend" | "SelfRenew" if status == "Provisioned" => {
                    let start: Option<DateTime<Utc>> = s(v, "/properties/scheduleInfo/startDateTime").parse().ok();
                    let end = s(v, "/properties/scheduleInfo/expiration/endDateTime").parse().ok().or_else(|| {
                        Some(start? + iso_duration(&s(v, "/properties/scheduleInfo/expiration/duration"))?)
                    });
                    // Later requests for the same role replace earlier ones.
                    active.retain(|a| !(a.scope == scope && a.role_def_id == role_def_id));
                    if start.is_some_and(|t| t <= now) && end.is_none_or(|t| t > now) {
                        active.push(Active {
                            id: s(v, "/id"),
                            start,
                            role: s(v, "/properties/expandedProperties/roleDefinition/displayName"),
                            scope_name: s(v, "/properties/expandedProperties/scope/displayName"),
                            subscription_id: subscription_of(&scope),
                            role_def_id,
                            end,
                            scope,
                        });
                    }
                }
                "SelfDeactivate" if !matches!(status.as_str(), "Failed" | "Denied" | "Canceled" | "PendingApproval") => {
                    active.retain(|a| !(a.scope == scope && a.role_def_id == role_def_id));
                }
                _ => {}
            }
        }
        Ok(Mine { active, pending: pending(&rows), latest })
    }

    /// The PIM policy for activating a role at a scope: approval, and the longest activation.
    pub async fn policy(&self, scope: &str, role_def_id: &str) -> Result<Policy> {
        let url = format!(
            "{ARM}{scope}/providers/Microsoft.Authorization/roleManagementPolicyAssignments?api-version={API}&$filter=roleDefinitionId eq '{role_def_id}'"
        );
        let assignments = self.get_all(url).await?;
        let rules: Vec<&Value> = assignments
            .iter()
            .flat_map(|a| a.pointer("/properties/effectiveRules").and_then(Value::as_array).into_iter().flatten())
            .collect();
        let rule = |id: &str| rules.iter().find(|r| s(r, "/id") == id).copied();
        Ok(Policy {
            approval: rule("Approval_EndUser_Assignment")
                .and_then(|r| r.pointer("/setting/isApprovalRequired"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            max_hours: rule("Expiration_EndUser_Assignment")
                .and_then(|r| iso_duration(&s(r, "/maximumDuration")))
                .map(|d| (d.num_minutes() / 60).max(1) as u32),
            fetched: Utc::now(),
        })
    }

    /// Who approved or denied a request, from its approval. Requesters may not
    /// be allowed to read it, in which case there's nothing to say.
    pub async fn decision(&self, approval_id: &str) -> Result<Option<Decision>> {
        let approval = self.send(self.http.get(format!("{ARM}{approval_id}?api-version={API_APPROVALS}"))).await?;
        let stages = approval.pointer("/properties/stages").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(stages
            .iter()
            .rev()
            .find(|v| !matches!(s(v, "/properties/reviewResult").as_str(), "" | "NotReviewed"))
            .map(|v| {
                let name = s(v, "/properties/reviewedBy/displayName");
                Decision {
                    by: if name.is_empty() { s(v, "/properties/reviewedBy/email") } else { name },
                    justification: s(v, "/properties/justification"),
                }
            }))
    }

    /// Asks for `hours` of the role; PIM answers with the request, e.g.
    /// `Provisioned`, or `PendingApproval` when an approver has to decide.
    pub async fn activate(&self, principal: &str, e: &Eligible, hours: u32, justification: &str) -> Result<Scheduled> {
        let body = json!({ "properties": {
            "principalId": principal,
            "roleDefinitionId": e.role_def_id,
            "requestType": "SelfActivate",
            "linkedRoleEligibilityScheduleId": e.eligibility_schedule_id,
            "justification": justification,
            "scheduleInfo": {
                "startDateTime": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "expiration": { "type": "AfterDuration", "duration": format!("PT{hours}H") }
            }
        }});
        self.schedule_request(&e.scope, body).await
    }

    pub async fn deactivate(&self, principal: &str, a: &Active) -> Result<Scheduled> {
        let body = json!({ "properties": {
            "principalId": principal,
            "roleDefinitionId": a.role_def_id,
            "requestType": "SelfDeactivate",
        }});
        self.schedule_request(&a.scope, body).await
    }

    async fn schedule_request(&self, scope: &str, body: Value) -> Result<Scheduled> {
        let path = format!("{scope}/providers/Microsoft.Authorization/roleAssignmentScheduleRequests/{}", uuid::Uuid::new_v4());
        let v = self.send(self.http.put(format!("{ARM}{path}?api-version={API}")).json(&body)).await?;
        let id = s(&v, "/id");
        Ok(Scheduled { id: if id.is_empty() { path } else { id }, status: s(&v, "/properties/status") })
    }

    /// Withdraws one of my requests while it waits for an approver.
    pub async fn cancel(&self, request_id: &str) -> Result<()> {
        // No body, but ARM wants to be told so (Content-Length: 0).
        self.send(self.http.post(format!("{ARM}{request_id}/cancel?api-version={API}")).body("")).await?;
        Ok(())
    }

    pub async fn review(&self, r: &Request, approve: bool, justification: &str) -> Result<()> {
        if r.approval_id.is_empty() {
            bail!("request has no approval id");
        }
        let approval = self
            .send(self.http.get(format!("{ARM}{}?api-version={API_APPROVALS}", r.approval_id)))
            .await?;
        let stages = approval.pointer("/properties/stages").and_then(Value::as_array).cloned().unwrap_or_default();
        let open = |v: &&Value| s(v, "/properties/status") == "InProgress";
        let stage = stages
            .iter()
            .filter(open)
            .find(|v| v.pointer("/properties/assignedToMe").and_then(Value::as_bool) == Some(true))
            .or_else(|| stages.iter().find(open))
            .ok_or_else(|| anyhow!("no open approval stage (already reviewed?)"))?;

        let stage_id = s(stage, "/id");
        let stage_path = if stage_id.starts_with('/') {
            stage_id
        } else {
            format!("{}/stages/{}", r.approval_id, s(stage, "/name"))
        };
        let body = json!({ "properties": {
            "reviewResult": if approve { "Approve" } else { "Deny" },
            "justification": justification,
        }});
        self.send(self.http.put(format!("{ARM}{stage_path}?api-version={API_APPROVALS}")).json(&body))
            .await?;
        Ok(())
    }
}
