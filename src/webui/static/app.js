const API_ROOT = new URL("api/v1", document.baseURI).pathname.replace(/\/$/, "");
const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const number = new Intl.NumberFormat();

const state = {
  items: [],
  summary: {},
  selected: null,
  operation: null,
  restoreFocus: null,
  cursor: null,
  nextCursor: null,
  adapters: [],
  settings: [],
  workloads: [],
  backups: [],
};

const elements = {
  status: document.querySelector("#status"),
  dashboard: document.querySelector("#dashboard-view"),
  items: document.querySelector("#items-view"),
  detail: document.querySelector("#detail-view"),
  adaptersView: document.querySelector("#adapters-view"),
  settingsView: document.querySelector("#settings-view"),
  workloadsView: document.querySelector("#workloads-view"),
  backupsView: document.querySelector("#backups-view"),
  auditView: document.querySelector("#audit-view"),
  summary: document.querySelector("#summary"),
  attention: document.querySelector("#attention-list"),
  itemList: document.querySelector("#item-list"),
  filters: document.querySelector("#filters"),
  loginDialog: document.querySelector("#login-dialog"),
  loginForm: document.querySelector("#login-form"),
  loginError: document.querySelector("#login-error"),
  operationDialog: document.querySelector("#operation-dialog"),
  operationForm: document.querySelector("#operation-form"),
  operationTitle: document.querySelector("#operation-title"),
  operationDescription: document.querySelector("#operation-description"),
  operationFields: document.querySelector("#operation-fields"),
  operationError: document.querySelector("#operation-error"),
  operationSubmit: document.querySelector("#operation-submit"),
  conflictDialog: document.querySelector("#conflict-dialog"),
  detailContent: document.querySelector("#detail-content"),
  adapterList: document.querySelector("#adapter-list"),
  runtimeSettings: document.querySelector("#runtime-settings"),
  bootstrapSettings: document.querySelector("#bootstrap-settings"),
  workloadList: document.querySelector("#workload-list"),
  backupList: document.querySelector("#backup-list"),
  backupUploadForm: document.querySelector("#backup-upload-form"),
  restoreDialog: document.querySelector("#restore-dialog"),
  restoreForm: document.querySelector("#restore-form"),
  restoreError: document.querySelector("#restore-error"),
  restorePassword: document.querySelector("#restore-password"),
  restoreConfirmation: document.querySelector("#restore-confirmation"),
  restoreBackupId: document.querySelector("#restore-backup-id"),
  adminEvents: document.querySelector("#admin-events"),
};

class ApiError extends Error {
  constructor(response, body) {
    super(body?.error?.message || `Request failed (${response.status})`);
    this.status = response.status;
    this.code = body?.error?.code || "REQUEST_FAILED";
    this.details = body?.error?.details || {};
    this.retryable = Boolean(body?.error?.retryable);
  }
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers);
  headers.set("Accept", "application/json");
  if (options.body && !(options.body instanceof FormData)) headers.set("Content-Type", "application/json");
  const csrf = sessionStorage.getItem("remindi-csrf");
  if (csrf && options.method && options.method !== "GET") headers.set("X-CSRF-Token", csrf);
  const response = await fetch(`${API_ROOT}${path}`, { ...options, headers, credentials: "same-origin" });
  const body = response.status === 204 ? { ok: true, data: null } : await response.json().catch(() => null);
  if (!response.ok || body?.ok === false) throw new ApiError(response, body);
  if (body?.data?.csrf_token) sessionStorage.setItem("remindi-csrf", body.data.csrf_token);
  return body?.data ?? body;
}

function mutationBody(data = {}) {
  return JSON.stringify({ ...data, idempotency_key: state.operation?.idempotencyKey || crypto.randomUUID() });
}

function announce(message, error = false) {
  elements.status.textContent = "";
  elements.status.classList.toggle("error", error);
  requestAnimationFrame(() => { elements.status.textContent = message; });
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}

function formatDate(value) {
  if (!value) return "Not scheduled";
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? String(value) : dateTime.format(date);
}

function itemState(item) {
  return String(item.state || item.status || "scheduled").toLowerCase();
}

async function start() {
  bindEvents();
  hydrateFilters();
  try {
    const session = await api("/session");
    if (session?.authenticated === false && session?.auth_required !== false) return openLogin();
    await load();
  } catch (error) {
    if (error.status === 401) openLogin();
    else showPageError(error);
  }
}

function bindEvents() {
  document.addEventListener("click", async (event) => {
    const action = event.target.closest("[data-action]")?.dataset.action;
    const id = event.target.closest("[data-id]")?.dataset.id;
    if (action) {
      event.preventDefault();
      if (action === "details") await showDetail(id);
      else openOperation(action, id, event.target.closest("button"));
    }
    const workload = event.target.closest("[data-workload-action]");
    if (workload) {
      event.preventDefault();
      await controlWorkload(workload.dataset.component, workload.dataset.workloadAction);
    }
    const backup = event.target.closest("[data-backup-action]");
    if (backup) {
      event.preventDefault();
      if (backup.dataset.backupAction === "verify") await verifyBackup(backup.dataset.backupId);
      else openRestore(backup.dataset.backupId, backup);
    }
    if (event.target.closest("[data-close]")) closeOperation();
    if (event.target.closest("[data-restore-close]")) closeRestore();
  });
  window.addEventListener("popstate", () => load());
  elements.loginForm.addEventListener("submit", login);
  elements.operationForm.addEventListener("submit", submitOperation);
  elements.restoreForm.addEventListener("submit", submitRestore);
  elements.backupUploadForm.addEventListener("submit", uploadBackup);
  elements.filters.addEventListener("submit", applyFilters);
  document.querySelector("#clear-filters").addEventListener("click", clearFilters);
  document.querySelector("#logout-button").addEventListener("click", logout);
  document.querySelector("#create-backup").addEventListener("click", createBackup);
  document.querySelector("#detail-back").addEventListener("click", () => navigate({ view: "items", item: null }));
  document.querySelector("#next-page").addEventListener("click", () => {
    if (state.nextCursor) {
      state.cursor = state.nextCursor;
      loadItems();
    }
  });
  document.querySelector("#previous-page").addEventListener("click", () => {
    state.cursor = null;
    loadItems();
  });
  elements.operationDialog.addEventListener("close", restoreFocus);
  elements.restoreDialog.addEventListener("close", () => {
    elements.restoreForm.reset();
    elements.restoreBackupId.value = "";
    clearDialogError(elements.restoreError);
    restoreFocus();
  });
  document.addEventListener("submit", async (event) => {
    if (event.target.matches("[data-setting-form]")) await saveSetting(event);
    if (event.target.matches("[data-adapter-form]")) await saveAdapter(event);
  });
  elements.conflictDialog.addEventListener("close", async () => {
    if (elements.conflictDialog.returnValue === "reload" && state.selected?.id) await showDetail(state.selected.id);
    restoreFocus();
  });
}

async function login(event) {
  event.preventDefault();
  clearDialogError(elements.loginError);
  if (!validateRequired(elements.loginForm)) return;
  const fields = new FormData(elements.loginForm);
  try {
    await api("/auth/login", {
      method: "POST",
      body: JSON.stringify({ username: fields.get("username"), password: fields.get("password") }),
    });
    elements.loginForm.reset();
    elements.loginDialog.close();
    announce("Signed in.");
    await load();
  } catch (error) {
    showDialogError(elements.loginError, error.message);
    document.querySelector("#username").focus();
  }
}

async function logout() {
  try { await api("/auth/logout", { method: "POST", body: "{}" }); } catch (_) { /* session is gone */ }
  sessionStorage.removeItem("remindi-csrf");
  openLogin();
}

function openLogin() {
  if (!elements.loginDialog.open) elements.loginDialog.showModal();
  requestAnimationFrame(() => document.querySelector("#username").focus());
}

async function load() {
  const params = new URLSearchParams(location.search);
  const view = params.get("view") || "dashboard";
  setView(view);
  if (params.get("item")) await showDetail(params.get("item"), false);
  else if (view === "items") await loadItems();
  else if (view === "adapters") await loadAdapters();
  else if (view === "settings") await loadSettings();
  else if (view === "workloads") await loadWorkloads();
  else if (view === "backups") await loadBackups();
  else if (view === "audit") await loadAudit();
  else await loadDashboard();
}

function setView(view) {
  elements.dashboard.hidden = view !== "dashboard";
  elements.items.hidden = view !== "items";
  elements.detail.hidden = view !== "detail";
  elements.adaptersView.hidden = view !== "adapters";
  elements.settingsView.hidden = view !== "settings";
  elements.workloadsView.hidden = view !== "workloads";
  elements.backupsView.hidden = view !== "backups";
  elements.auditView.hidden = view !== "audit";
  document.querySelectorAll("[data-nav]").forEach((link) => {
    if (link.dataset.nav === view) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  });
}

async function loadSettings() {
  try {
    const [settings, bootstrap] = await Promise.all([
      api("/settings"),
      api("/settings/bootstrap"),
    ]);
    state.settings = settings;
    elements.runtimeSettings.innerHTML = `<table>
      <thead><tr><th>Setting</th><th>Value</th><th>Activation</th><th>Action</th></tr></thead>
      <tbody>${settings.map((setting) => `<tr>
        <td data-label="Setting"><code>${escapeHtml(setting.key)}</code></td>
        <td data-label="Value"><form data-setting-form data-key="${escapeHtml(setting.key)}">
          <input name="value" type="number" min="${escapeHtml(setting.minimum)}" ${setting.maximum == null ? "" : `max="${escapeHtml(setting.maximum)}"`} value="${escapeHtml(setting.value)}" required>
          <input name="expected_version" type="hidden" value="${escapeHtml(setting.version)}">
        </form></td>
        <td data-label="Activation">${setting.restart_required ? "Workload restart required" : "Immediate"}</td>
        <td data-label="Action"><button class="button" type="submit" form="${settingFormId(setting.key)}">Save</button></td>
      </tr>`).join("")}</tbody>
    </table>`;
    elements.runtimeSettings.querySelectorAll("[data-setting-form]").forEach((form) => {
      form.id = settingFormId(form.dataset.key);
    });
    elements.bootstrapSettings.innerHTML = `<table>
      <thead><tr><th>Bootstrap setting</th><th>Effective value</th><th>Mutable</th></tr></thead>
      <tbody>${bootstrap.settings.map((setting) => `<tr>
        <td data-label="Setting"><code>${escapeHtml(setting.name)}</code></td>
        <td data-label="Effective value">${escapeHtml(setting.effective_value ?? (setting.configured ? "[configured]" : "Not configured"))}</td>
        <td data-label="Mutable">${setting.mutable ? "Yes" : "No"}</td>
      </tr>`).join("")}</tbody>
    </table>`;
  } catch (error) { showPageError(error); }
}

function settingFormId(key) {
  return `setting-${String(key).replaceAll(".", "-")}`;
}

async function saveSetting(event) {
  event.preventDefault();
  const form = event.target;
  const values = new FormData(form);
  try {
    await api(`/settings/${encodeURIComponent(form.dataset.key)}`, {
      method: "PATCH",
      body: JSON.stringify({
        value: Number(values.get("value")),
        expected_version: Number(values.get("expected_version")),
      }),
    });
    announce("Runtime setting saved.");
    await loadSettings();
  } catch (error) {
    if (error.code === "VERSION_CONFLICT") {
      announce("The setting changed elsewhere. Latest values were reloaded.", true);
      await loadSettings();
    } else showPageError(error);
  }
}

async function loadAdapters() {
  try {
    state.adapters = await api("/adapters");
    elements.adapterList.innerHTML = state.adapters.map((adapter) => `<form class="panel" data-adapter-form data-name="${escapeHtml(adapter.adapter_name)}">
      <div class="panel-heading"><div><p class="eyebrow">${escapeHtml(adapter.configuration.type)}</p><h2>${escapeHtml(adapter.adapter_name)}</h2></div>
        <label><input name="enabled" type="checkbox" ${adapter.enabled ? "checked" : ""}> Enabled</label></div>
      <div class="field">
        <label for="adapter-${escapeHtml(adapter.adapter_name)}">Typed configuration</label>
        <textarea id="adapter-${escapeHtml(adapter.adapter_name)}" name="configuration" rows="10" spellcheck="false" required>${escapeHtml(JSON.stringify(adapter.configuration, null, 2))}</textarea>
        <small class="muted">Only fields valid for this adapter type and allowlisted aliases are accepted.</small>
      </div>
      <input name="expected_version" type="hidden" value="${escapeHtml(adapter.version)}">
      <button class="button primary" type="submit">Save ${escapeHtml(adapter.adapter_name)}</button>
    </form>`).join("");
  } catch (error) { showPageError(error); }
}

async function saveAdapter(event) {
  event.preventDefault();
  const form = event.target;
  const values = new FormData(form);
  let configuration;
  try {
    configuration = JSON.parse(values.get("configuration"));
  } catch (_) {
    announce("Adapter configuration must be valid JSON.", true);
    form.elements.configuration.focus();
    return;
  }
  try {
    await api(`/adapters/${encodeURIComponent(form.dataset.name)}`, {
      method: "PATCH",
      body: JSON.stringify({
        enabled: form.elements.enabled.checked,
        configuration,
        expected_version: Number(values.get("expected_version")),
      }),
    });
    announce("Adapter configuration published.");
    await loadAdapters();
  } catch (error) {
    if (error.code === "VERSION_CONFLICT") {
      announce("The adapter changed elsewhere. Latest values were reloaded.", true);
      await loadAdapters();
    } else showPageError(error);
  }
}

async function loadWorkloads() {
  try {
    state.workloads = await api("/workloads");
    elements.workloadList.innerHTML = state.workloads.map((workload) => `<article class="panel">
      <div class="panel-heading"><div><p class="eyebrow">${escapeHtml(workload.desired)} desired</p><h2>${escapeHtml(workload.component)}</h2></div>
        <span class="badge ${escapeHtml(workload.actual)}">${escapeHtml(workload.actual)}</span></div>
      ${workload.last_error ? `<p class="inline-error">${escapeHtml(workload.last_error)}</p>` : ""}
      <div class="button-row">
        <button class="button" data-workload-action="start" data-component="${escapeHtml(workload.component)}">Start</button>
        <button class="button danger" data-workload-action="stop" data-component="${escapeHtml(workload.component)}">Stop</button>
        <button class="button" data-workload-action="restart" data-component="${escapeHtml(workload.component)}">Restart</button>
      </div>
    </article>`).join("");
  } catch (error) { showPageError(error); }
}

async function controlWorkload(component, action) {
  if (["stop", "restart"].includes(action) && !window.confirm(`${action} the ${component} workload? The control plane will remain available.`)) return;
  try {
    await api(`/workloads/${encodeURIComponent(component)}/${encodeURIComponent(action)}`, {
      method: "POST",
      body: "{}",
    });
    announce(`${component} ${action} completed.`);
    await loadWorkloads();
  } catch (error) { showPageError(error); }
}

async function loadBackups() {
  try {
    state.backups = await api("/backups");
    elements.backupList.innerHTML = state.backups.length ? `<table>
      <thead><tr><th>Created</th><th>Source</th><th>Status</th><th>Size</th><th>Schema</th><th>Actions</th></tr></thead>
      <tbody>${state.backups.map((backup) => `<tr>
        <td data-label="Created">${escapeHtml(formatDate(backup.created_at))}<div class="meta"><code>${escapeHtml(backup.file_name)}</code></div></td>
        <td data-label="Source">${escapeHtml(backup.source)}</td>
        <td data-label="Status"><span class="badge ${escapeHtml(backup.status)}">${escapeHtml(backup.status)}</span></td>
        <td data-label="Size">${escapeHtml(formatBytes(backup.size_bytes))}</td>
        <td data-label="Schema">${escapeHtml(backup.schema_version)}</td>
        <td data-label="Actions"><div class="row-actions">
          <a class="button" href="${API_ROOT}/backups/${encodeURIComponent(backup.id)}/download">Download</a>
          <button class="button" type="button" data-backup-action="verify" data-backup-id="${escapeHtml(backup.id)}">Verify</button>
          <button class="button danger" type="button" data-backup-action="restore" data-backup-id="${escapeHtml(backup.id)}" ${backup.status === "ready" ? "" : "disabled"}>Restore</button>
        </div></td>
      </tr>`).join("")}</tbody>
    </table>` : `<div class="empty"><h2>No backups yet</h2><p>Create a verified manual backup or upload a SQLite database.</p></div>`;
  } catch (error) { showPageError(error); }
}

function formatBytes(value) {
  const bytes = Number(value);
  if (!Number.isFinite(bytes) || bytes < 0) return String(value ?? "");
  if (bytes < 1024) return `${number.format(bytes)} B`;
  const units = ["KiB", "MiB", "GiB"];
  let amount = bytes;
  let unit = -1;
  do {
    amount /= 1024;
    unit += 1;
  } while (amount >= 1024 && unit < units.length - 1);
  return `${number.format(Number(amount.toFixed(1)))} ${units[unit]}`;
}

async function createBackup() {
  try {
    await api("/backups", { method: "POST" });
    announce("Verified backup created.");
    await loadBackups();
  } catch (error) { showPageError(error); }
}

async function uploadBackup(event) {
  event.preventDefault();
  if (!elements.backupUploadForm.reportValidity()) return;
  const data = new FormData(elements.backupUploadForm);
  try {
    await api("/backups/upload", { method: "POST", body: data });
    elements.backupUploadForm.reset();
    announce("Backup uploaded and verified.");
    await loadBackups();
  } catch (error) { showPageError(error); }
}

async function verifyBackup(id) {
  try {
    await api(`/backups/${encodeURIComponent(id)}/verify`, { method: "POST" });
    announce("Backup verification passed.");
    await loadBackups();
  } catch (error) { showPageError(error); }
}

function openRestore(id, invoker) {
  state.restoreFocus = invoker || document.activeElement;
  elements.restoreForm.reset();
  elements.restoreBackupId.value = id;
  clearDialogError(elements.restoreError);
  elements.restoreDialog.showModal();
  requestAnimationFrame(() => elements.restorePassword.focus());
}

function closeRestore() {
  if (elements.restoreDialog.open) elements.restoreDialog.close();
}

async function submitRestore(event) {
  event.preventDefault();
  clearDialogError(elements.restoreError);
  if (!validateRequired(elements.restoreForm)) return;
  if (elements.restoreConfirmation.value !== "RESTORE REMINDI") {
    elements.restoreConfirmation.setAttribute("aria-invalid", "true");
    elements.restoreConfirmation.focus();
    showDialogError(elements.restoreError, "Type RESTORE REMINDI exactly.");
    return;
  }
  const backupId = elements.restoreBackupId.value;
  let password = elements.restorePassword.value;
  elements.restorePassword.value = "";
  const submit = elements.restoreForm.querySelector("[type='submit']");
  submit.disabled = true;
  try {
    await api("/auth/reauthenticate", {
      method: "POST",
      body: JSON.stringify({ password }),
    });
    password = "";
    await api(`/backups/${encodeURIComponent(backupId)}/restore`, {
      method: "POST",
      body: JSON.stringify({ confirmation: "RESTORE REMINDI" }),
    });
    closeRestore();
    announce("Database restored from the verified backup.");
    await loadBackups();
  } catch (error) {
    password = "";
    showDialogError(elements.restoreError, error.message);
  } finally {
    password = "";
    submit.disabled = false;
  }
}

async function loadAudit() {
  try {
    const events = await api("/admin-events?limit=200");
    elements.adminEvents.innerHTML = events.length ? `<table>
      <thead><tr><th>Time</th><th>Action</th><th>Outcome</th><th>Actor</th><th>Details</th></tr></thead>
      <tbody>${events.map((event) => `<tr>
        <td data-label="Time">${escapeHtml(formatDate(event.occurred_at))}</td>
        <td data-label="Action">${escapeHtml(event.event_type)}</td>
        <td data-label="Outcome">${escapeHtml(event.outcome)}</td>
        <td data-label="Actor"><code>${escapeHtml(event.actor_id)}</code></td>
        <td data-label="Details"><code>${escapeHtml(JSON.stringify(event.details))}</code></td>
      </tr>`).join("")}</tbody>
    </table>` : `<div class="empty"><h2>No administrative events</h2><p>Configuration and workload changes will appear here.</p></div>`;
  } catch (error) { showPageError(error); }
}

async function loadDashboard() {
  try {
    const result = await api("/remindi?limit=100");
    state.items = normaliseItems(result);
    state.summary = countStates(state.items);
    renderSummary();
    renderTable(elements.attention, state.items.filter((item) => ["due", "overdue"].includes(itemState(item))).slice(0, 5), true);
  } catch (error) { showPageError(error); }
}

async function loadItems() {
  const params = new URLSearchParams(location.search);
  const query = new URLSearchParams({ limit: "25" });
  const projectId = params.get("project_id");
  const states = params.get("state");
  if (projectId) query.set("project_id", projectId);
  if (states) query.set("states", states);
  if (state.cursor) query.set("cursor", state.cursor);
  try {
    const result = await api(`/remindi?${query}`);
    const search = (params.get("q") || "").trim().toLocaleLowerCase();
    state.items = normaliseItems(result).filter((item) => !search || [
      item.message,
      item.project_id,
      item.task_id,
    ].some((value) => String(value || "").toLocaleLowerCase().includes(search)));
    state.nextCursor = result?.next_cursor || null;
    renderTable(elements.itemList, state.items);
    document.querySelector("#next-page").disabled = !state.nextCursor;
    document.querySelector("#previous-page").disabled = !state.cursor;
  } catch (error) { showPageError(error); }
}

function normaliseItems(result) {
  return Array.isArray(result) ? result : result?.items || result?.results || [];
}

function countStates(items) {
  const counts = Object.fromEntries(["scheduled", "due", "overdue", "snoozed", "completed", "cancelled"].map((key) => [key, 0]));
  for (const item of items) counts[itemState(item)] = (counts[itemState(item)] || 0) + 1;
  return counts;
}

function renderSummary() {
  elements.summary.innerHTML = Object.entries(state.summary).map(([label, count]) => `
    <article class="summary-card">
      <span>${escapeHtml(label)}</span><strong>${number.format(count)}</strong>
    </article>`).join("");
}

function renderTable(container, items, compact = false) {
  if (!items.length) {
    container.innerHTML = `<div class="empty"><h3>Nothing here yet</h3><p>${compact ? "No due or overdue items." : "Change the filters or add your first Remindi item."}</p></div>`;
    return;
  }
  container.innerHTML = `<table>
    <thead><tr><th class="title-cell">Item</th><th>State</th><th>Priority</th><th>Next fire</th><th>Actions</th></tr></thead>
    <tbody>${items.map((item) => `<tr>
      <td class="title-cell" data-label="Item"><button class="back-link item-title" data-action="details" data-id="${escapeHtml(item.id)}">${escapeHtml(item.message)}</button><div class="meta">${escapeHtml(item.project_id || "No project")}${item.task_id ? ` · ${escapeHtml(item.task_id)}` : ""}</div></td>
      <td data-label="State"><span class="badge ${escapeHtml(itemState(item))}">${escapeHtml(itemState(item))}</span></td>
      <td data-label="Priority">${escapeHtml(item.priority ?? 0)}</td>
      <td data-label="Next fire">${escapeHtml(formatDate(item.next_fire_at || item.snooze_until))}</td>
      <td data-label="Actions"><div class="row-actions">
        <button class="button" data-action="snooze" data-id="${escapeHtml(item.id)}">Snooze</button>
        <button class="button" data-action="complete" data-id="${escapeHtml(item.id)}">Complete</button>
        <button class="button quiet" data-action="update" data-id="${escapeHtml(item.id)}">Edit</button>
        <button class="button danger" data-action="cancel" data-id="${escapeHtml(item.id)}">Cancel</button>
      </div></td>
    </tr>`).join("")}</tbody>
  </table>`;
}

async function showDetail(id, push = true) {
  try {
    const item = await api(`/remindi/${encodeURIComponent(id)}`);
    const history = await api(`/remindi/${encodeURIComponent(id)}/history`);
    state.selected = item?.item || item;
    if (push) navigate({ view: "detail", item: id }, false);
    setView("detail");
    elements.detailContent.innerHTML = `
      <div class="heading-row"><div><p class="eyebrow">${escapeHtml(itemState(state.selected))}</p><h1 id="detail-title">${escapeHtml(state.selected.message)}</h1></div>
        <div class="button-row"><button class="button" data-action="update" data-id="${escapeHtml(id)}">Edit</button><button class="button" data-action="complete" data-id="${escapeHtml(id)}">Complete</button></div></div>
      <div class="detail-grid">
        <section class="panel"><h2>Details</h2><dl class="detail-list">
          <dt>ID</dt><dd>${escapeHtml(id)}</dd><dt>Project</dt><dd>${escapeHtml(state.selected.project_id || "None")}</dd>
          <dt>Task</dt><dd>${escapeHtml(state.selected.task_id || "None")}</dd><dt>Priority</dt><dd>${escapeHtml(state.selected.priority ?? 0)}</dd>
          <dt>Next fire</dt><dd>${escapeHtml(formatDate(state.selected.next_fire_at || state.selected.snooze_until))}</dd><dt>Version</dt><dd>${escapeHtml(state.selected.version)}</dd>
          <dt>Instructions</dt><dd>${escapeHtml(state.selected.instructions || "No instructions")}</dd>
        </dl></section>
        <section class="panel"><h2>History</h2><ol class="history-list">${normaliseHistory(history).map((entry) => `<li><strong>${escapeHtml(entry.event_type || entry.type || "changed")}</strong><div class="meta">${escapeHtml(formatDate(entry.created_at || entry.occurred_at))}</div><p>${escapeHtml(entry.details?.reason || entry.reason || "")}</p></li>`).join("") || "<li>No history recorded.</li>"}</ol></section>
      </div>`;
  } catch (error) { showPageError(error); }
}

function normaliseHistory(result) {
  return Array.isArray(result) ? result : result?.events || result?.history || [];
}

function openOperation(kind, id, invoker) {
  const item = state.items.find((candidate) => candidate.id === id) || (state.selected?.id === id ? state.selected : null);
  state.restoreFocus = invoker || document.activeElement;
  state.operation = { kind, item, idempotencyKey: crypto.randomUUID() };
  clearDialogError(elements.operationError);
  elements.operationTitle.textContent = operationTitle(kind);
  elements.operationDescription.textContent = operationDescription(kind);
  elements.operationFields.innerHTML = operationFields(kind, item);
  elements.operationSubmit.textContent = kind === "add" ? "Add item" : operationTitle(kind);
  elements.operationSubmit.classList.toggle("danger", kind === "cancel");
  elements.operationDialog.showModal();
  requestAnimationFrame(() => elements.operationFields.querySelector("input, select, textarea")?.focus());
}

function closeOperation() {
  if (elements.operationDialog.open) elements.operationDialog.close();
}

function restoreFocus() {
  state.restoreFocus?.focus?.();
  state.restoreFocus = null;
}

function operationTitle(kind) {
  return ({ add: "Add Remindi item", check: "Check ready items", update: "Edit item", complete: "Complete item", snooze: "Snooze item", cancel: "Cancel item" })[kind] || "Action";
}

function operationDescription(kind) {
  return ({ add: "Create a reminder with a time trigger.", check: "Evaluate what is ready now.", update: "Save changes against the current item version.", complete: "Record structured evidence for this occurrence.", snooze: "Temporarily defer this occurrence.", cancel: "Cancel this item. This action is recorded in history." })[kind] || "";
}

function field(name, label, value = "", options = {}) {
  const id = `operation-${name}`;
  const described = `${id}-help`;
  if (options.type === "textarea") return `<div class="field"><label for="${id}">${label}</label><textarea id="${id}" name="${name}" ${options.required ? "required" : ""} aria-describedby="${described}">${escapeHtml(value)}</textarea>${options.help ? `<small id="${described}" class="muted">${escapeHtml(options.help)}</small>` : ""}</div>`;
  return `<div class="field"><label for="${id}">${label}</label><input id="${id}" name="${name}" type="${options.type || "text"}" value="${escapeHtml(value)}" ${options.required ? "required" : ""} ${options.min !== undefined ? `min="${options.min}"` : ""} aria-describedby="${described}">${options.help ? `<small id="${described}" class="muted">${escapeHtml(options.help)}</small>` : ""}</div>`;
}

function selectField(name, label, value, choices, options = {}) {
  const id = `operation-${name}`;
  return `<div class="field"><label for="${id}">${label}</label><select id="${id}" name="${name}" ${options.required ? "required" : ""}>${choices.map((choice) => `<option value="${escapeHtml(choice)}" ${choice === value ? "selected" : ""}>${escapeHtml(choice.replaceAll("_", " "))}</option>`).join("")}</select></div>`;
}

function operationFields(kind, item) {
  const expected = item ? `<input type="hidden" name="expected_version" value="${escapeHtml(item.version)}">` : "";
  if (kind === "add") return [
    field("message", "Message", "", { required: true }),
    field("project_id", "Project", "", { required: true }),
    field("task_id", "Task (optional)"),
    selectField("priority", "Priority", "normal", ["low", "normal", "high", "critical"], { required: true }),
    field("due_at", "Due at", "", { type: "datetime-local", required: true }),
    field("instructions", "Instructions (optional)", "", { type: "textarea", help: "Long text wraps without hiding actions." }),
  ].join("");
  if (kind === "check") return [
    field("project_id", "Project", "", { required: true }),
    field("task_id", "Task (optional)"),
    selectField("lifecycle_event", "Lifecycle event", "checkpoint", ["task_start", "checkpoint", "continuation", "final_review"], { required: true }),
    field("limit", "Maximum items", "25", { type: "number", min: 1 }),
  ].join("");
  if (kind === "update") return [
    expected,
    field("message", "Message", item?.message, { required: true }),
    selectField("priority", "Priority", item?.priority || "normal", ["low", "normal", "high", "critical"], { required: true }),
    field("instructions", "Instructions", item?.instructions, { type: "textarea" }),
    field("reason", "Reason for change", "", { required: true }),
  ].join("");
  if (kind === "complete") return expected + field("summary", "What was observed?", "", { required: true, type: "textarea" }) + field("reference_uri", "Stable reference URI", "", { required: true }) + field("observed_at", "Observed at", toLocalDateTime(new Date()), { required: true, type: "datetime-local" });
  if (kind === "snooze") return expected + field("snooze_until", "Snooze until", "", { required: true, type: "datetime-local" }) + field("reason", "Reason", "", { required: true });
  if (kind === "cancel") return expected + field("reason", "Reason for cancellation", "", { required: true, type: "textarea" });
  return expected;
}

function toLocalDateTime(date) {
  const offset = date.getTimezoneOffset() * 60_000;
  return new Date(date.valueOf() - offset).toISOString().slice(0, 16);
}

async function submitOperation(event) {
  event.preventDefault();
  clearDialogError(elements.operationError);
  if (!validateRequired(elements.operationForm)) return;
  const values = Object.fromEntries(new FormData(elements.operationForm));
  const { kind, item } = state.operation;
  const id = item?.id;
  let path;
  let method = "POST";
  let body;
  if (kind === "add") {
    path = "/remindi";
    body = { message: values.message, project_id: values.project_id, task_id: values.task_id || null, priority: values.priority, instructions: values.instructions || null, trigger: { type: "at_time", at: new Date(values.due_at).toISOString() } };
  } else if (kind === "check") {
    path = "/remindi/check";
    body = { project_id: values.project_id, task_id: values.task_id || null, lifecycle_event: values.lifecycle_event, limit: Number(values.limit) };
  } else if (kind === "update") {
    path = `/remindi/${encodeURIComponent(id)}`; method = "PATCH";
    body = { expected_version: Number(values.expected_version), message: values.message, priority: values.priority, instructions: values.instructions || null, reason: values.reason };
  } else if (kind === "complete") {
    path = `/remindi/${encodeURIComponent(id)}/complete`;
    body = { expected_version: Number(values.expected_version), evidence: { type: "observation", summary: values.summary, reference_uri: values.reference_uri, observed_at: new Date(values.observed_at).toISOString() } };
  } else if (kind === "snooze") {
    path = `/remindi/${encodeURIComponent(id)}/snooze`;
    body = { expected_version: Number(values.expected_version), snooze_until: new Date(values.snooze_until).toISOString(), reason: values.reason };
  } else {
    path = `/remindi/${encodeURIComponent(id)}/cancel`;
    body = { expected_version: Number(values.expected_version), reason: values.reason };
  }
  try {
    const payload = kind === "check" ? JSON.stringify(body) : mutationBody(body);
    const result = await api(path, { method, body: payload });
    closeOperation();
    announce(kind === "check" ? `${number.format(normaliseItems(result).length)} item(s) ready.` : `${operationTitle(kind)} succeeded.`);
    await load();
  } catch (error) {
    if (error.code === "VERSION_CONFLICT") {
      closeOperation();
      elements.conflictDialog.showModal();
      return;
    }
    showDialogError(elements.operationError, `${error.message}${error.retryable ? " You can retry safely." : ""}`);
  }
}

function validateRequired(form) {
  form.querySelectorAll("[aria-invalid='true']").forEach((field) => field.removeAttribute("aria-invalid"));
  const invalid = [...form.querySelectorAll("[required]")].find((field) => !field.value.trim());
  if (!invalid) return true;
  invalid.setAttribute("aria-invalid", "true");
  invalid.focus();
  const target = form === elements.loginForm
    ? elements.loginError
    : form === elements.restoreForm
      ? elements.restoreError
      : elements.operationError;
  showDialogError(target, `${invalid.labels?.[0]?.textContent || "This field"} is required.`);
  return false;
}

function showDialogError(element, message) {
  element.textContent = message;
  element.hidden = false;
}
function clearDialogError(element) {
  element.textContent = "";
  element.hidden = true;
}
function showPageError(error) {
  if (error.status === 401) return openLogin();
  announce(`${error.message}${error.retryable ? " Try again." : ""}`, true);
}

function applyFilters(event) {
  event.preventDefault();
  const values = new FormData(elements.filters);
  navigate({ view: "items", q: values.get("q"), state: values.get("state"), project_id: values.get("project_id"), item: null });
}
function clearFilters() {
  elements.filters.reset();
  navigate({ view: "items", q: null, state: null, project_id: null, item: null });
}
function hydrateFilters() {
  const params = new URLSearchParams(location.search);
  for (const key of ["q", "state", "project_id"]) {
    const field = elements.filters.elements.namedItem(key);
    if (field) field.value = params.get(key) || "";
  }
}
function navigate(changes, shouldLoad = true) {
  const params = new URLSearchParams(location.search);
  for (const [key, value] of Object.entries(changes)) {
    if (value) params.set(key, value);
    else params.delete(key);
  }
  history.pushState({}, "", `${location.pathname}?${params}`);
  if (shouldLoad) load();
}

start();
