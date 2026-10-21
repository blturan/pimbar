// The panel: draws the snapshot the app sends, and turns clicks into its commands.
// Everything from Azure (names, justifications) goes in as text, never as HTML.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const HOURS = [1, 2, 4, 8];
const $ = (id) => document.getElementById(id);

let snap = null;
const ui = {
  query: "",
  /** Subscriptions opened by hand. */
  open: new Set(),
  /** The one inline form showing: { kind: "activate" | "review", id, hours, text, busy, error }. */
  form: null,
  /** The last action's result: { tone, text }. */
  flash: null,
  /** A redraw that waited for typing to stop. */
  stale: false,
  /** "main", "settings" or "history". */
  view: "main",
};

/** Builds an element; strings become text nodes, `on…` props become listeners. */
function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props ?? {})) {
    if (v == null || v === false) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat(Infinity)) if (kid != null && kid !== false) el.append(kid);
  return el;
}

/** A pill on the right of a row: where something stands, in its tone; without
 *  one, grey, for plain facts like time left and finished tasks. */
const pill = (tone, text, title = null) => h("span", { class: `pill tone-${tone ?? "never"}`, title }, text);
/** Holds the ✕'s place on a row that has none (yet), so the pills above and below line up. */
const slot = () => h("span", { class: "slot", "aria-hidden": "true" });
const svg = (markup) => {
  const t = document.createElement("template");
  t.innerHTML = markup; // Constant markup only.
  return t.content.firstChild;
};
const chevron = () => svg('<svg class="chev" viewBox="0 0 10 10"><path d="M3.5 1.5 7 5l-3.5 3.5"/></svg>');

/** Time left, as a span of time: "3h 12m", "45m". */
function left(end) {
  if (!end) return "No end";
  const mins = Math.max(0, Math.round((new Date(end) - Date.now()) / 60000));
  return mins >= 60 ? `${Math.floor(mins / 60)}h ${String(mins % 60).padStart(2, "0")}m` : `${mins}m`;
}

function ago(when) {
  if (!when) return "";
  const mins = Math.round((Date.now() - new Date(when)) / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  return `${Math.floor(mins / 60)}h ago`;
}

/** "PT2H30M" → "2h 30m". */
function duration(iso) {
  const m = /PT(?:(\d+)H)?(?:(\d+)M)?/.exec(iso ?? "");
  if (!m) return iso || "";
  return [m[1] && `${m[1]}h`, m[2] && `${m[2]}m`].filter(Boolean).join(" ");
}

const subName = (id) => snap.subscriptions[id] || id || "Above subscriptions";
const isOpen = (t) => t.phase === "running" || t.phase === "waiting";
/** The task in progress for a role, if any. */
const taskFor = (scope, roleDefId) => snap.tasks.find((t) => isOpen(t) && t.scope === scope && t.role_def_id === roleDefId);
const clock = (when) => new Date(when).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
const spin = () => h("span", { class: "spin", "aria-hidden": "true" });
/** Waiting on an approver: amber, like the dot beside it. */
const waiting = () => pill("warn", "Pending", "Waiting for an approver");
const isActive = (e) => snap.active.some((a) => a.scope === e.scope && a.role_def_id === e.role_def_id);

// ---- Actions ---------------------------------------------------------------

function flash(tone, text) {
  ui.flash = { tone, text };
  render();
  clearTimeout(flash.timer);
  flash.timer = setTimeout(() => { ui.flash = null; render(); }, 8000);
}

/** Starts a task: the app answers at once and the task list takes it from there. */
async function run(command, args) {
  try {
    await invoke(command, args);
    // The form goes; so does the focus in its text box, or the redraw would wait for typing to end.
    if (ui.form) document.activeElement?.blur();
    ui.form = null;
  } catch (err) {
    if (ui.form) ui.form = { ...ui.form, error: String(err) };
    else flash("fail", String(err));
  }
  render();
}

function openForm(kind, id) {
  const same = ui.form?.kind === kind && ui.form?.id === id;
  ui.form = same ? null : { kind, id, hours: 1, text: "", busy: false, error: null };
  render();
}

// ---- Sections --------------------------------------------------------------

function section(title, count, ...body) {
  return h("section", { class: "section" },
    h("h2", { class: "title" }, title, count != null && h("span", {}, `· ${count}`)),
    body);
}

function textarea(placeholder) {
  return h("textarea", {
    class: "input", rows: 2, placeholder, "data-autofocus": true, disabled: ui.form.busy,
    oninput: (e) => { ui.form.text = e.target.value; },
  }, ui.form.text);
}

const formError = () => ui.form.error && h("div", { class: "banner tone-fail" }, ui.form.error);

function activateForm(e) {
  const f = ui.form;
  // Only what the role's policy allows; its longest if that's not one of ours.
  const hours = e.max_hours ? HOURS.filter((n) => n <= e.max_hours) : HOURS;
  if (!hours.length) hours.push(e.max_hours);
  if (!hours.includes(f.hours)) f.hours = hours[0];
  const verb = e.approval ? "Request" : "Elevate";
  const submit = () => run("activate", { id: e.id, hours: f.hours, justification: f.text });
  return h("div", { class: "form" },
    h("div", { class: "label" }, "Duration"),
    h("div", { class: "choices" }, hours.map((n) =>
      h("button", { class: "choice", "aria-pressed": String(f.hours === n), disabled: f.busy, onclick: () => { f.hours = n; render(); } }, `${n}h`))),
    h("div", { class: "label" }, "Justification"),
    textarea("Why do you need it?"),
    formError(),
    h("div", { class: "actions" },
      h("button", { class: "btn small", disabled: f.busy, onclick: () => { ui.form = null; render(); } }, "Cancel"),
      h("button", { class: "btn small toned tone-accent", disabled: f.busy, onclick: submit },
        verb)));
}

function approvals() {
  if (!snap.to_review.length) return null;
  return section("Waiting for your approval", snap.to_review.length, snap.to_review.map((r) => {
    const open = ui.form?.kind === "review" && ui.form.id === r.id;
    const decide = (approve) => run("review", { id: r.id, approve, justification: ui.form.text });
    return h("div", { class: "card" },
      h("div", { class: "row task tone-warn", title: r.requester },
        mark("waiting"),
        h("div", { class: "grow" },
          h("div", { class: "name" }, who(r)),
          h("div", { class: "sub" }, `${r.role} · ${r.scope_name}`)),
        pill(null, duration(r.duration), `Asks for ${duration(r.duration)}`),
        // ✓ to review: the form asks for a reason before Approve or Deny.
        open ? slot() : iconButton("Review", "M3.2 8.4 6.6 11.6 12.8 4.6", () => openForm("review", r.id))),
      open && h("div", { class: "form" },
        h("dl", { class: "facts" },
          h("dt", {}, "Role"), h("dd", {}, r.role),
          h("dt", {}, "Scope"), h("dd", {}, `${r.scope_name} (${subName(r.subscription_id)})`),
          h("dt", {}, "For"), h("dd", {}, duration(r.duration)),
          h("dt", {}, "Asked"), h("dd", {}, ago(r.created))),
        h("div", { class: "label" }, "Their reason"),
        h("p", { class: "quote" }, r.justification || "—"),
        h("div", { class: "label" }, "Your justification"),
        textarea("Required, for the audit log"),
        formError(),
        h("div", { class: "actions" },
          h("button", { class: "btn small", disabled: ui.form.busy, onclick: () => { ui.form = null; render(); } }, "Cancel"),
          h("button", { class: "btn small toned tone-fail", disabled: ui.form.busy, onclick: () => decide(false) }, "Deny"),
          h("button", { class: "btn small toned tone-pass", disabled: ui.form.busy, onclick: () => decide(true) }, "Approve"))));
  }));
}

/** A mark on the left, in its row's tone: a ✓, a dot or a spinner. Every
 *  card in Tasks, Active and Your requests has one, so their text lines up;
 *  so does an eligible role you have or are getting. */
function mark(phase, title = null) {
  return h("span", { class: "phase", title }, phase === "waiting" ? h("span", { class: "dot" }) : PHASES[phase].icon ? svg(PHASES[phase].icon) : spin());
}

/** A role you have: how long it has left and Stop. Stopped in its first five
 *  minutes, PIM won't yet: the stop waits, says when, and can be undone. */
function active() {
  if (!snap.active.length) return null;
  return section("Active", snap.active.length, snap.active.map((a) => {
    const t = taskFor(a.scope, a.role_def_id);
    const where = a.scope_name === subName(a.subscription_id) ? a.scope_name : `${a.scope_name} · ${subName(a.subscription_id)}`;
    const scheduled = t?.phase === "waiting" && t.kind === "deactivate";
    // Time left, grey; amber in its last 10 minutes, so an ending role catches the eye.
    const soon = a.end && new Date(a.end) - Date.now() <= 10 * 60000;
    const [phase, sub, right] = scheduled
      ? ["waiting", where, [
        // Why it waits is in the pill's tooltip.
        pill("warn", `Stops ${t.until ? clock(t.until) : "soon"}`, "PIM won't stop a role in its first 5 minutes after activating"),
        h("button", { class: "btn small", title: "Cancel the stop: the role stays active", onclick: () => run("cancel", { id: t.id }) }, "Undo")]]
      : t
        ? ["running", where, [pill("accent", "Stopping"), slot()]]
        : ["done", where, [pill(soon ? "warn" : null, left(a.end), a.end ? `Until ${clock(a.end)}` : null), stopButton(a, where)]];
    return h("div", { class: "card" }, h("div", { class: `row task tone-${PHASES[phase].tone}` },
      mark(phase),
      h("div", { class: "grow" }, h("div", { class: "name" }, a.role), h("div", { class: "sub" }, sub)),
      right));
  }));
}

function mine() {
  if (!snap.my_pending.length) return null;
  return section("Your requests", snap.my_pending.length, snap.my_pending.map((r) =>
    h("div", { class: "card" }, h("div", { class: "row task tone-warn" },
      mark("waiting"),
      h("div", { class: "grow" },
        h("div", { class: "name" }, r.role),
        h("div", { class: "sub" }, `${r.scope_name} · ${ago(r.created)}`)),
      waiting(),
      cancelButton(r.id, r.role, r.scope_name)))));
}

/** A small icon on a card, like the ✕ on tasks: `path` on the 16 grid the task marks use. */
const iconButton = (label, path, onclick) => h("button", { class: "btn icon dismiss", title: label, "aria-label": label, onclick },
  svg(`<svg viewBox="0 0 16 16"><path d="${path}"/></svg>`));

/** The ✕ on a card: clears it, or, after asking (`ask`), stops or cancels. */
const xButton = (label, onclick) => iconButton(label, "M4.5 4.5 11.5 11.5M11.5 4.5 4.5 11.5", onclick);

/** ✕ for one of your requests still waiting for an approver. */
const cancelButton = (id, role, scope) => xButton("Cancel request", () => ask(
  "Cancel this request?",
  `This withdraws your request for ${role} on ${scope}, so approvers won't see it any more. You can ask again later.`,
  "Cancel request",
  () => run("cancel_request", { id })));

/** ✕ for a role you have. */
const stopButton = (a, where) => xButton("Stop", () => ask(
  "Stop this role?",
  `You lose ${a.role} on ${where}. To get it back, you'll have to ask for it again.`,
  "Stop",
  () => run("deactivate", { id: a.id })));

/** Asks before something that can't be undone, as Quit does; `yes` names it. */
let confirmed = null;
function ask(title, text, yes, action) {
  confirmed = action;
  $("confirm-title").textContent = title;
  $("confirm-text").textContent = text;
  $("confirm-yes").textContent = yes;
  $("confirm-dialog").hidden = false;
  $("confirm-keep").focus();
}
function closeConfirm() {
  confirmed = null;
  $("confirm-dialog").hidden = true;
}

/** Sets opening at login; a refusal from macOS shows as a banner. */
const setLogin = (on) => invoke("set_login", { on }).catch((err) => flash("fail", String(err)));

/** The first-run question: open at login? Asked once. */
function loginPrompt() {
  if (!snap.ask_login) return null;
  return h("div", { class: "card toned tone-accent prompt" },
    h("div", { class: "row" },
      h("div", { class: "grow" },
        h("div", { class: "name" }, "Open pimbar at login?"),
        h("div", { class: "sub wrap" }, "Then it's always in your menu bar, and you hear about approvals from the moment you log in."))),
    h("div", { class: "actions pad" },
      h("button", { class: "btn small", onclick: () => invoke("skip_login") }, "Not now"),
      h("button", { class: "btn small toned tone-accent", onclick: () => setLogin(true) }, "Open at login")));
}

/** Running from its installer: move to Applications first, or quit. */
function movePrompt() {
  const move = async () => {
    ui.moving = true;
    render();
    try {
      await invoke("move_to_applications");
    } catch (err) {
      ui.moving = false;
      flash("fail", `Couldn't move pimbar: ${err}`);
    }
  };
  return h("div", { class: "card prompt" },
    h("div", { class: "row task tone-warn" },
      h("span", { class: "phase" }, svg('<svg viewBox="0 0 16 16"><path d="M8 2.2 14.4 13.4H1.6z"/><path d="M8 6.4v3M8 11.4v.1"/></svg>')),
      h("div", { class: "grow" },
        h("div", { class: "name" }, "Move pimbar to Applications?"),
        h("div", { class: "sub wrap" }, "pimbar is running from the installer. In Applications it can open at login, update itself, and keep working after you eject the installer."))),
    h("div", { class: "actions pad" },
      h("button", { class: "btn small", disabled: ui.moving, onclick: () => invoke("quit") }, "Quit"),
      h("button", { class: "btn small toned tone-accent", disabled: ui.moving, onclick: move },
        ui.moving ? [spin(), "Moving…"] : "Move to Applications")));
}

/** Shows a page ("settings", "history") or, with "main", goes back. */
function show(view) {
  ui.view = view;
  ui.form = null;
  // Fresh notification and login status for the page.
  if (view === "settings") invoke("refresh");
  render();
}
const openSettings = (open) => show(open ? "settings" : "main");

/** Sends a test notification; a short line under the Notifications row says so for a few seconds. */
function testNotification() {
  invoke("test_notification").then(() => {
    ui.testSent = true;
    render();
    clearTimeout(testNotification.timer);
    testNotification.timer = setTimeout(() => { ui.testSent = false; render(); }, 6000);
  });
}

/** Saves the sync settings, changing only what's given. */
const setSync = (change) => invoke("set_sync", { requests: snap.sync_minutes, roles: snap.roles_minutes, manual: snap.manual, ...change })
  .catch((err) => flash("fail", String(err)));

/** The values − and + step through. */
const STEPS = [1, 2, 5, 10, 15, 30, 60, 120, 240];

/** A sync interval: − [n min] +, saved as it changes; the number can be typed too. */
function stepper(label, key, value, max) {
  const set = (n) => {
    n = Math.min(max, Math.max(1, Math.round(n)));
    if (n !== value) setSync({ [key]: n });
  };
  const less = STEPS.filter((s) => s < value).pop() ?? 1;
  const more = STEPS.find((s) => s > value && s <= max) ?? max;
  return h("div", { class: "row" },
    h("div", { class: "grow name" }, label),
    h("div", { class: "stepper" },
      h("button", { class: "step", "aria-label": `${label}: less often`, disabled: value <= 1, onclick: () => set(less) }, "−"),
      h("input", {
        class: "step-value", type: "text", inputmode: "numeric", value: String(value), "aria-label": `${label}, minutes`,
        onchange: (e) => { const n = parseInt(e.target.value, 10); if (Number.isFinite(n)) set(n); else e.target.value = value; },
        onkeydown: (e) => { if (e.key === "Enter") e.target.blur(); },
      }),
      h("span", { class: "step-unit" }, "min"),
      h("button", { class: "step", "aria-label": `${label}: more often`, disabled: value >= max, onclick: () => set(more) }, "+")));
}

/** Settings → Updates: this version and whether there's a newer one (with the
 *  button for it), when pimbar last looked, and who installs updates. */
function updateRow() {
  const u = snap.update;
  return [
    h("div", { class: "row" },
      // How it came, quietly: Homebrew or the .dmg, which is also how Update installs.
      h("div", { class: "grow name" }, `Version ${snap.version} `,
        h("span", { class: "muted via" }, u.via === "homebrew" ? "(Homebrew)" : "(.dmg)")),
      u.updating ? pill("accent", [spin(), "Updating"])
        : u.available ? pill("accent", `${u.latest} available`)
        : u.latest ? pill("pass", "Up to date") : null,
      u.available
        ? h("button", { class: "btn small toned tone-accent", disabled: u.updating, onclick: () => invoke("install_update").catch((err) => flash("fail", String(err))) }, "Update")
        : h("button", { class: "btn small", onclick: () => invoke("check_for_update") }, "Check now")),
    // Grey, whatever happened; what went wrong is in the tooltip.
    h("div", { class: "note tone-never", title: u.error && !u.updating ? u.error : null },
      u.error && !u.updating ? (u.available ? "Unable to update right now" : "Unable to check for updates")
        : u.checked_at ? `Checked ${ago(u.checked_at)}` : "Not checked yet"),
  ];
}

const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** Settings → Hidden: folded to one line ("2 subscriptions, 3 roles"); open,
 *  each with a Show button. With nothing hidden, how to hide something. */
function hiddenCard() {
  const subs = [...snap.hidden].sort((a, b) => subName(a).localeCompare(subName(b)));
  // Roles hidden one by one, while still eligible; a hidden subscription's are counted with it.
  const roles = snap.eligible.filter((e) => roleHidden(e) && !snap.hidden.includes(e.subscription_id))
    .sort((a, b) => a.role.localeCompare(b.role));
  if (!subs.length && !roles.length) {
    return h("div", { class: "card" }, h("div", { class: "row" },
      h("div", { class: "sub wrap" }, "Access you never use? Click a subscription or role with two fingers to hide it.")));
  }
  const count = [subs.length && plural(subs.length, "subscription"), roles.length && plural(roles.length, "role")].filter(Boolean).join(", ");
  const show = (kind, key) => h("button", { class: "btn small soft", onclick: () => setHidden(kind, key, false) }, "Show");
  return h("details", {
    class: "card group", open: ui.open.has("settings:hidden"),
    ontoggle: (ev) => ev.target.open ? ui.open.add("settings:hidden") : ui.open.delete("settings:hidden"),
  },
    h("summary", { class: "row" }, chevron(), h("div", { class: "grow name" }, count)),
    subs.map((id) => h("div", { class: "row role" },
      h("div", { class: "grow" },
        h("div", { class: "name" }, subName(id)),
        h("div", { class: "sub" }, `Subscription · ${plural(snap.eligible.filter((e) => e.subscription_id === id).length, "role")}`)),
      show("subscription", id))),
    roles.map((e) => h("div", { class: "row role" },
      h("div", { class: "grow" },
        h("div", { class: "name" }, e.role),
        h("div", { class: "sub" }, e.scope_name === subName(e.subscription_id) ? e.scope_name : `${e.scope_name} · ${subName(e.subscription_id)}`)),
      show("role", roleKey(e)))));
}

/** Settings: opening at login, sync, notifications, and who's signed in. */
function settingsPage() {
  const login = {
    on: "pimbar starts when you log in.",
    off: "Start pimbar when you log in, so it's always in your menu bar.",
    approval: "Switched off in Login Items: turn it on there to finish.",
    unavailable: "Only for pimbar installed as an app.",
  }[snap.login];
  const n = snap.notifications;
  return [
    section("General", null, h("div", { class: "card" },
      h("label", { class: "row" },
        h("div", { class: "grow" },
          h("div", { class: "name" }, "Open at login"),
          h("div", { class: "sub wrap" }, login)),
        h("input", {
          type: "checkbox", class: "toggle", checked: snap.login === "on", disabled: snap.login === "unavailable",
          // While macOS waits for approval, asking again opens Login Items.
          onchange: (e) => setLogin(snap.login === "approval" || e.target.checked),
        })))),
    section("Appearance", null, h("div", { class: "card" },
      h("div", { class: "row" },
        h("div", { class: "segmented three" }, [["system", "System"], ["light", "Light"], ["dark", "Dark"]].map(([mode, label]) =>
          h("button", {
            "aria-pressed": String(snap.appearance === mode),
            onclick: () => invoke("set_appearance", { appearance: mode }).catch((err) => flash("fail", String(err))),
          }, label)))))),
    section("Sync", null, h("div", { class: "card" },
      h("div", { class: "row" },
        h("div", { class: "segmented" },
          h("button", { "aria-pressed": String(!snap.manual), onclick: () => setSync({ manual: false }) }, "Automatic"),
          h("button", { "aria-pressed": String(snap.manual), onclick: () => setSync({ manual: true }) }, "Manual"))),
      !snap.manual && stepper("Requests and approvals", "requests", snap.sync_minutes, 60),
      !snap.manual && stepper("Eligible roles", "roles", snap.roles_minutes, 240))),
    section("Hidden", null, hiddenCard()),
    section("Notifications", null, h("div", { class: "card" },
      // One line: what, whether macOS allows it, and a test.
      h("div", { class: "row" },
        h("div", { class: "grow name" }, "Approvals and task updates"),
        pill(n === false ? "fail" : n ? "pass" : null, n === false ? "Off" : n ? "On" : "…"),
        n === false && h("button", { class: "btn small", title: "Open System Settings → Notifications", onclick: () => invoke("open_notification_settings") }, "Turn on"),
        h("button", { class: "btn small", onclick: testNotification }, "Send test")),
      ui.testSent && h("div", { class: "note tone-never" }, "Sent · nothing showed? Check Focus"),
      snap.notify_error && h("div", { class: "actions pad" },
        h("div", { class: "banner tone-fail" }, `The last notification didn't go out: ${snap.notify_error}`)))),
    section("Account", null, h("div", { class: "card" },
      h("div", { class: "row" },
        h("div", { class: "grow" },
          h("div", { class: "name" }, snap.user || "Not signed in"),
          h("div", { class: "sub wrap" }, "To use another account, run az login."))))),
    section("Updates", null, h("div", { class: "card" }, updateRow())),
  ];
}

/** What PIM calls a scope, as people say it. */
const SCOPE_KINDS = { resourcegroup: "Resource group", resource: "Resource", managementgroup: "Management group" };

/** Where an eligible role stands, when it can't be elevated now: active, its
 *  request waiting for an approver, or being set up. Shown as the cards show
 *  it, by a mark on the left; its button greyed out, the tooltip says why. */
function roleState(e) {
  if (isActive(e)) return { phase: "done", title: "You have this role" };
  const t = taskFor(e.scope, e.role_def_id);
  if (!t || t.kind !== "activate") return null;
  return t.phase === "waiting"
    ? { phase: "waiting", title: "Waiting for an approver" }
    : { phase: "running", title: "Activating" };
}

const PHASES = {
  running: { tone: "accent", icon: null },
  waiting: { tone: "warn", icon: null },
  done: { tone: "pass", icon: '<svg viewBox="0 0 16 16"><path d="M3.2 8.4 6.6 11.6 12.8 4.6"/></svg>' },
  failed: { tone: "fail", icon: '<svg viewBox="0 0 16 16"><path d="M4.5 4.5 11.5 11.5M11.5 4.5 4.5 11.5"/></svg>' },
};

const who = (t) => t.requester.replace(/ <.*>$/, "");

const isCancelled = (t) => t.phase === "failed" && /^Cancell?ed/.test(t.message);

/** What a running task is doing, in a word. */
const DOING = { activate: "Activating", deactivate: "Stopping", approve: "Approving", deny: "Denying" };

/** Where a task stands: its pill's tone and word. Its row's mark takes the same tone. */
function status(t) {
  if (isCancelled(t)) return ["never", "Cancelled"];
  if (t.phase === "failed") return ["fail", t.message.startsWith("Denied") ? "Denied" : "Failed"];
  if (t.phase === "running") return ["accent", t.message.startsWith("Cancelling") ? "Cancelling" : DOING[t.kind]];
  if (t.phase === "waiting") return ["warn", t.kind === "deactivate" ? "Scheduled" : "Pending"];
  switch (t.kind) {
    case "activate": return ["pass", t.message.startsWith("Approved") ? "Approved" : "Active"];
    // Access removed: done, but not a green "you have it".
    case "deactivate": return ["accent", "Removed"];
    case "approve": return ["pass", "Approved"];
    // Someone's request you turned down: done, quietly.
    default: return ["never", "Denied"];
  }
}

/** A task's pill. Finished, grey: its mark already says how it went; red if it
 *  failed, since that needs a look. Still under way, in its tone. */
const outcome = (t) => {
  const [tone, text] = status(t);
  return pill(isOpen(t) || tone === "fail" ? tone : null, text);
};

/** Line two: what kind of task, and where. */
function kindLine(t) {
  const kind = { activate: "Activation", deactivate: "Deactivation", approve: `Approval for ${who(t)}`, deny: `Denial for ${who(t)}` }[t.kind];
  return `${kind} · ${t.scope_name}`;
}

/** Line three: the details. How it's going or how it ended, and the reason you gave. */
function detailLine(t) {
  let detail;
  if (t.phase === "done" && t.kind === "activate") {
    const by = t.message.match(/^Approved by (.*)$/)?.[1];
    detail = [by && `Approved by ${by}`, t.until && `Until ${clock(t.until)}`].filter(Boolean).join(" · ");
  } else if (t.phase === "failed" && t.message.startsWith("Denied")) {
    // Azure doesn't show the requester who denied it or why; when it does, say so.
    const by = t.message.match(/^Denied by ([^:]*)(?::\s*(.*))?$/);
    detail = by ? [`Denied by ${by[1]}`, by[2] && `“${by[2]}”`].filter(Boolean).join(": ") : "";
  } else if (t.phase === "waiting") {
    detail = t.kind === "deactivate" ? t.message : "Waiting for an approver";
  } else if (t.phase === "done") {
    detail = t.kind === "deactivate" ? "Access removed" : "";
  } else if (isCancelled(t)) {
    detail = "Withdrawn before anyone approved it";
  } else {
    detail = t.message;
  }
  return [detail, t.justification && `Your reason: ${t.justification}`].filter(Boolean).join(" · ") || "—";
}

/** A task's second line, short: how it stands in a few words, then where. */
function shortLine(t) {
  let detail = "";
  if (t.phase === "done" && t.kind === "activate") detail = t.until ? `Until ${clock(t.until)}` : "";
  else if (t.phase === "waiting") detail = t.kind === "deactivate" ? t.message : "";
  // Cancelled says it in its pill.
  else if (t.phase === "failed" && !isCancelled(t)) detail = t.message.match(/^Denied by ([^:]*)/)?.[0] ?? (t.message.startsWith("Denied") ? "" : t.message);
  else if (t.phase === "running" && !t.message.startsWith("Cancelling")) detail = t.message;
  const where = t.kind === "approve" || t.kind === "deny" ? `${who(t)} · ${t.scope_name}` : t.scope_name;
  return [detail, where].filter(Boolean).join(" · ");
}

/** Two lines: the name, with when and the pill (and `end`, the ✕) in the top
 *  right corner; then the details, the whole width. */
function twoLines(name, pillEl, sub, time, end = null) {
  return h("div", { class: "grow lines" },
    h("div", { class: "line" }, h("div", { class: "name" }, name), h("span", { class: "muted" }, time), pillEl, end),
    h("div", { class: "sub" }, sub));
}

/** One task, two lines: the role and how it ended; the details and when. The
 *  rest (what kind, the reason you gave) is in the tooltip. A finished one in
 *  the Tasks list (`dismissable`) gets an ✕ to clear it now; a request waiting
 *  for an approver, Cancel. */
function taskRow(t, time, dismissable = false) {
  const [tone] = status(t);
  const pending = dismissable && t.kind === "activate" && t.phase === "waiting" && t.request_id;
  return h("div", { class: `row task tone-${tone}`, title: [t.role, kindLine(t), detailLine(t)].join("\n") },
    mark(t.phase),
    twoLines(t.role, outcome(t), shortLine(t), time,
      pending ? cancelButton(t.request_id, t.role, t.scope_name)
        : dismissable && (isOpen(t) ? slot() : xButton("Clear", () => invoke("dismiss", { id: t.id })))));
}

/** Activations, deactivations and reviews: running, waiting on an approver, or
 *  finished since you last looked (they go when the panel closes). */
function taskList() {
  // A stop waiting out PIM's five minutes shows on its Active card instead.
  const tasks = snap.tasks.filter((t) => !(t.phase === "waiting" && t.kind === "deactivate"));
  if (!tasks.length) return null;
  const busy = tasks.filter(isOpen).length;
  return section("Tasks", busy || null,
    tasks.map((t) => h("div", { class: "card" }, taskRow(t, ago(t.finished ?? t.started), true))));
}

/** Hiding, by `kind`: a `subscription` (its id) or a `role` (`roleKey`). */
const setHidden = (kind, key, hidden) => invoke("set_hidden", { kind, key, hidden });
const roleKey = (e) => `${e.scope}|${e.role_def_id}`;
const roleHidden = (e) => snap.hidden_roles.includes(roleKey(e));
const isHidden = (e) => snap.hidden.includes(e.subscription_id) || roleHidden(e);
/** The two-finger-click menu: Hide Subscription or Hide Role. */
const hideMenu = (kind, key) => (ev) => { ev.preventDefault(); ev.stopPropagation(); invoke("hide_menu", { kind, key }); };

function eligible() {
  const q = ui.query.trim().toLowerCase();
  const shown = snap.eligible.filter((e) => !isHidden(e));
  const groups = new Map();
  for (const e of shown) {
    const name = subName(e.subscription_id);
    if (q && ![e.role, e.scope_name, name].some((s) => s.toLowerCase().includes(q))) continue;
    if (!groups.has(name)) groups.set(name, []);
    groups.get(name).push(e);
  }
  // Subscriptions with something active first, then by name.
  const sorted = [...groups].sort(([a, ra], [b, rb]) =>
    (rb.some(isActive) - ra.some(isActive)) || a.localeCompare(b));

  // Hidden ones, quietly, under the list: how many, and where to get them back.
  const subs = new Set(snap.eligible.filter((e) => snap.hidden.includes(e.subscription_id)).map((e) => e.subscription_id)).size;
  const roles = snap.eligible.filter((e) => roleHidden(e) && !snap.hidden.includes(e.subscription_id)).length;
  const hidden = subs + roles;
  const hiddenNote = hidden > 0 && h("p", { class: "list-note" },
    [subs && plural(subs, "subscription"), roles && plural(roles, "role")].filter(Boolean).join(" and "), " hidden · ",
    h("a", { href: "#", onclick: (e) => { e.preventDefault(); openSettings(true); ui.open.add("settings:hidden"); render(); } }, "Show in Settings"));

  const total = `${shown.length} in ${new Set(shown.map((e) => e.subscription_id)).size} subscriptions`;
  if (!sorted.length) {
    const empty = q ? `Nothing matches “${ui.query}”.` : hidden ? "Everything is hidden." : "You have no eligible roles.";
    return section("Eligible", total, h("p", { class: "empty" }, empty), hiddenNote);
  }
  return section("Eligible", total, sorted.map(([name, roles]) => {
    const on = roles.filter(isActive).length;
    roles.sort((a, b) => a.role.localeCompare(b.role));
    return h("details", {
      class: "card group", open: Boolean(q) || ui.open.has(name),
      // A two-finger click: the app's own menu, with Hide Subscription.
      oncontextmenu: hideMenu("subscription", roles[0].subscription_id),
      ontoggle: (ev) => { if (!q) ev.target.open ? ui.open.add(name) : ui.open.delete(name); },
    },
      h("summary", { class: "row" },
        chevron(),
        h("div", { class: "grow name" }, name),
        on > 0 && pill("pass", [svg(PHASES.done.icon), `${on}`], `${on} active`),
        h("span", { class: "muted" }, `${roles.length}`)),
      roles.flatMap((e) => {
        const open = ui.form?.kind === "activate" && ui.form.id === e.id;
        const state = roleState(e);
        const row = h("div", { class: `row role${state ? ` tone-${PHASES[state.phase].tone}` : ""}`, oncontextmenu: hideMenu("role", roleKey(e)) },
          // Under the subscription's arrow: the role's mark, as on the cards above.
          state && mark(state.phase, state.title),
          h("div", { class: "grow" },
            h("div", { class: "name" }, e.role),
            // Narrower than the subscription: what kind of scope, then its name, each on a line.
            e.scope_type !== "subscription" && h("div", { class: "sub" }, SCOPE_KINDS[e.scope_type] ?? e.scope_type),
            e.scope_type !== "subscription" && h("div", { class: "sub scope-name" }, e.scope_name)),
          h("button", {
            // The word says what happens: Elevate goes straight through, Request needs an approver.
            class: `btn small soft${open ? " toned tone-accent" : ""}`,
            disabled: Boolean(state),
            title: state ? state.title : e.approval == null ? null : e.approval ? "Needs approval" : "Activates straight away",
            onclick: () => openForm("activate", e.id),
          }, e.approval ? "Request" : "Elevate"));
        return open ? [row, activateForm(e)] : [row];
      }));
  }), hiddenNote);
}

/** "Today 14:03", "Yesterday 09:12", or "Mon 19 Oct 14:03". */
function when(at) {
  const d = new Date(at);
  const days = Math.round((new Date().setHours(0, 0, 0, 0) - new Date(at).setHours(0, 0, 0, 0)) / 86400000);
  const day = days === 0 ? "Today" : days === 1 ? "Yesterday"
    : d.toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" });
  return `${day} ${clock(at)}`;
}

/** Every finished task, newest first, grouped by day. */
/** "5 min", "1 h 20 min". */
function span(from, to) {
  const mins = Math.max(1, Math.round((new Date(to) - new Date(from)) / 60000));
  return mins < 60 ? `${mins} min` : `${Math.floor(mins / 60)} h${mins % 60 ? ` ${mins % 60} min` : ""}`;
}

/**
 * A role's history as a timeline, oldest first: activated, then removed (or
 * expired, when its time ran out without a stop), and requests that never
 * became active. Each event: when, what, the details, and a pill.
 */
function timeline(tasks) {
  const events = [];
  let since = null; // The activation still open, for "removed after 5 min" and "expired".
  const close = () => {
    if (since?.until && new Date(since.until) <= Date.now()) {
      events.push({ at: since.until, label: "Expired", what: "Expired", detail: `After ${span(since.at, since.until)}` });
    }
    since = null;
  };
  for (const t of [...tasks].sort((a, b) => new Date(a.started) - new Date(b.started))) {
    const at = t.finished ?? t.started;
    if (t.kind === "activate" && t.phase === "done") {
      close();
      const by = t.message.match(/^Approved by (.*)$/)?.[1];
      events.push({
        at, label: "Active", what: by ? "Approved and activated" : "Activated",
        detail: [t.until && `Until ${clock(t.until)}`, by && `By ${by}`, t.justification && `Your reason: ${t.justification}`].filter(Boolean).join(" · "),
      });
      since = { at, until: t.until };
    } else if (t.kind === "deactivate" && t.phase === "done") {
      events.push({ at, label: "Removed", what: "Removed", detail: since ? `After ${span(since.at, at)}` : "Access removed" });
      since = null;
    } else {
      events.push({ at, task: t, what: { activate: "Requested", deactivate: "Stop" }[t.kind] ?? "", detail: detailLine(t) });
    }
  }
  close();
  return events;
}

/** An event's pill: grey, as a finished task's is; red if it failed. */
const eventPill = (e) => e.label ? pill(null, e.label) : outcome(e.task);

/** One event in a role's timeline. Two lines, always: when and what; the details. */
function eventRow(e) {
  const head = `${when(e.at)} · ${e.what}`;
  return h("div", { class: "row session", title: [head, e.detail].join("\n") },
    h("div", { class: "grow lines" },
      h("div", { class: "name" }, head),
      h("div", { class: "detail" }, e.detail || "—")),
    eventPill(e));
}

/** Your history, a card per role and subscription with its timeline; then your reviews. */
function historyPage() {
  if (!snap.history.length) return [h("p", { class: "empty" }, "Nothing yet. Activations, stops and reviews show up here once they finish.")];
  const roles = new Map();
  const reviews = [];
  for (const t of snap.history) {
    if (t.kind === "approve" || t.kind === "deny") { reviews.push(t); continue; }
    const key = `${t.scope}|${t.role_def_id}`;
    if (!roles.has(key)) roles.set(key, []);
    roles.get(key).push(t);
  }
  // Roles with the latest activity first; inside, the story in order.
  const cards = [...roles].map(([key, tasks]) => {
    const events = timeline(tasks);
    const latest = events[events.length - 1];
    const count = events.length === 1 ? "1 event" : `${events.length} events`;
    return h("details", {
      class: "card group", open: ui.open.has(`history:${key}`),
      ontoggle: (ev) => ev.target.open ? ui.open.add(`history:${key}`) : ui.open.delete(`history:${key}`),
    },
      h("summary", { class: "row" },
        chevron(),
        twoLines(tasks[0].role, eventPill(latest), `${tasks[0].scope_name} · ${count}`, ago(latest.at))),
      events.map(eventRow));
  });
  return [
    section("Your roles", null, cards),
    reviews.length && section("Your reviews", null,
      reviews.map((t) => h("div", { class: "card" }, taskRow(t, when(t.finished ?? t.started))))),
  ].filter(Boolean);
}

/** The footer: "Synced just now", "Synced 4 min ago", "Synced 2 h ago". */
function lastSync() {
  if (!snap?.refreshed_at) return;
  const mins = Math.floor((Date.now() - new Date(snap.refreshed_at)) / 60000);
  const ago = mins < 1 ? "just now" : mins < 60 ? `${mins} min ago` : `${Math.floor(mins / 60)} h ago`;
  $("status").textContent = `Synced ${ago}`;
  $("status").title = `Synced at ${clock(snap.refreshed_at)}`;
}

// ---- Drawing ---------------------------------------------------------------

/** Light or dark when pinned in Settings; otherwise the panel follows macOS. */
function applyAppearance() {
  const mode = snap?.appearance;
  if (mode === "light" || mode === "dark") document.documentElement.dataset.theme = mode;
  else delete document.documentElement.dataset.theme;
}

function render() {
  applyAppearance();
  // Don't redraw under someone typing a justification; catch up when they leave the field.
  if (document.activeElement?.closest?.("#content textarea, #content .step-value") && !ui.form?.busy) {
    ui.stale = true;
    return;
  }
  ui.stale = false;

  const content = $("content");
  const scroll = content.scrollTop;
  content.replaceChildren();

  const page = ui.view !== "main";
  $("search-bar").hidden = page;
  $("page-head").hidden = !page;
  $("page-title").textContent = ui.view === "settings" ? "Settings" : "History";
  $("clear-history").hidden = ui.view !== "history" || !snap?.history.length;
  $("settings").classList.toggle("on", ui.view === "settings");
  // A newer pimbar: a dot on the gear, which leads to Settings → Updates.
  $("settings").classList.toggle("dot", Boolean(snap?.update?.available));
  $("history").classList.toggle("on", ui.view === "history");
  // From its installer: only the offer to move to Applications.
  if (snap?.misplaced) {
    $("search-bar").hidden = true;
    $("page-head").hidden = true;
    content.append(...[ui.flash && h("div", { class: `banner tone-${ui.flash.tone}` }, ui.flash.text), movePrompt()].filter(Boolean));
    return;
  }
  if (page && snap) {
    const body = ui.view === "settings" ? settingsPage() : historyPage();
    content.append(...[ui.flash && h("div", { class: `banner tone-${ui.flash.tone}` }, ui.flash.text), ...body].filter(Boolean));
    return;
  }

  if (!snap || (snap.loading && !snap.refreshed_at)) {
    const prompt = snap && loginPrompt();
    if (prompt) content.append(prompt);
    content.append(h("div", { class: "loading", role: "status" },
      h("div", { class: "spinner", "aria-hidden": "true" }),
      h("p", { class: "muted" }, "Fetching your access…")));
    return;
  }
  $("refresh").classList.toggle("busy", snap.loading);

  content.append(
    ...[
      ui.flash && h("div", { class: `banner tone-${ui.flash.tone}` }, ui.flash.text),
      loginPrompt(),
      ...snap.errors.map((e) => h("div", { class: "banner tone-fail" }, e)),
      snap.notifications === false && h("div", { class: "banner tone-warn" },
        "Notifications are off, so you won't hear when someone needs your approval. ",
        h("a", { href: "#", onclick: (e) => { e.preventDefault(); openSettings(true); } }, "Settings")),
      taskList(),
      approvals(),
      active(),
      mine(),
      eligible(),
    ].filter(Boolean));
  content.scrollTop = scroll;

  lastSync();

  const focus = content.querySelector("[data-autofocus]");
  if (focus && !focus.disabled) {
    focus.focus();
    focus.setSelectionRange(focus.value.length, focus.value.length);
  }
}

// ---- Wiring ----------------------------------------------------------------

$("query").addEventListener("input", (e) => { ui.query = e.target.value; render(); });
$("refresh").addEventListener("click", () => invoke("refresh"));
// Quitting asks first: a stray click shouldn't silence approvals.
function confirmQuit(ask) {
  $("quit-dialog").hidden = !ask;
  if (ask) $("quit-cancel").focus();
}
$("quit").addEventListener("click", () => confirmQuit(true));
$("quit-cancel").addEventListener("click", () => confirmQuit(false));
$("quit-yes").addEventListener("click", () => invoke("quit"));
// A click beside the dialog is a "no".
$("quit-dialog").addEventListener("click", (e) => { if (e.target === e.currentTarget) confirmQuit(false); });
$("confirm-keep").addEventListener("click", closeConfirm);
$("confirm-yes").addEventListener("click", () => { const action = confirmed; closeConfirm(); action?.(); });
$("confirm-dialog").addEventListener("click", (e) => { if (e.target === e.currentTarget) closeConfirm(); });
$("settings").addEventListener("click", () => show(ui.view === "settings" ? "main" : "settings"));
$("history").addEventListener("click", () => show(ui.view === "history" ? "main" : "history"));
$("clear-history").addEventListener("click", () => invoke("clear_history"));
$("back").addEventListener("click", () => openSettings(false));
$("content").addEventListener("focusout", () => setTimeout(() => ui.stale && render()));

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!$("quit-dialog").hidden) confirmQuit(false);
    else if (!$("confirm-dialog").hidden) closeConfirm();
    else if (ui.view !== "main") show("main");
    else if (ui.form && !ui.form.busy) { ui.form = null; render(); }
    else if (ui.query) { ui.query = $("query").value = ""; render(); }
    else invoke("hide");
  } else if (e.key === "r" && e.metaKey) {
    e.preventDefault();
    invoke("refresh");
  }
});

// Opening the panel puts you in the search box, unless a form is waiting.
window.addEventListener("focus", () => { if (!ui.form) $("query").focus(); });
// Keep "time left" honest between polls, and the footer's "Last sync" by the minute.
setInterval(render, 30_000);
setInterval(lastSync, 10_000);

listen("state", (e) => { snap = e.payload; render(); });
// Each opening starts fresh: the main view at the top, subscriptions folded, no form or search.
listen("closed", () => {
  confirmQuit(false);
  closeConfirm();
  ui.view = "main";
  ui.open.clear();
  ui.form = null;
  ui.query = $("query").value = "";
  document.activeElement?.blur();
  // render() keeps the scroll position, so reset it first.
  $("content").scrollTop = 0;
  render();
});
invoke("snapshot").then((s) => { snap = s; render(); });
