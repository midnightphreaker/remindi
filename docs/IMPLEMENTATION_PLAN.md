# Remindi Version 1 Implementation Plan

**Goal:** Implement and verify the version 1 Remindi service defined by
`SPEC.md` and structured by `DESIGN.md`.

**Architecture:** One Rust binary and one container serve the Axum control
plane, rmcp Streamable HTTP endpoint, embedded WebUI, JSON API, scheduler, and
backup/restore functions over one SQLite database. MCP and WebUI operations use
the same service layer.

**Stack:** Use the crate families and production shape in `DESIGN.md` Sections
4–5. Resolve compatible releases during the foundation phase and commit the
resulting `Cargo.lock`; this plan does not add independent version pins.

## 1. Scope and execution rules

- `SPEC.md` is the behavioral and public-contract authority.
- `DESIGN.md` supplies the component boundaries, project layout, and eight-phase
  implementation order. If it conflicts with `SPEC.md`, follow `SPEC.md`.
- Implement only version 1 scope from `SPEC.md` Section 3. Do not implement its
  roadmap items or any other future feature.
- Preserve the single-owner, single-binary, single-container, single-listener,
  and single-database design.
- Do not add arbitrary execution, delivery channels, a plugin system, calendar
  recurrence, multi-writer storage, container control, a frontend framework, an
  external browser dependency, or another production runtime.
- Copy public schemas, routes, tables, settings, errors, security rules, and
  backup/restore behavior directly from the governing sections. Do not create
  competing inventories in this plan.
- Keep clock, ID generation, network resolution, HTTP execution, filesystem
  roots, and adapter results replaceable in tests as required by `DESIGN.md`
  Section 23.2.
- Name or tag tests with applicable `G-*` and `FR-*` identifiers so acceptance
  evidence remains traceable.
- For each task, add the focused failing check first where practical, implement
  the smallest passing change, and run the listed checks.
- Finish phases in order. A phase passes only after its gate succeeds.
- Keep `SPEC.md` and `DESIGN.md` unchanged unless the owner separately approves
  a source change.

Version 1 is done only when `SPEC.md` Sections 27–28 pass against the built
container, including real MCP, browser, restart, security, performance, backup,
and restore verification.

## 2. Phase 1 — Foundation

### Task 1: Scaffold the crate and control plane

**Source:** `SPEC.md` Sections 3, 7–8, 17, and 19–21; `DESIGN.md` Sections 2–8
and 20.

**Files:**

- Create: `Cargo.toml`
- Create: `Cargo.lock`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/app.rs`
- Create: `src/config.rs`
- Create: `src/error.rs`
- Create: `src/clock.rs`
- Create: `src/http/mod.rs`
- Create: `src/http/router.rs`
- Create: `src/http/middleware.rs`
- Create: `src/http/health.rs`
- Create: `tests/contract.rs`
- Create: `tests/contract/foundation.rs`

**Steps:**

- [ ] Create one library target for integration tests and one production binary.
- [ ] Add only the dependency families required by `DESIGN.md` Section 5.
- [ ] Prove the selected rmcp Streamable HTTP, Axum, Schemars, and SQLx APIs
      compile together before feature implementation.
- [ ] Parse and validate the bootstrap environment contract from `SPEC.md`
      Section 21, retaining secrets in secret-bearing types and redacting them
      from errors and logs.
- [ ] Add deterministic clock and ID seams without introducing a general
      dependency-injection framework.
- [ ] Add structured errors, request IDs, JSON tracing, and the content-privacy
      defaults from `SPEC.md` Sections 19–20.
- [ ] Assemble the single Axum listener and minimal liveness/readiness shell.
- [ ] Add graceful process shutdown; workload-specific draining is completed in
      later phases.

**Targeted check:**

```bash
cargo test --test contract foundation
```

### Task 2: Add SQLite migrations and database ownership

**Source:** `SPEC.md` Sections 9, 18, 21, and 22.1–22.2/22.6; `DESIGN.md`
Sections 6 and 10.

**Files:**

- Create: `migrations/0001_initial.sql`
- Create: `migrations/0002_admin_webui.sql`
- Create: `src/db/mod.rs`
- Create: `src/db/manager.rs`
- Create: `src/db/migrations.rs`
- Create: `src/db/transactions.rs`
- Create: `tests/database.rs`
- Create: `tests/database/foundation.rs`

**Steps:**

- [ ] Implement the exact schema, indexes, constraints, and initial rows from
      `SPEC.md` Section 9.
- [ ] Open SQLite with the required WAL, foreign-key, busy-timeout, and
      synchronous settings.
- [ ] Validate data-path ownership and permissions before normal operation.
- [ ] Apply ordered migrations, record them in `schema_migrations`, refuse an
      unknown newer schema, and run the required integrity checks.
- [ ] Make `DatabaseManager` the only owner of the SQLx pool and maintenance
      gate.
- [ ] Add short transaction helpers for atomic state, event, evidence, and
      idempotency writes.
- [ ] Test a fresh database, supported upgrade path, constraints, pragmas,
      migration drift, clean close, and startup rejection cases.

**Targeted check:**

```bash
cargo test --test database foundation
```

### Phase 1 gate

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

The gate passes when an empty data directory starts, migrates, reports health,
and shuts down cleanly with no feature code outside the foundation boundary.

## 3. Phase 2 — Remindi core

### Task 3: Implement domain rules and deterministic evaluation

**Source:** `SPEC.md` Sections 9–12 and 15; `DESIGN.md` Sections 9 and 12.

**Files:**

- Create: `src/remindi/mod.rs`
- Create: `src/remindi/model.rs`
- Create: `src/remindi/state_machine.rs`
- Create: `src/remindi/recurrence.rs`
- Create: `src/remindi/evidence.rs`
- Create: `src/triggers/mod.rs`
- Create: `src/triggers/evaluator.rs`
- Create: `tests/database/domain.rs`

**Steps:**

- [ ] Model the exact Remindi, trigger, recurrence, evidence, context, and event
      values from the source documents.
- [ ] Normalize accepted timestamps to UTC and enforce the documented precision
      and validation boundary.
- [ ] Implement ready/overdue evaluation for time, elapsed, interval,
      next-session, next-continuation, goal-active, and condition triggers.
- [ ] Implement the state machine, including soft cancellation, ready-only
      snooze, terminal-state rejection, and non-consuming checks.
- [ ] Implement fixed recurrence from the scheduled occurrence with coalesce,
      catch-up, and skip policies.
- [ ] Validate structured completion evidence without treating trigger or
      adapter satisfaction as proof of completion.
- [ ] Cover boundary times, invalid transitions, recurrence gaps, lifecycle
      context, and evidence rules with deterministic tests.

**Targeted check:**

```bash
cargo test --test database domain
```

### Task 4: Implement repository and service transactions

**Source:** `SPEC.md` Sections 6, 9.3, 14.1, and 18–20; `DESIGN.md` Sections
3.1, 9–10, and 19.

**Files:**

- Create: `src/remindi/repository.rs`
- Create: `src/remindi/service.rs`
- Create: `tests/database/repository.rs`
- Create: `tests/database/concurrency.rs`

**Steps:**

- [ ] Scope every query and mutation to the configured owner; never accept an
      owner selector from a caller.
- [ ] Implement add, check, complete, snooze, update, list, cancel, and history
      over one `RemindiService`.
- [ ] Atomically write every state change with its immutable event and any
      evidence or idempotency record.
- [ ] Enforce expected versions and make idempotency replays return the original
      result while rejecting a changed request under the same key.
- [ ] Implement the source-defined filters, ordering, pagination, and cursor
      integrity without leaking internal database details.
- [ ] Keep adapter and filesystem work outside write transactions.
- [ ] Test retry replay, conflicting key reuse, racing versions, immutable
      history, restart persistence, query plans, and concurrent readers/writers.

**Targeted check:**

```bash
cargo test --test database repository
cargo test --test database concurrency
```

### Phase 2 gate

```bash
cargo test --test database
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

The gate passes when `FR-01`–`FR-20` and the single-owner parts of
`FR-23`/`FR-25` have deterministic unit and database evidence.

## 4. Phase 3 — MCP

### Task 5: Define the eight MCP tools

**Source:** `SPEC.md` Section 14; `DESIGN.md` Section 11.

**Files:**

- Create: `src/mcp/mod.rs`
- Create: `src/mcp/schemas.rs`
- Create: `src/mcp/responses.rs`
- Create: `src/mcp/tools/mod.rs`
- Create: `src/mcp/tools/add.rs`
- Create: `src/mcp/tools/check.rs`
- Create: `src/mcp/tools/complete.rs`
- Create: `src/mcp/tools/snooze.rs`
- Create: `src/mcp/tools/update.rs`
- Create: `src/mcp/tools/list.rs`
- Create: `src/mcp/tools/cancel.rs`
- Create: `src/mcp/tools/history.rs`
- Create: `tests/contract/mcp_tools.rs`

**Steps:**

- [ ] Define exactly the eight source-specified tools and no others.
- [ ] Generate and validate their Draft 2020-12-compatible input and output
      schemas from typed DTOs.
- [ ] Reject unknown fields and ensure no schema exposes `owner_id`.
- [ ] Apply the exact tool annotations, success response, error envelope, and
      retryability rules from `SPEC.md` Sections 14 and 19.
- [ ] Convert transport DTOs once and call the shared `RemindiService`; do not
      duplicate lifecycle logic in handlers.
- [ ] Contract-test discovery, positive and negative payloads, structured
      results, annotations, and schema stability.

**Targeted check:**

```bash
cargo test --test contract mcp_tools
```

### Task 6: Serve MCP over authenticated Streamable HTTP

**Source:** `SPEC.md` Sections 6.4, 14, 17, and 22.5; `DESIGN.md` Sections 7–8,
11, and 17.1.

**Files:**

- Create: `src/auth/mod.rs`
- Create: `src/auth/mcp.rs`
- Create: `src/mcp/server.rs`
- Update: `src/http/router.rs`
- Update: `src/http/middleware.rs`
- Update: `src/app.rs`
- Create: `tests/contract/mcp_transport.rs`

**Steps:**

- [ ] Mount rmcp Streamable HTTP only at `/mcp`.
- [ ] Require the dedicated bearer token using constant-time comparison and
      enforce the documented Host/Origin policy.
- [ ] Keep logical work-session IDs in tool input separate from MCP transport
      session IDs.
- [ ] Map protocol/authentication failures at the HTTP boundary and business
      failures through the structured tool result.
- [ ] Make the MCP workload startable, drainable, and stoppable in process while
      leaving the control plane alive.
- [ ] Test initialize, tool discovery, tool calls, invalid authentication,
      transport behavior, disconnects, drain, and restart.

**Targeted check:**

```bash
cargo test --test contract mcp_transport
```

### Phase 3 gate

```bash
cargo test --test contract
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

The gate passes when all eight tools satisfy `FR-21`–`FR-24` through real
Streamable HTTP contract tests.

## 5. Phase 4 — Scheduler and adapters

### Task 7: Implement the four contained adapters

**Source:** `SPEC.md` Sections 11.8–11.9, 13, 17.5, and 23.4; `DESIGN.md`
Section 13.

**Files:**

- Create: `src/triggers/adapters/mod.rs`
- Create: `src/triggers/adapters/observation_window.rs`
- Create: `src/triggers/adapters/http_health.rs`
- Create: `src/triggers/adapters/tcp_reachable.rs`
- Create: `src/triggers/adapters/file_exists.rs`
- Create: `tests/adapters.rs`
- Create: `tests/adapters/functional.rs`
- Create: `tests/adapters/containment.rs`

**Steps:**

- [ ] Register exactly `observation_window_ended`, `http_health`,
      `tcp_reachable`, and `file_exists`.
- [ ] Accept only named configured aliases from Remindi items; reject arbitrary
      URL, host, port, IP, path, SQL, or command input.
- [ ] Implement the exact result contract and manual-verification fallback.
- [ ] Enforce adapter deadlines, cancellation, bounded output, redaction, TLS,
      redirects, DNS re-resolution, network-address policy, and path
      containment from the source documents.
- [ ] Ensure evaluation is read-only and occurs outside database write
      transactions.
- [ ] Test all four adapters plus disabled, malformed, timeout, cancellation,
      SSRF, redirect, DNS, TLS, response-size, and filesystem escape cases.

**Targeted check:**

```bash
cargo test --test adapters
```

### Task 8: Implement scheduler evaluation and lease

**Source:** `SPEC.md` Sections 6.2, 11, 18.2, and 22.5; `DESIGN.md` Sections 12
and 17.2.

**Files:**

- Create: `src/scheduler/mod.rs`
- Create: `src/scheduler/lease.rs`
- Create: `src/scheduler/runner.rs`
- Update: `src/triggers/evaluator.rs`
- Update: `src/app.rs`
- Create: `tests/e2e.rs`
- Create: `tests/e2e/scheduler.rs`

**Steps:**

- [ ] Select scheduler candidates using the source-defined readiness and
      condition rules.
- [ ] Acquire and renew the single-host scheduler lease so two loops cannot
      evaluate concurrently.
- [ ] Evaluate adapters outside write transactions, then apply results through
      the same repository/service invariants.
- [ ] Start the scheduler by default and support cancellation, clean lease
      release, restart, and persisted desired state hooks.
- [ ] Keep MCP pull checks fully functional while the scheduler is stopped.
- [ ] Test deterministic polls, lease loss, overlapping candidates, adapter
      failure isolation, restart, and process recovery.

**Targeted check:**

```bash
cargo test --test e2e scheduler
```

### Phase 4 gate

```bash
cargo test --test adapters
cargo test --test e2e scheduler
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

The gate passes when all trigger classes and all four adapters meet
`FR-09`–`FR-15`, `G-06`, `G-07`, and `G-10`, including containment and restart
tests.

## 6. Phase 5 — WebUI Remindi surface

### Task 9: Add browser authentication and the Remindi JSON API

**Source:** `SPEC.md` Sections 14.12, 17.6, and 21; `DESIGN.md` Sections 7–8 and
15.

**Files:**

- Create: `src/auth/web_session.rs`
- Create: `src/auth/csrf.rs`
- Create: `src/http/api/mod.rs`
- Create: `src/http/api/remindi.rs`
- Update: `src/http/router.rs`
- Update: `src/http/middleware.rs`
- Create: `tests/webui.rs`
- Create: `tests/webui/auth.rs`
- Create: `tests/webui/api.rs`

**Steps:**

- [ ] Implement the enabled/authenticated, enabled/unauthenticated, and disabled
      WebUI modes exactly as specified.
- [ ] Implement the application-rendered login flow with an HttpOnly SameSite
      session cookie and no browser-native Basic Auth challenge.
- [ ] Enforce expiry, logout, process-restart invalidation, rate limiting,
      same-origin requests, CSRF, body limits, and browser security headers.
- [ ] Expose all eight Remindi operations under `/api/v1` through the same
      `RemindiService`, preserving validation, evidence, idempotency, versions,
      errors, and owner scoping.
- [ ] Test every authentication-variable combination, login outcome, cookie
      property, session boundary, mutation rejection, and API operation.

**Targeted check:**

```bash
cargo test --test webui auth
cargo test --test webui api
```

### Task 10: Build and verify the embedded Remindi UI

**Source:** `SPEC.md` Sections 14.13 and 23.6; `DESIGN.md` Sections 14 and 23.3.

**Files:**

- Create: `src/webui/mod.rs`
- Create: `src/webui/assets.rs`
- Create: `src/webui/static/index.html`
- Create: `src/webui/static/app.css`
- Create: `src/webui/static/app.js`
- Create: `src/webui/static/logo.svg`
- Create: `src/webui/static/favicon.svg`
- Create: `tests/webui/browser.rs`

**Steps:**

- [ ] Embed plain HTML, CSS, and ES modules with no second production runtime,
      frontend framework, or external CDN.
- [ ] Implement the sign-in modal, dashboard, list/filter view, create/edit,
      check, snooze, complete, cancel, details, history, and error/conflict
      states.
- [ ] Apply the default PHrK tokens/assets and the source-defined read-only
      mounted overrides, with blank overrides retaining defaults.
- [ ] Support keyboard use, visible focus, labelled controls, modal focus
      management, reduced motion, responsive layouts, long content, empty
      states, and inline errors.
- [ ] Run the built UI in a real browser at desktop and mobile sizes through all
      eight operations and the required auth/error paths.

**Targeted check:**

```bash
cargo test --test webui browser
```

### Phase 5 gate

```bash
cargo test --test webui
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

The gate passes when `FR-26`–`FR-29` work through the API and the rendered
browser UI, including authentication, accessibility, responsive, and security
checks.

## 7. Phase 6 — Administration

### Task 11: Implement settings, adapter administration, and audit

**Source:** `SPEC.md` Sections 20.3, 21, and `FR-30`/`FR-31`/`FR-37`;
`DESIGN.md` Sections 13.5 and 16.

**Files:**

- Create: `src/admin/mod.rs`
- Create: `src/admin/settings.rs`
- Create: `src/admin/adapters.rs`
- Create: `src/admin/audit.rs`
- Create: `src/http/api/admin.rs`
- Update: `src/webui/static/index.html`
- Update: `src/webui/static/app.js`
- Create: `tests/webui/admin.rs`

**Steps:**

- [ ] Expose bootstrap settings only through the documented redacted,
      read-only view.
- [ ] Implement only the safe mutable runtime-setting allowlist, validation,
      bounds, versions, and restart markers from `SPEC.md` Section 21.1.
- [ ] Implement adapter configuration for the four named adapters and
      allowlisted aliases; reject generic key/value or arbitrary target input.
- [ ] Publish a fully validated adapter configuration atomically.
- [ ] Append immutable, redacted administrative events for every attempted
      mutation with the source-defined fields and outcomes.
- [ ] Add authenticated forms and conflict/error handling for settings and
      adapter administration.
- [ ] Test allowlists, redaction, invalid aliases, version conflicts,
      persistence, atomic publication, CSRF, and audit content.

**Targeted check:**

```bash
cargo test --test webui admin
```

### Task 12: Implement in-process workload control

**Source:** `SPEC.md` Sections 14.12 and 22.5; `DESIGN.md` Section 17.

**Files:**

- Create: `src/admin/workloads.rs`
- Update: `src/mcp/server.rs`
- Update: `src/scheduler/runner.rs`
- Update: `src/http/api/admin.rs`
- Update: `src/http/health.rs`
- Update: `src/webui/static/app.js`
- Create: `tests/e2e/workloads.rs`

**Steps:**

- [ ] Control only the `mcp` and `scheduler` workloads; do not add container,
      Docker socket, host-service, or process-manager control.
- [ ] Persist desired state before start, stop, or restart transitions.
- [ ] Keep actual state and the last bounded transition error in memory and
      return conflicts for overlapping transitions.
- [ ] Leave authentication, WebUI, backup, and control APIs available while
      either workload is stopped.
- [ ] Make stopped MCP return `503` and stopped scheduler cease background
      evaluation while retaining pull behavior once MCP resumes.
- [ ] Restore persisted desired state after process/container restart and audit
      each action.
- [ ] Test start, stop, restart, conflict, failure, persistence, UI continuity,
      and health reporting.

**Targeted check:**

```bash
cargo test --test e2e workloads
```

### Phase 6 gate

```bash
cargo test --test webui admin
cargo test --test e2e workloads
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

The gate passes when `FR-30`–`FR-33` and `FR-37` work through authenticated API
and browser paths with persistence, redaction, conflict, and audit evidence.

## 8. Phase 7 — Backup and restore

### Task 13: Implement verified backup, upload, download, and retention

**Source:** `SPEC.md` Sections 22.3 and 23.7; `DESIGN.md` Sections 18.1–18.3.

**Files:**

- Create: `src/admin/backup.rs`
- Update: `src/http/api/admin.rs`
- Update: `src/webui/static/app.js`
- Create: `tests/restore.rs`
- Create: `tests/restore/backup.rs`

**Steps:**

- [ ] Create manual and automatic backups through SQLite’s online backup API or
      `VACUUM INTO`; never copy a live database file.
- [ ] Use safe generated temporary names, fsync, integrity/schema/application
      checks, SHA-256, atomic rename, and the specified sidecar manifest.
- [ ] Stream bounded uploads to a temporary file and reject invalid file type,
      page size, integrity, schema, owner, or application invariants before
      registration.
- [ ] Authorize list and download, return the specified metadata, and reconcile
      inventory only from verified database/manifest pairs.
- [ ] Apply automatic retention only to eligible automatic/upload backups and
      retain the audit record; do not add manual deletion.
- [ ] Protect backup files like the live database and keep content out of logs.
- [ ] Test creation, upload rejection, download, digest, reconciliation,
      retention, permissions, restart, and interrupted temporary files.

**Targeted check:**

```bash
cargo test --test restore backup
```

### Task 14: Implement guarded restore and process-loss recovery

**Source:** `SPEC.md` Sections 22.4 and 23.7; `DESIGN.md` Section 18.4.

**Files:**

- Update: `src/admin/backup.rs`
- Update: `src/db/manager.rs`
- Update: `src/http/api/admin.rs`
- Update: `src/http/router.rs`
- Update: `src/webui/static/app.js`
- Create: `tests/restore/restore.rs`
- Create: `tests/restore/recovery.rs`

**Steps:**

- [ ] Require recent password reauthentication and the exact confirmation
      phrase before restore.
- [ ] Validate the candidate and create a verified pre-restore backup before
      entering exclusive maintenance.
- [ ] Quiesce MCP and scheduler without stopping the control plane, drain
      database requests, close the pool, checkpoint WAL, and atomically replace
      the live database.
- [ ] Reopen, apply only supported forward migrations, repeat verification,
      clear transient leases, reconcile backup inventory, and restart workloads
      according to restored desired state.
- [ ] On failure, atomically reinstall and verify the pre-restore database
      before restarting workloads when safe.
- [ ] Persist the fsync-backed restore journal and reconcile every journal phase
      deterministically at startup.
- [ ] Return `MAINTENANCE_ACTIVE` from unrelated database APIs during the
      guarded window and record the final redacted administrative outcome.
- [ ] Exercise upload, reauthentication, confirmation, restore progress, and
      failure recovery through the rendered browser UI.
- [ ] Inject failures before swap, during reopen, after swap, and at every
      journal phase; verify either the original or the validated replacement is
      active, never a partial database.

**Targeted check:**

```bash
cargo test --test restore restore
cargo test --test restore recovery
```

### Phase 7 gate

```bash
cargo test --test restore
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

The gate passes when `FR-34`–`FR-37` meet the manual/automatic backup,
upload/download, retention, restore, rollback, and process-loss requirements
through API and real-browser paths.

## 9. Phase 8 — Docker acceptance

### Task 15: Package and document the supported deployment

**Source:** `SPEC.md` Sections 16, 22, and 24; `DESIGN.md` Sections 21–22.

**Files:**

- Create: `Dockerfile`
- Create: `compose.yaml`
- Create: `README.md`
- Create: `tests/e2e/docker.rs`

**Steps:**

- [ ] Build one Rust binary in a multi-stage image and run it as an unprivileged
      fixed numeric user.
- [ ] Listen on `0.0.0.0:8000` inside the container, publish to loopback by
      default, and persist only the mounted `/data` boundary.
- [ ] Use a read-only root filesystem where practical, require no Docker socket,
      and expose the documented liveness/readiness behavior.
- [ ] Document the exact bootstrap/runtime configuration split, permissions,
      TLS/reverse-proxy boundary, backup/restore procedure, workload behavior,
      health interpretation, and known limitations.
- [ ] Include the pull-mode `AGENTS.md` guidance from `SPEC.md` Section 16 in the
      deployment documentation.
- [ ] Test non-root execution, port mapping, health, persistence, restart,
      permissions, and absence of container-control access.

**Targeted check:**

```bash
cargo test --test e2e docker
docker compose build
docker compose up -d
docker compose ps
docker compose down
```

### Task 16: Run and reconcile version 1 acceptance

**Source:** `SPEC.md` Sections 7, 23, and 27–28; `DESIGN.md` Sections 23–25.

**Files:**

- Update only implementation, test, deployment, or documentation files when a
  failing acceptance check proves a source-backed correction is required.

**Steps:**

- [ ] Run the full unit, property, database, MCP, router, adapter, scheduler,
      WebUI, backup/restore, and Docker suites.
- [ ] Exercise a clean built container through real MCP initialize/list/call
      flows and all eight tools.
- [ ] Exercise the built WebUI in a real browser for the complete workflow,
      administration, workload, backup, restore, keyboard, responsive, branding,
      and security scenarios in `DESIGN.md` Section 23.3.
- [ ] Prove restart persistence, scheduler/pull independence, idempotency races,
      adapter containment, migration, backup, restore rollback, and interrupted
      restore recovery.
- [ ] Run the `SPEC.md` Section 23.8 reference performance workload and record
      the hardware and results; do not silently relax the target.
- [ ] Confirm every `SPEC.md` Section 27 criterion and Section 28 item points to
      passing test or real-world evidence carrying the relevant requirement ID.
- [ ] Inspect logs, HTTP responses, browser storage, screenshots, image layers,
      configuration output, and the repository diff for leaked content,
      credentials, generated junk, or unrequested scope.
- [ ] Reconcile the implementation and `README.md` with the governing documents;
      do not alter `SPEC.md` or `DESIGN.md` to make a failing implementation
      appear compliant.

**Final checks:**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
docker compose build
```

### Phase 8 gate

The release gate passes only when:

- every acceptance criterion in `SPEC.md` Section 27 passes;
- every Definition of Done item in `SPEC.md` Section 28 is evidenced;
- the built container passes real MCP and browser verification;
- backup and restore pass failure and process-loss testing;
- the performance result is recorded against the specified dataset;
- the final diff contains no source conflict, secret, unrelated cleanup, or
  version 2 feature.

## 10. Requirement traceability

Detailed tests retain the exact requirement IDs. This table assigns each source
area to the phase that first closes it; Phase 8 reruns the complete set.

| Phase | Primary source coverage |
|---|---|
| 1 — Foundation | `G-08`, `G-11`; architecture, NFRs, configuration, startup, shutdown, migration, logging, and error boundaries |
| 2 — Remindi core | `G-01`–`G-06`; `FR-01`–`FR-20`, `FR-23`, `FR-25`; lifecycle, evidence, integrity, concurrency, and audit |
| 3 — MCP | `FR-21`–`FR-24`; eight tool contracts, Streamable HTTP, bearer authentication, and structured results |
| 4 — Scheduler/adapters | `G-06`, `G-07`, `G-10`; `FR-03`, `FR-09`–`FR-16`; seven trigger classes, four adapters, lease, containment, and pull independence |
| 5 — WebUI | `G-09`, `G-11`; `FR-26`–`FR-29`; browser authentication, Remindi API/UI, visual, accessibility, and browser security requirements |
| 6 — Administration | `G-02`, `G-09`–`G-11`; `FR-30`–`FR-33`, `FR-37`; runtime settings, adapter configuration, workload control, redaction, and admin audit |
| 7 — Backup/restore | `G-02`, `G-08`, `G-09`; `FR-34`–`FR-37`; backup, upload/download, retention, guarded restore, rollback, and recovery |
| 8 — Docker acceptance | `G-01`–`G-11`, `FR-01`–`FR-37`; all NFRs, deployment requirements, acceptance criteria, and Definition of Done |
