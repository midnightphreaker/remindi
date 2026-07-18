const API_ROOT = "/api/v1";
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
};

const elements = {
  status: document.querySelector("#status"),
  dashboard: document.querySelector("#dashboard-view"),
  items: document.querySelector("#items-view"),
  detail: document.querySelector("#detail-view"),
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
    if (event.target.closest("[data-close]")) closeOperation();
  });
  window.addEventListener("popstate", () => load());
  elements.loginForm.addEventListener("submit", login);
  elements.operationForm.addEventListener("submit", submitOperation);
  elements.filters.addEventListener("submit", applyFilters);
  document.querySelector("#clear-filters").addEventListener("click", clearFilters);
  document.querySelector("#logout-button").addEventListener("click", logout);
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
  else await loadDashboard();
}

function setView(view) {
  elements.dashboard.hidden = view !== "dashboard";
  elements.items.hidden = view !== "items";
  elements.detail.hidden = view !== "detail";
  document.querySelectorAll("[data-nav]").forEach((link) => {
    if (link.dataset.nav === view) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  });
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
  for (const key of ["q", "state", "project_id"]) {
    const value = params.get(key);
    if (value) query.set(key, value);
  }
  if (state.cursor) query.set("cursor", state.cursor);
  try {
    const result = await api(`/remindi?${query}`);
    state.items = normaliseItems(result);
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
      <td class="title-cell" data-label="Item"><button class="back-link item-title" data-action="details" data-id="${escapeHtml(item.id)}">${escapeHtml(item.title)}</button><div class="meta">${escapeHtml(item.project_id || "No project")}${item.task_id ? ` · ${escapeHtml(item.task_id)}` : ""}</div></td>
      <td data-label="State"><span class="badge ${escapeHtml(itemState(item))}">${escapeHtml(itemState(item))}</span></td>
      <td data-label="Priority">${escapeHtml(item.priority ?? 0)}</td>
      <td data-label="Next fire">${escapeHtml(formatDate(item.next_fire_at || item.due_at || item.snoozed_until))}</td>
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
      <div class="heading-row"><div><p class="eyebrow">${escapeHtml(itemState(state.selected))}</p><h1 id="detail-title">${escapeHtml(state.selected.title)}</h1></div>
        <div class="button-row"><button class="button" data-action="update" data-id="${escapeHtml(id)}">Edit</button><button class="button" data-action="complete" data-id="${escapeHtml(id)}">Complete</button></div></div>
      <div class="detail-grid">
        <section class="panel"><h2>Details</h2><dl class="detail-list">
          <dt>ID</dt><dd>${escapeHtml(id)}</dd><dt>Project</dt><dd>${escapeHtml(state.selected.project_id || "None")}</dd>
          <dt>Task</dt><dd>${escapeHtml(state.selected.task_id || "None")}</dd><dt>Priority</dt><dd>${escapeHtml(state.selected.priority ?? 0)}</dd>
          <dt>Next fire</dt><dd>${escapeHtml(formatDate(state.selected.next_fire_at))}</dd><dt>Version</dt><dd>${escapeHtml(state.selected.version)}</dd>
          <dt>Notes</dt><dd>${escapeHtml(state.selected.notes || "No notes")}</dd>
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
  const described = `${name}-help`;
  if (options.type === "textarea") return `<div class="field"><label for="${name}">${label}</label><textarea id="${name}" name="${name}" ${options.required ? "required" : ""} aria-describedby="${described}">${escapeHtml(value)}</textarea>${options.help ? `<small id="${described}" class="muted">${escapeHtml(options.help)}</small>` : ""}</div>`;
  return `<div class="field"><label for="${name}">${label}</label><input id="${name}" name="${name}" type="${options.type || "text"}" value="${escapeHtml(value)}" ${options.required ? "required" : ""} ${options.min !== undefined ? `min="${options.min}"` : ""} aria-describedby="${described}">${options.help ? `<small id="${described}" class="muted">${escapeHtml(options.help)}</small>` : ""}</div>`;
}

function operationFields(kind, item) {
  const expected = item ? `<input type="hidden" name="expected_version" value="${escapeHtml(item.version)}">` : "";
  if (kind === "add") return [
    field("title", "Title", "", { required: true }),
    field("project_id", "Project", "", { required: true }),
    field("task_id", "Task (optional)"),
    field("priority", "Priority", "0", { type: "number", min: -100 }),
    field("due_at", "Due at", "", { type: "datetime-local", required: true }),
    field("notes", "Notes (optional)", "", { type: "textarea", help: "Long text wraps without hiding actions." }),
  ].join("");
  if (kind === "check") return field("limit", "Maximum items", "25", { type: "number", min: 1 });
  if (kind === "update") return expected + field("title", "Title", item?.title, { required: true }) + field("priority", "Priority", item?.priority ?? 0, { type: "number", min: -100 }) + field("notes", "Notes", item?.notes, { type: "textarea" });
  if (kind === "complete") return expected + field("summary", "What was observed?", "", { required: true, type: "textarea" }) + field("reference", "Stable reference URI or SHA-256", "", { required: true }) + field("observed_at", "Observed at", toLocalDateTime(new Date()), { required: true, type: "datetime-local" });
  if (kind === "snooze") return expected + field("until", "Snooze until", "", { required: true, type: "datetime-local" }) + field("reason", "Reason", "", { required: true });
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
    body = { title: values.title, project_id: values.project_id, task_id: values.task_id || null, priority: Number(values.priority), notes: values.notes || null, trigger: { type: "time", at: new Date(values.due_at).toISOString() } };
  } else if (kind === "check") {
    path = "/remindi/check";
    body = { limit: Number(values.limit) };
  } else if (kind === "update") {
    path = `/remindi/${encodeURIComponent(id)}`; method = "PATCH";
    body = { expected_version: Number(values.expected_version), title: values.title, priority: Number(values.priority), notes: values.notes || null };
  } else if (kind === "complete") {
    path = `/remindi/${encodeURIComponent(id)}/complete`;
    body = { expected_version: Number(values.expected_version), evidence: { type: "manual_verification", summary: values.summary, observed_at: new Date(values.observed_at).toISOString(), references: [values.reference] } };
  } else if (kind === "snooze") {
    path = `/remindi/${encodeURIComponent(id)}/snooze`;
    body = { expected_version: Number(values.expected_version), until: new Date(values.until).toISOString(), reason: values.reason };
  } else {
    path = `/remindi/${encodeURIComponent(id)}/cancel`;
    body = { expected_version: Number(values.expected_version), reason: values.reason };
  }
  try {
    const result = await api(path, { method, body: mutationBody(body) });
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
  const target = form === elements.loginForm ? elements.loginError : elements.operationError;
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
