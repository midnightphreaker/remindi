# Remindi Version 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking. Do not start a later phase until the
> current phase gate is satisfied.

**Goal:** Build and verify the version 1 Remindi service described by
`SPEC.md` v1.2.0 and `DESIGN.md` v1.1.0 as one Rust binary and one hardened
container.

**Architecture:** One Axum control plane owns one SQLite database and one
internal `0.0.0.0:8000` listener. The same listener serves the rmcp Streamable
HTTP endpoint at `/mcp`, an embedded plain-JavaScript WebUI and JSON API, health
routes, in-process MCP and scheduler workloads, and guarded backup and restore.
Both MCP and WebUI handlers call the same `RemindiService`; only repositories
write domain data.

**Tech Stack:** Rust 1.94 minimum and Rust 1.97.1 pinned builder/toolchain,
Tokio, Axum/Tower, rmcp, SQLx with SQLite, Schemars/Serde, plain embedded
HTML/CSS/ES modules, a multi-stage Docker image, and Docker Compose.

**Planning confidence:** 0.96. Both governing documents were read completely,
their existing validation evidence was rechecked, current implementation APIs
were researched from primary sources, and independent requirements and
architecture reviews found no unresolved owner decision. Confidence must remain
above 0.90 at every phase gate; new contradictory evidence pauses the phase.

## Global Constraints

- Preserve `SPEC.md` as behavioral/public-contract authority and use
  `DESIGN.md` only where it does not conflict; changing either requires explicit
  owner approval.
- Produce one Rust binary, one hardened container, one SQLite database, one
  configured owner, and one internal `0.0.0.0:8000` listener.
- Expose exactly eight MCP tools, seven trigger classes, four named read-only
  adapters, two controllable workloads, and the exact routes/tables/indexes/
  environment variables/runtime keys in Section 16.
- Use Rust 1.94 as the declared MSRV, test 1.94.1, and build/pin with Rust
  1.97.1; commit exact dependency resolution in `Cargo.lock`.
- Use test-first changes, explicit-offset RFC 3339 input, normalized UTC
  millisecond storage, `BEGIN IMMEDIATE` writes, exact owner scoping, bounded
  I/O, and immutable domain/administrative audit.
- Do not add arbitrary execution, owner selection, delivery, a second
  production runtime/listener/database, Docker control, external CDN assets, or
  a production deployment.
- Never place credentials, private payloads, real content, database fixtures,
  browser artifacts, or generated secrets in source, logs, screenshots,
  commits, or acceptance records.

---

## 1. Source authority and scope lock

### 1.1 Governing inputs

| Priority | Source | Version and SHA-256 |
|---:|---|---|
| 1 | `SPEC.md` | v1.2.0 — `ddb9270a039a5818ff33a01f84d8098afad9df0b4cb1007a5b696aee21810113` |
| 2 | `DESIGN.md` | v1.1.0 — `12d42ab03bad722d0ad29a7832642d974204090263f5f68d9920f9a024c7e3ca` |
| 3 | This plan | Sequencing, file ownership, bounded engineering choices, commands, and gates |

If prose in this plan conflicts with `SPEC.md`, stop and correct this plan
before changing code. `DESIGN.md` governs structure when it does not conflict
with `SPEC.md`. Current library documentation is version evidence, not product
authority.

### 1.2 In scope

- The exact 37 functional requirements, 11 goals, 13 non-functional
  requirements, 43 acceptance checks, and eight Definition of Done checks in
  `SPEC.md`.
- Exactly eight MCP tools:
  `remindi_add`, `remindi_check`, `remindi_complete`, `remindi_snooze`,
  `remindi_update`, `remindi_list`, `remindi_cancel`, and `remindi_history`.
- Exactly seven trigger classes and four condition adapters.
- The exact application environment variables, runtime-setting keys, HTTP
  routes, SQLite tables, indexes, enums, and error codes in the governing
  documents.
- One embedded WebUI, in-process workload control, automatic and manual
  backups, guarded restore, Docker packaging, tests, and user/operator guidance.

### 1.3 Out of scope

Do not add arbitrary execution, free-form conditions, delivery channels,
browser notifications, RRULE/calendar recurrence, a workflow engine, multiple
writers, clustering, replication, a secrets manager, Docker socket access,
container or host control, bootstrap-setting mutation, or manual backup
deletion. The reserved `delivery_*` event names remain storage compatibility
values and do not authorize delivery code.

### 1.4 Authority boundary

Implementation may change this repository, run local tests, build local images,
and use disposable test databases and containers. It must not deploy to
production, alter external services, write credentials, or weaken host
permissions. Secrets enter tests only through generated ephemeral values and
must never appear in fixtures, logs, screenshots, commits, or command output.

### 1.5 Done when

Version 1 is complete only when the Phase 8 acceptance gate maps every
formal `G-*` and `FR-*` identifier, plus every plan-assigned `NFR-*`, `AC-*`,
and `DOD-*` source-order label, to passing evidence; the real container has
been exercised through MCP and a real browser; backup and restore have survived
injected failures; and the final diff contains only the approved service, tests,
deployment files, and concise operating guidance.

## 2. Resolved implementation choices

These choices close implementation seams already permitted by `DESIGN.md`.
They do not broaden the product.

| Seam | Decision |
|---|---|
| Library/test layout | Add `src/lib.rs` because the design requires a library target. Add `tests/contract.rs`, `tests/database.rs`, `tests/adapters.rs`, `tests/webui.rs`, `tests/restore.rs`, and `tests/e2e.rs`; each includes modules from its same-named directory. |
| Migration authority | Use `sqlx::migrate!()` as the checksum authority in `_sqlx_migrations`. Each migration also creates or updates the required public `schema_migrations` row. The public names are the exact source filenames `0001_initial.sql` and `0002_admin_webui.sql`; startup maps SQLx versions to those expected names, verifies parity, and refuses checksum drift or a newer schema. `_sqlx_migrations` is engine metadata; `schema_migrations` is the product-visible schema version. |
| Schema API | Use Schemars 1.x `schemars::Schema`; the `RootSchema` spelling in the design example is obsolete. Generated schemas remain Draft 2020-12 and are contract-tested. |
| Adapter polymorphism | Use `async-trait` for the heterogeneous `Arc<dyn ConditionAdapter>` registry. No broader plugin system is introduced. |
| Idempotency ownership | One transaction-borrowing `IdempotencyStore` owns `idempotency_records`. Use transport-neutral Remindi action names and explicit `admin.setting.update`, `admin.adapter.update`, `admin.workload.action`, `admin.backup.create`, `admin.backup.upload`, and `admin.restore` namespaces in the existing `tool_name` column. |
| Cursor integrity | Derive a cursor MAC key from the MCP token with HKDF-SHA-256 using info `remindi/v1/cursor-mac`; authenticate canonical cursor bytes with HMAC-SHA-256 and verify in constant time. Never use the actor/token fingerprint as a key. |
| Browser sessions | Use a bounded custom in-memory `WebSessionStore` with `getrandom`-filled 256-bit identifiers, fixed expiry, lazy pruning, and explicit logout/process-restart invalidation. Use `tower-cookies` only for cookie syntax/middleware. |
| Static assets | Use `include_str!`/`include_bytes!`; do not add a frontend framework, Node production runtime, service worker, or `rust-embed`. |
| Backup engine | Use parameterized `VACUUM INTO` as the SQLite online-safe snapshot operation. Never copy a live database file. Pair `integrity_check` with `foreign_key_check` and application-invariant checks. |
| Automatic backups | Run a control-plane-owned `BackupRunner` using the same verified pipeline as manual backup. It is not a third controllable workload and must pause during exclusive maintenance. |
| Restore continuity | Store a safe operation ID, journal phase, and generated filenames in the restore journal. Reconcile the terminal audit and idempotency outcome in the active database after replacement or rollback. |
| Detailed readiness | `/health/live` is public and minimal. `/health/ready` requires the MCP bearer token; WebUI sessions receive equivalent bounded state through authenticated admin APIs. |
| Proxy and same-origin handling | Do not trust `Forwarded` or `X-Forwarded-*`; this specification exposes no trusted-proxy configuration. Validate Host and configured Origin policy before authentication, and require same-origin browser mutations. |
| Network aliases | Item data accepts aliases only. Admin configuration may set `public_only`, `allow_private`, and `allow_loopback` per alias. Link-local, multicast, unspecified, documentation, and metadata addresses are always denied; TLS verification cannot be disabled. |
| Checked-event coalescing | Always record state transitions and changed condition outcomes. Coalesce an otherwise identical ready-item `checked` event for 15 minutes; a coalesced check performs no write or version increment. |
| Restore reauthentication | The restore request carries the current password and exact phrase `RESTORE REMINDI`; successful comparison refreshes `reauthenticated_at` and the same request proceeds. Password bytes are dropped before audit/log construction. |
| Custom media | Custom logos accept PNG, JPEG, or WebP by magic bytes; custom favicons accept PNG or ICO. Custom SVG is rejected. Embedded SVG defaults remain trusted build assets. |
| Content logging | `REMINDI_LOG_CONTENT=true` permits only bounded, redacted message/instruction/evidence-summary fields. It never permits credentials, headers, cookies, evidence metadata, adapter bodies, raw requests, paths, or complete configuration. |

### 2.1 Fixed safety bounds

Put these values in typed validators, not scattered literals:

| Control | Bound |
|---|---:|
| MCP token minimum | 32 UTF-8 bytes after rejecting surrounding whitespace |
| Request body default | 1 MiB |
| Evidence metadata after canonical JSON encoding | 16 KiB |
| Evidence future skew | 5 minutes |
| Login attempts | 5 failures per hashed remote-address/username tuple per 5 minutes |
| Login limiter entries | 4,096, oldest-expiry eviction |
| Pre-session login nonce | 10-minute lifetime, 1,024-entry cap, single use |
| Route timeout: ordinary JSON/API | 30 seconds |
| Route timeout: MCP tool call | 60 seconds, with shorter adapter deadlines |
| Graceful MCP drain | 30 seconds |
| Session store entries | 4,096 |
| Custom CSS | 256 KiB |
| Custom logo | 2 MiB |
| Custom favicon | 512 KiB |

Runtime-setting bounds:

| Key | Inclusive bounds |
|---|---:|
| `scheduler.poll_interval_seconds` | 1–3,600 |
| `scheduler.lease_seconds` | 3–10,800 and at least three poll intervals |
| `adapters.timeout_seconds` | 1–60 |
| `adapters.max_concurrency` | 1–64 |
| `recurrence.max_catch_up_occurrences` | 1–1,000 |
| `remindi.default_overdue_seconds` | 0–31,536,000 |
| `remindi.max_snooze_seconds` | 60–31,536,000 |
| `idempotency.retention_days` | 1–365 |
| `backups.interval_seconds` | 300–31,536,000 |
| `backups.retention_count` | 1–365 |
| `backups.upload_max_bytes` | 1 MiB–16 GiB |

## 3. Dependency and protocol baseline

Task 1 must prove these lines compile together before feature work. Use compatible
minor ranges in `Cargo.toml` and commit the exact resolution in `Cargo.lock`.

| Concern | Required baseline |
|---|---|
| MCP | `rmcp` 2.2 with `server`, `macros`, and `transport-streamable-http-server` |
| HTTP | `axum` 0.8.9, `tower` 0.5.3, `tower-http` 0.7.0, `http` 1.4, `http-body-util` 0.1 |
| Async | `tokio` 1.52, `tokio-util` 0.7.18, `futures` 0.3, `async-trait` 0.1 |
| SQLite | `sqlx` 0.9.0 with `runtime-tokio`, `sqlite-bundled`, `macros`, and `migrate`; do not enable umbrella `sqlite` |
| Data | `serde` 1, `serde_json` 1, `schemars` 1.2.1, `time` 0.3.53, `uuid` 1.24 |
| Integrity/secrets | `sha2` 0.11, `hkdf` 0.13, `hmac` 0.13, `base64` 0.22, `secrecy` 0.10, `subtle` 2.6, `getrandom` 0.4 |
| Web/outbound | `tower-cookies` 0.11, `reqwest` 0.13.4 with rustls and streaming, `url` 2 |
| Linux path containment | `rustix` 1.1.4 with `fs`; use Linux 5.6+ `openat2` beneath an owned root descriptor for `file_exists` |
| Errors/logging | `thiserror` 2, `anyhow` 1 only in `main`, `tracing` 0.1, `tracing-subscriber` 0.3 |
| Test-only | `tempfile`, `proptest`, `jsonschema`, `rcgen`, and `tokio-rustls` at current compatible releases pinned by `Cargo.lock` |
| Development tools | Node.js 24.18.0 LTS, `@playwright/test` 1.61.1 in `package-lock.json`, and `cargo-deny` 0.19.4 |

Primary compatibility references checked on 2026-07-18:

- [Rust 1.97.1 patched release](https://blog.rust-lang.org/2026/07/16/Rust-1.97.1/)
- [rmcp 2.2 feature/API documentation](https://docs.rs/crate/rmcp/latest)
- [rmcp `StreamableHttpService`](https://docs.rs/rmcp/latest/rmcp/transport/streamable_http_server/tower/struct.StreamableHttpService.html)
- [MCP Streamable HTTP specification, 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [Axum router and middleware documentation](https://docs.rs/axum/latest/axum/struct.Router.html)
- [SQLx `Connection::begin_with`](https://docs.rs/sqlx/latest/sqlx/trait.Connection.html)
- [Schemars 1.2](https://docs.rs/schemars/latest/schemars/)
- [Reqwest custom DNS resolver](https://docs.rs/reqwest/latest/reqwest/dns/trait.Resolve.html)
- [Rustix `openat2`](https://docs.rs/rustix/latest/rustix/fs/fn.openat2.html)
- [Linux `openat2(2)`](https://man7.org/linux/man-pages/man2/openat2.2.html)
- [SQLite `VACUUM INTO`](https://www.sqlite.org/lang_vacuum.html)
- [SQLite integrity and foreign-key pragmas](https://www.sqlite.org/pragma.html)
- [SQLite WAL](https://sqlite.org/wal.html)
- [Node.js 24.18.0 LTS release](https://nodejs.org/en/blog/release/v24.18.0)
- [`cargo-deny` 0.19.4 release](https://github.com/EmbarkStudios/cargo-deny/releases/tag/0.19.4)

Patch releases can advance before implementation. Task 1 must re-run the same
compile-level probes and record the resulting versions in `Cargo.lock`; it must
not silently switch protocols, frameworks, databases, or transport modes.

## 4. Shared interfaces and invariants

### 4.1 Test seams

Use concrete production types by default. Keep only the seams required for
determinism and safety:

```rust
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> time::OffsetDateTime;
    fn monotonic_now(&self) -> tokio::time::Instant;
}

pub trait IdSource: Send + Sync {
    fn new_uuid(&self) -> uuid::Uuid;
    fn random_bytes_32(&self) -> [u8; 32];
}

#[async_trait::async_trait]
pub trait ConditionAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn parameter_schema(&self) -> schemars::Schema;
    async fn evaluate(
        &self,
        params: serde_json::Value,
        deadline: tokio::time::Instant,
        cancel: tokio_util::sync::CancellationToken,
    ) -> AdapterResult;
}
```

Network resolution, HTTP execution, and filesystem roots also receive injectable
test implementations. Production uses one guarded resolver, one reqwest client
policy, and canonical configured roots.

### 4.2 Service boundary

Both transports call the same concrete `Arc<RemindiService>`. Its public method
surface is fixed (the following is signature notation, not a second trait):

```text
impl RemindiService {
    pub async fn add(&self, actor: &Actor, command: AddCommand) -> ServiceResult<MutationResult>;
    pub async fn check(&self, actor: &Actor, command: CheckCommand) -> ServiceResult<CheckResult>;
    pub async fn complete(&self, actor: &Actor, command: CompleteCommand) -> ServiceResult<MutationResult>;
    pub async fn snooze(&self, actor: &Actor, command: SnoozeCommand) -> ServiceResult<MutationResult>;
    pub async fn update(&self, actor: &Actor, command: UpdateCommand) -> ServiceResult<MutationResult>;
    pub async fn list(&self, actor: &Actor, query: ListQuery) -> ServiceResult<ListResult>;
    pub async fn cancel(&self, actor: &Actor, command: CancelCommand) -> ServiceResult<MutationResult>;
    pub async fn history(&self, actor: &Actor, query: HistoryQuery) -> ServiceResult<HistoryResult>;
}
```

Transport DTOs deserialize exact public fields, reject unknown properties, and
convert once into domain commands. They never accept `owner_id`. Repositories
accept the configured owner as a required bound parameter on every item query.

### 4.3 Database/maintenance boundary

`DatabaseManager` owns `RwLock<Option<SqlitePool>>` and a maintenance gate.
Normal operations acquire a read permit that contains the pool clone; restore
acquires the exclusive permit, drains ordinary permits, removes and closes the
pool, swaps files, and installs a newly verified pool. No code path can clone a
pool without holding the corresponding permit.

Every state-changing repository method:

1. acquires an ordinary database permit;
2. starts `BEGIN IMMEDIATE` through `Connection::begin_with`;
3. performs bound, allowlisted SQL;
4. enforces expected version and domain invariants;
5. writes the required domain/admin event and, for an explicit keyed mutation,
   the `IdempotencyStore` response in the same transaction; naturally
   repeat-safe check/scheduler transitions have no invented key;
6. increments the item/configuration version exactly once;
7. commits before returning.

External adapter work, filesystem hashing, and backup copying never occur inside
that write transaction.

Lifecycle/restore takes the administrative mutex, closes new admission,
cancels/drains component work, and then takes the exclusive maintenance permit.
Backup work holds no write transaction during file I/O.
Adapter-configuration publication uses only its own mutex and a short snapshot
write lock.

### 4.4 Error boundary

Use `thiserror` per library layer and `anyhow` only in `main`. Map errors once:

```text
SQLx/SQLite -> RepositoryError -> ServiceError
ServiceError -> MCP structured tool result
ServiceError -> HTTP status plus JSON envelope
```

HTTP authentication/protocol errors remain HTTP errors. Business errors use the
exact 20 codes and retryability classifications in `SPEC.md` Section 19. Internal
details, SQL, paths, credentials, and backtraces never cross the boundary.

## 5. Workstream gates and evidence

| Workstream | Gate | Required evidence | Abort/rollback |
|---|---|---|---|
| WS-01 Foundation | G1 | Dependency probes, config tests, migrations on empty/prior DB, pragmas, maintenance drain, router/health smoke | Revert only WS-01 commits; delete disposable databases. No feature phase starts. |
| WS-02 Core | G2 | Deterministic state/trigger/recurrence tests, atomic DB tests, idempotency/concurrency tests, cursor tamper tests | Revert WS-02 commits; migrations remain compatible and unused by release. |
| WS-03 MCP | G3 | Raw Streamable HTTP initialize/list/call tests, exact schemas/annotations, auth/origin/session restart tests | Stop MCP runtime, revert WS-03 commits, preserve DB evidence. |
| WS-04 Scheduler/adapters | G4 | Lease/restart tests, four adapters, containment/timeout/cancellation tests, pull-context separation | Stop scheduler, release lease, revert WS-04 commits. |
| WS-05 WebUI | G5 | Session/CSRF/router tests and a real desktop/mobile keyboard browser run of all eight operations | Disable WebUI in disposable config and revert WS-05 commits. |
| WS-06 Administration | G6 | Settings/adapter/workload/audit tests; UI stays available while workloads stop/restart | Restore prior runtime rows from disposable fixture or revert WS-06 commits. |
| WS-07 Backup/restore | G7 | Verified backup/upload/retention and failure-injected restore/rollback/process-loss recovery | Keep pre-restore backup and journal; rollback through the tested state machine, never by file copy. |
| WS-08 Docker/release | G8 | Hardened image, Compose lifecycle, real MCP/browser checks, performance, full trace matrix | Do not publish/deploy; revert release-only files or build from last accepted commit. |

For code-only rollback, use a normal `git revert` of task commits; do not rewrite
history. For database rollback, restore a verified pre-migration or pre-restore
backup. Each gate records the exact command, exit status, relevant test count,
commit, and real-world evidence location in the implementation handoff or pull
request.

## 6. Global execution rules

1. Keep `SPEC.md` and `DESIGN.md` unchanged until the final reconciliation task.
2. Start each behavior with a failing test named with its requirement ID, such
   as `fr_06_complete_requires_evidence` or
   `ac_s04_dns_rejects_any_denied_address`.
3. Make the smallest passing implementation, then refactor only duplication
   created by that task.
4. Use explicit-offset RFC 3339 inputs and store normalized UTC with millisecond
   precision. Tests use `TestClock`; no timing-semantic test sleeps.
5. Production code must not use `unwrap`, `expect`, unchecked indexing, or
   panic-based input handling. Narrow compile-time invariants are the only
   exception and require a comment.
6. Use static SQL or allowlisted query fragments. Bind all values. Dynamic SQL
   strings never include client text.
7. Name integration tests through the six top-level harnesses so each suite has
   one Cargo binary and focused module files.
8. After every task run its focused command, `cargo fmt --all -- --check`, and
   `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
9. At every phase gate also run
   `cargo test --workspace --all-targets --all-features --locked`.
10. Commit each passing task with the exact Conventional Commit title shown.
    Stage only task-owned files.

## 7. Phase 1 — Foundation

### Task 1: Prove dependency compatibility and scaffold the crate

**Trace:** `V1-IN-01`, `NFR-06`, `NFR-08`, `ARCH-01`, `TEST-03`.

**Files:**

- Create: `Cargo.toml`
- Create: `Cargo.lock`
- Create: `build.rs`
- Create: `rust-toolchain.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/app.rs`
- Create: `src/error.rs`
- Create: `src/clock.rs`
- Create: `tests/contract.rs`
- Create: `tests/contract/sdk_compat.rs`
- Create: `tests/database.rs`
- Create: `tests/adapters.rs`
- Create: `tests/webui.rs`
- Create: `tests/restore.rs`
- Create: `tests/e2e.rs`

**Interfaces:**

- Consumes: the locked source documents and the dependency/version matrix in
  Section 3; no application interface exists yet.
- Produces: a compiling crate/module/test-harness graph and compile-proven
  spellings for rmcp service/session management, Schemars schemas, SQLx
  immediate transactions and migration embedding, reqwest resolution, rustix
  containment, and Tokio cancellation/tracking.

**Steps:**

- [ ] Write `tests/contract/sdk_compat.rs` with compile/runtime smoke tests that
      construct rmcp `StreamableHttpService` with its in-memory session manager,
      add a typed output schema and all five tool annotations, generate a
      Schemars Draft 2020-12 schema, call
      `Connection::begin_with("BEGIN IMMEDIATE")` on an in-memory SQLite
      connection, construct a reqwest client with a custom resolver and disabled
      redirects/proxies, compile the Linux `rustix::fs::openat2` path with the
      required flags, and pair `TaskTracker` with
      `CancellationToken`.
- [ ] Run
      `cargo test --test contract sdk_compat -- --nocapture`.
      Expected before the scaffold: Cargo or unresolved-module failure.
- [ ] Pin `rust-toolchain.toml` to 1.97.1. Set package
      `rust-version = "1.94"` only after both toolchains pass. Add the dependency
      families and exact features from Section 3, including SQLx `macros` and
      `migrate`. Keep default
      features disabled where they introduce a second TLS backend, client
      transport, or unused runtime.
- [ ] Add `build.rs` with `cargo:rerun-if-changed=migrations` so stable Cargo
      rebuilds the binary whenever an embedded migration is added or changed.
- [ ] Make `src/lib.rs` export the Task 1 `app`, `clock`, and `error` modules
      only, and keep `src/main.rs` as a Tokio entry point that delegates to
      `app::run`. Later tasks register each additional module when its file is
      created.
- [ ] Make `tests/contract.rs` register
      `tests/contract/sdk_compat.rs`; create the remaining top-level integration
      harnesses as compile-safe empty harnesses. Every later task that creates a
      Rust module or nested test must list and update its nearest `mod.rs`,
      `src/lib.rs`, or top-level test harness in the same commit.
- [ ] Re-run the focused test. Expected: all `sdk_compat` tests pass.
- [ ] Run `cargo tree -d` and confirm one `libsqlite3-sys`, one Schemars major,
      and no native-TLS/OpenSSL dependency.
- [ ] Run
      `cargo +1.94.1 check --workspace --all-targets --all-features --locked`,
      `cargo +1.97.1 check --workspace --all-targets --all-features --locked`,
      `cargo test --workspace --all-targets --locked`,
      `cargo tree --locked -d`, and
      `cargo tree --locked -e features`.
      Expected: exit 0; one SQLite/Schemars/HTTP family; no SQLx
      load-extension/deserialize/unlock-notify, reqwest system-proxy/native-TLS,
      or unused rmcp client transport feature.
- [ ] Commit with `chore: scaffold compatible Remindi crate`.

### Task 2: Implement bootstrap configuration, secrets, IDs, and safe errors

**Trace:** `FR-23`–`FR-28`, `SEC-01`–`SEC-04`, `CFG-01`, `CFG-02`, `CFG-04`,
`ERR-01`, `ERR-02`, `AUD-01`.

**Files:**

- Create: `src/config.rs`
- Update: `src/lib.rs`
- Update: `src/error.rs`
- Update: `src/clock.rs`
- Update: `src/app.rs`
- Create: `tests/contract/bootstrap_config.rs`
- Update: `tests/contract.rs`

**Interfaces:**

- Consumes: the crate/module graph and external API probes from Task 1.
- Produces: `BootstrapConfig::from_env() -> Result<BootstrapConfig,
  ConfigError>`, `Clock`, `IdSource`, redacted application errors, and
  `app::run(BootstrapConfig)`.

**Steps:**

- [ ] Add failing table tests for all 18 application environment variables,
      their defaults, required/conditional fields, fixed internal listener, and
      rejection of application-level `REMINDI_WEBUI_HOST` or
      `REMINDI_WEBUI_PORT`.
- [ ] Add failing cases for blank owner, MCP token under 32 bytes, surrounding
      token whitespace, enabled WebUI auth without both credentials, relative
      database/backup/asset paths, non-owner-only database/backup files or
      backup directories, a world-writable database parent, symlinked final
      paths, and invalid allowed Host/Origin entries. Host entries are exact
      authorities, not wildcards or URLs; Origin entries are normalized
      absolute HTTP(S) origins with no path, query, fragment, or userinfo.
- [ ] Implement immutable `BootstrapConfig`. Wrap the MCP token and WebUI
      password in secrecy types; implement redacted `Debug`; never implement
      `Serialize` for secrets.
- [ ] Validate paths without following a final symlink. Require the database
      parent to exist and be protected; create a missing backup directory with
      mode 0700 and no-follow component checks, but never auto-chown or relax an
      existing path. Custom-asset paths are read-only and are never created.
- [ ] Implement `SystemClock`, `TestClock`, `SystemIdSource`, and
      `DeterministicIdSource`. Generate lowercase UUIDs and random 32-byte
      session/nonce material.
- [ ] Implement the typed error hierarchy and the exact public error-code enum.
      Keep safe public messages separate from internal error sources.
- [ ] Initialize JSON tracing with request-safe fields and a redaction layer.
      Test that `Debug`, error chains, and default logs omit configured sentinel
      secrets and content.
- [ ] Run
      `cargo test --test contract bootstrap_config -- --nocapture`.
      Expected: all configuration and redaction cases pass.
- [ ] Commit with `feat: validate Remindi bootstrap configuration`.

### Task 3: Create exact migrations, DatabaseManager, and maintenance permits

**Trace:** `G-01`, `G-02`, `G-08`, `DB-CONV-01`, `DB-SCHEMA-01`,
`DB-INV-01`–`DB-INV-07`, `CONC-01`, `CONC-02`, `OPS-MIGRATE-01`,
`AC-I01`, `AC-I07`, `AC-O01`, `AC-O02`.

**Files:**

- Create: `migrations/0001_initial.sql`
- Create: `migrations/0002_admin_webui.sql`
- Create: `src/db/mod.rs`
- Create: `src/db/manager.rs`
- Create: `src/db/migrations.rs`
- Create: `src/db/transactions.rs`
- Create: `src/admin/mod.rs`
- Create: `src/admin/audit.rs`
- Create: `src/admin/settings.rs`
- Create: `src/admin/adapters.rs`
- Create: `src/admin/workloads.rs`
- Update: `src/lib.rs`
- Update: `src/app.rs`
- Create: `tests/database/migrations.rs`
- Create: `tests/database/constraints.rs`
- Create: `tests/database/maintenance.rs`
- Create: `tests/database/admin_audit.rs`
- Create: `tests/database/workload_state.rs`
- Update: `tests/database.rs`

**Interfaces:**

- Consumes: `BootstrapConfig`, `Clock`, typed errors, and the SQLx compatibility
  proof from Tasks 1–2.
- Produces: `DatabaseManager::{open, ordinary, begin_immediate,
  exclusive_maintenance, checkpoint_and_close, reopen_verified}`, exact
  migrations, transaction-borrowing `RuntimeSettingsStore`,
  `AdapterConfigStore`, `WorkloadStateStore`, and `AdminAuditWriter`.

**Steps:**

- [ ] Copy the normative tables, constraints, foreign keys, and ten indexes from
      `SPEC.md` Section 9.2 without renaming or weakening them. Put the core
      schema in `0001_initial.sql` and the runtime/adapter/workload/backup/admin
      schema in `0002_admin_webui.sql`.
- [ ] In each migration, insert its exact version/name into
      `schema_migrations`: version 1 is `0001_initial.sql` and version 2 is
      `0002_admin_webui.sql`. Map SQLx's version to these compiled expected
      filenames when checking parity; do not compare the public name to SQLx's
      human description. Use `lease_name`, not the prose shorthand `name`, for
      `scheduler_leases`.
- [ ] Write failing empty-database and prior-version tests that assert every
      public table/index, every `STRICT` property, SQLx checksum validation,
      parity between `_sqlx_migrations` and `schema_migrations`, newer-schema
      refusal, and no partial migration after injected failure.
- [ ] In a disposable copy of the crate, build once, add or change a migration
      without touching Rust source, rebuild, and assert the embedded SQLx
      migrator changed. This proves `build.rs` tracks the migration directory.
- [ ] Add failing seed/store tests for exactly 11 runtime-setting defaults, four
      disabled adapter rows, and one `service_runtime` row each for `mcp` and
      `scheduler`, both initially `running`. Seeding is idempotent and never
      overwrites an existing versioned value.
- [ ] Configure every connection with foreign keys on, a 5-second busy timeout,
      synchronous FULL, protected temporary-file handling, and a bounded pool.
      Enable and verify WAL before accepting traffic; keep rollback/WAL
      artifacts beside the database in its protected parent.
- [ ] Create a new database with mode 0600 under the validated parent; validate
      ownership/type/permissions through its opened descriptor and never
      auto-repair an unsafe existing database.
- [ ] Implement `DatabaseManager::open`, `ordinary`, `begin_immediate`,
      `exclusive_maintenance`, `checkpoint_and_close`, and `reopen_verified`.
      An ordinary permit contains its pool clone; no raw pool getter exists.
- [ ] Implement the narrow shared persistence primitives used by later phases:
      snapshot/read and transaction-borrowing versioned updates for runtime
      settings, adapter configuration, and desired workload state, plus
      append-only `AdminAuditWriter::append_in_tx`. Keep HTTP/session/runtime
      orchestration out of these stores.
- [ ] Add constraint tests for all enums, JSON validity, terminal timestamps,
      snooze-field pairing, foreign keys, uniqueness, and append-only repository
      policy. Use the exact 12 public table and ten index inventories.
- [ ] Add a two-writer contention test: the second `BEGIN IMMEDIATE` waits up to
      the busy timeout and returns mapped `DATABASE_BUSY`; a reader remains
      available under WAL.
- [ ] Add maintenance tests proving the exclusive permit drains ordinary
      permits, prevents new work with `MAINTENANCE_ACTIVE`, closes the pool, and
      can reopen the same file.
- [ ] Run
      `cargo test --test database migrations -- --nocapture`.
      Expected: all migration and parity cases pass.
- [ ] Run
      `cargo test --test database constraints -- --nocapture`.
      Expected: all normative constraints pass.
- [ ] Run
      `cargo test --test database maintenance -- --nocapture`.
      Expected: drain, close, and reopen pass.
- [ ] Run
      `cargo test --test database admin_audit -- --nocapture` and
      `cargo test --test database workload_state -- --nocapture`.
      Expected: default rows, version conflicts, transactional audit, and
      idempotent seeding pass.
- [ ] Commit with `feat: add durable SQLite foundation`.

### Task 4: Build the single listener, middleware order, health shell, and shutdown

**Trace:** `FR-21`, `FR-25`, `FR-26`, `ARCH-02`, `NFR-02`, `NFR-09`,
`SEC-02`, `OPS-START-01`, `OPS-STOP-01`, `HEALTH-01`.

**Files:**

- Create: `src/http/mod.rs`
- Create: `src/http/router.rs`
- Create: `src/http/middleware.rs`
- Create: `src/http/health.rs`
- Create: `src/auth/mod.rs`
- Create: `src/auth/mcp.rs`
- Update: `src/lib.rs`
- Update: `src/app.rs`
- Update: `src/main.rs`
- Create: `tests/contract/router.rs`
- Update: `tests/contract.rs`

**Interfaces:**

- Consumes: `BootstrapConfig`, `DatabaseManager`, typed errors, root
  cancellation, and task tracking from Tasks 1–3.
- Produces: `build_router(AppState) -> axum::Router`,
  `Application::run() -> Result<(), AppError>`, exact middleware ordering,
  health handlers, and graceful shutdown.

**Steps:**

- [ ] Write failing router tests for the exact route tree, JSON `/api/*` 404s,
      ordinary non-API 404s, public minimal `/health/live`, authenticated
      `/health/ready`, and absence of a WebUI catch-all.
- [ ] Build one router and one fixed `0.0.0.0:8000` listener. Keep `/mcp`
      registered from the start; its workload delegate returns bounded `503`
      with `Retry-After` until Phase 3 starts it.
- [ ] Apply middleware outer-to-inner in the design order: request ID, safe
      forwarded-header normalization, Host validation, body limit, redacted
      tracing, security response headers, route timeout, route authentication,
      browser Origin/CSRF, handler.
- [ ] Treat `Forwarded`, `X-Forwarded-Host`, `X-Forwarded-Proto`, and similar
      client headers as untrusted and strip them before application logic.
      Security decisions use only the normalized request Host, socket peer, and
      bootstrap policy. Require a TLS proxy to preserve the external Host and
      remove client-supplied forwarding headers; do not invent a trusted-proxy
      allowlist or infer trust from source-address text.
- [ ] Parse exactly one Host as an HTTP authority, reject ambiguous or malformed
      forms, and normalize it consistently with configured Host and Origin
      values.
- [ ] Accept a valid incoming request ID only if it matches the bounded syntax;
      otherwise generate `req_` plus a random lowercase UUID. Echo it on every
      response and include it in spans and error envelopes.
- [ ] Implement minimal liveness without database/content/owner fields. Protect
      detailed readiness with the MCP bearer token and expose database,
      migration, workload, scheduler, adapter, oldest-ready, WAL, backup, and
      restore fields only as bounded status values.
- [ ] Use
      `axum::serve(listener, router).with_graceful_shutdown(shutdown_signal)`
      plus `TaskTracker` and
      a root `CancellationToken`. On signal: stop admission, drain requests,
      cancel child tasks, finish/rollback transactions, release leases,
      checkpoint WAL, close the pool, then exit.
- [ ] Test malformed Host/Origin, absent/invalid readiness bearer, oversized
      JSON, timeout mapping, request ID propagation, redacted tracing, and
      shutdown ordering.
- [ ] Run `cargo test --test contract router -- --nocapture`.
      Expected: all router, middleware, health, and shutdown tests pass.
- [ ] Run the Phase 1 gate commands:
      `cargo fmt --all -- --check`;
      `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`;
      `cargo test --workspace --all-targets --all-features --locked`.
- [ ] Record G1 evidence and commit with
      `feat: establish Remindi control plane`.

## 8. Phase 2 — Remindi core

### Task 5: Implement domain types and boundary validation

**Trace:** `FR-01`–`FR-15`, `SM-01`–`SM-03`, `TRIG-COMMON-01`,
`MCP-SCHEMA-01`, `EVID-01`–`EVID-03`, `SEC-04`.

**Files:**

- Create: `src/remindi/mod.rs`
- Create: `src/remindi/model.rs`
- Create: `src/remindi/state_machine.rs`
- Create: `src/remindi/evidence.rs`
- Create: `src/remindi/recurrence.rs`
- Create: `src/triggers/mod.rs`
- Create: `src/triggers/evaluator.rs`
- Update: `src/lib.rs`
- Create: `tests/contract/domain_validation.rs`
- Update: `tests/contract.rs`

**Interfaces:**

- Consumes: `Clock`, `IdSource`, canonical JSON/error conventions, and the
  exact public schemas in `SPEC.md`.
- Produces: transport-neutral command/result types, `Remindi`, `Trigger`,
  `Evidence`, `Actor`, `AdapterResult`, canonical JSON encoding, and the
  `ConditionAdapter` contract in Section 4.1.

**Steps:**

- [ ] Add failing serialization tests for the exact state, priority, trigger,
      recurrence, link, evidence, actor, lifecycle-event, readiness, condition
      status, event, and workload strings. Reject unknown variants.
- [ ] Define transport-neutral commands/results and domain newtypes. Use
      `OffsetDateTime` internally; parse explicit-offset RFC 3339 and emit UTC
      millisecond precision.
- [ ] Implement one canonical JSON encoder that recursively sorts object keys,
      emits no insignificant whitespace, and rejects non-finite numbers before
      storage/hash use.
- [ ] Validate every length, enum, numeric, control-character, URI, SHA-256, and
      cross-field rule from the shared and eight MCP input schemas. Reject owner
      selectors and credentials embedded in evidence URIs.
- [ ] Enforce evidence metadata at 16 KiB, observation time no more than five
      minutes in the future, at least one URI/hash, SHA-256 only, and the rule
      that adapter satisfaction alone is not completion evidence.
- [ ] Add property tests for serde round trips, canonical JSON stability,
      explicit-offset normalization, and invalid control characters.
- [ ] Run
      `cargo test --test contract domain_validation -- --nocapture`.
      Expected: every positive/negative schema example and property test passes.
- [ ] Commit with `feat: define validated Remindi domain`.

### Task 6: Implement deterministic state, triggers, snooze, and recurrence

**Trace:** `G-03`, `G-04`, `G-06`, `FR-03`–`FR-15`, `FR-19`, `FR-20`,
`DB-INV-02`–`DB-INV-05`, `SM-01`–`SM-03`, `TRIG-AT-01`,
`TRIG-ELAPSED-01`, `TRIG-INTERVAL-01`, `TRIG-SESSION-01`,
`TRIG-CONT-01`, `TRIG-GOAL-01`, `REC-01`, `AC-C05`, `AC-C07`,
`AC-C08`, `AC-C11`.

**Files:**

- Update: `src/remindi/state_machine.rs`
- Update: `src/remindi/recurrence.rs`
- Update: `src/triggers/evaluator.rs`
- Create: `tests/contract/state_machine.rs`
- Create: `tests/contract/triggers.rs`
- Create: `tests/contract/recurrence.rs`
- Update: `tests/contract.rs`

**Interfaces:**

- Consumes: Task 5 domain types plus injected `Clock`.
- Produces: pure transition/trigger evaluators and recurrence advancement
  functions whose only inputs are validated state, explicit context, and time;
  no database or transport dependency.

**Steps:**

- [ ] Write a complete transition-table test. Active states are `scheduled`,
      `snoozed`, `due`, and `overdue`; completion is legal from every active
      state; snooze is legal only from due/overdue; terminal states reject every
      mutation.
- [ ] Implement pure evaluation for `at_time`, resolved `after_elapsed`,
      `interval`, `next_session`, `next_continuation`, and `goal_active`.
      Condition observations enter as data; the pure evaluator performs no I/O.
- [ ] Prove exact-boundary behavior (`now == fire_at`), overdue grace,
      monotonic ready state, logical session independence from MCP transport
      sessions, and explicit active-goal matching.
- [ ] Implement snooze so `original_next_fire_at` and `due_since` remain
      preserved, `snoozed_from_state` restores at expiry, and later interval
      anchors do not drift.
- [ ] Implement recurrence from the scheduled anchor for `coalesce`,
      `catch_up`, and `skip`. When both `max_occurrences` and `end_at` exist,
      the earlier limit wins. The final occurrence rejects acknowledged/skipped
      disposition and requires completion or cancellation.
- [ ] Add property tests showing recurrence never drifts from
      `first_at + n * every_seconds`, never exceeds configured limits, and never
      creates a second Remindi row.
- [ ] Run `cargo test --test contract state_machine -- --nocapture`,
      `cargo test --test contract triggers -- --nocapture`, and
      `cargo test --test contract recurrence -- --nocapture`.
      Expected: all deterministic and property tests pass without wall-clock
      sleeps.
- [ ] Commit with `feat: implement deterministic Remindi lifecycle`.

### Task 7: Implement repository transactions, events, optimistic versions, and idempotency

**Trace:** `G-02`, `G-05`, `FR-16`–`FR-20`, `CONC-01`, `CONC-03`,
`AUD-02`, `AC-I01`–`AC-I07`.

**Files:**

- Create: `src/remindi/repository.rs`
- Update: `src/remindi/mod.rs`
- Create: `src/db/queries.rs`
- Create: `src/db/idempotency.rs`
- Update: `src/db/mod.rs`
- Update: `src/db/transactions.rs`
- Create: `tests/database/repository.rs`
- Create: `tests/database/idempotency.rs`
- Create: `tests/database/concurrency.rs`
- Update: `tests/database.rs`

**Interfaces:**

- Consumes: Task 3 transaction/permit APIs and Tasks 5–6 domain invariants.
- Produces: owner-bound `RemindiRepository`, transaction-borrowing
  `IdempotencyStore::{lookup, insert}`, fixed SQL query builders, and atomic
  event/evidence/idempotency persistence.

**Steps:**

- [ ] Add failing tests for atomic item/event writes, append-only events,
      compare-and-swap updates, exactly one version increment, terminal
      immutability, evidence/item cross-table invariants, and rollback after
      injected failure at each statement boundary.
- [ ] Implement owner-bound repository queries. Build list filters from an
      allowlist of fixed SQL fragments; bind every value.
- [ ] Hash canonical command JSON without `idempotency_key`. On the tuple
      `(actor_id, tool_name, idempotency_key)`, replay the stored response for an
      identical hash and return `IDEMPOTENCY_KEY_REUSED` for a different hash.
- [ ] Implement one `IdempotencyStore` whose lookup/insert methods borrow the
      caller-owned immediate transaction. Reuse it for Remindi, workload,
      backup, upload, and restore mutations; do not create transport-specific
      idempotency implementations.
- [ ] Insert the idempotency row, item/evidence changes, and event in the same
      `BEGIN IMMEDIATE` transaction. Persist the original response JSON exactly.
- [ ] Implement CAS updates with `WHERE id = ? AND owner_id = ? AND version = ?`.
      A zero-row update re-reads the authorized current version and maps to
      `VERSION_CONFLICT`; absence remains indistinguishable from unauthorized
      absence.
- [ ] Add 100 concurrent same-key calls and 100 racing-version calls. Assert one
      item/event response for identical retries and exactly one winner for a
      version race.
- [ ] Run
      `cargo test --test database repository -- --nocapture`,
      `cargo test --test database idempotency -- --nocapture`, and
      `cargo test --test database concurrency -- --nocapture`.
      Expected: all atomicity, retry, and race tests pass.
- [ ] Commit with `feat: persist atomic Remindi mutations`.

### Task 8: Implement the mutation service and evidence-gated terminal actions

**Trace:** `FR-01`, `FR-04`–`FR-07`, `MCP-ADD-01`,
`MCP-COMPLETE-01`, `MCP-SNOOZE-01`, `MCP-UPDATE-01`,
`MCP-CANCEL-01`, `EVID-01`–`EVID-03`, `AC-C01`, `AC-C08`–`AC-C10`.

**Files:**

- Create: `src/remindi/service.rs`
- Update: `src/remindi/mod.rs`
- Update: `src/remindi/evidence.rs`
- Update: `src/remindi/repository.rs`
- Create: `tests/database/service_mutations.rs`
- Update: `tests/database.rs`

**Interfaces:**

- Consumes: `RemindiRepository`, `IdempotencyStore`, Task 6 pure lifecycle
  functions, `Clock`, and `IdSource`.
- Produces: the exact `RemindiService::{add, complete, snooze, update, cancel}`
  methods declared in Section 4.2 and their transport-neutral results.

**Steps:**

- [ ] Add failing service tests for add, every legal update patch/null
      combination, due-only snooze, completion from every active state with
      evidence, completion without evidence, soft cancellation, version
      conflict, idempotent replay, and recurring disposition.
- [ ] Implement `add` so elapsed duration becomes an absolute anchor at
      creation; interval recurrence matches its trigger interval; goal triggers
      have exactly one goal link; condition triggers contain only adapter name,
      parameters, and admin alias.
- [ ] Implement `update` so omitted fields remain unchanged, explicit nullable
      fields clear, at least one mutable field/disposition is present, trigger
      replacement re-derives anchors, and update never terminates an item.
- [ ] Implement `snooze` with reason, future time, configured horizon, original
      schedule preservation, and paired snooze fields.
- [ ] Implement `complete` so evidence is inserted before the item becomes
      completed in the same transaction. Implement `cancel` as terminal soft
      cancellation; neither action deletes data.
- [ ] Emit only bounded transition details: changed field names, trigger
      summaries, evidence ID, note/reason, prior/new versions, and recurrence
      counts.
- [ ] Run
      `cargo test --test database service_mutations -- --nocapture`.
      Expected: every lifecycle mutation and rejection passes.
- [ ] Commit with `feat: implement Remindi mutation service`.

### Task 9: Implement check, list, history, and authenticated keyset cursors

**Trace:** `FR-02`, `FR-03`, `FR-08`–`FR-15`, `MCP-CHECK-01`,
`MCP-LIST-01`, `MCP-HISTORY-01`, `AGENT-02`, `AC-C05`–`AC-C07`.

**Files:**

- Update: `src/remindi/service.rs`
- Update: `src/remindi/repository.rs`
- Create: `src/remindi/cursor.rs`
- Update: `src/remindi/mod.rs`
- Create: `tests/database/check.rs`
- Create: `tests/database/pagination.rs`
- Create: `tests/database/history.rs`
- Update: `tests/database.rs`

**Interfaces:**

- Consumes: Task 8 `RemindiService`, owner-bound repository queries, canonical
  JSON, the configured MCP-token secret, and Task 6 trigger evaluation.
- Produces: core/context-aware `RemindiService::{check, list, history}`,
  authenticated versioned cursors, exact ready ordering, and immutable history
  projections. Task 19 adds adapter execution after the adapter registry exists.

**Steps:**

- [ ] Add failing tests for ready ordering: overdue, due, manual verification;
      then priority descending; then `next_fire_at` ascending with nulls last;
      then stable ID tie-break.
- [ ] Implement `check` as CAS retries over bounded candidates. It may transition
      due/overdue state and append events, but never advances an occurrence or
      completes an item. Context-dependent triggers evaluate only from supplied
      logical context. Preserve the public `evaluate_conditions` input through
      the service boundary; the adapter-aware branch is completed and tested in
      Task 19 after the registry exists.
- [ ] Enforce checked-event policy: transitions and changed condition results
      always write; otherwise an identical ready result writes at most once per
      item per 15 minutes. A coalesced result does not change `last_checked_at`
      or version.
- [ ] Implement `list` as read-only and `history` as ordered immutable events
      plus completion evidence. Apply implicit configured-owner scope; no user
      selector exists.
- [ ] Encode versioned cursor payloads as canonical JSON with exact sort tuple,
      direction, and last key. HKDF derives the HMAC key; base64url carries
      payload and tag. Reject tampering, wrong endpoint, wrong filter hash,
      unknown cursor version, and oversize cursor.
- [ ] Add page-walk property tests with concurrent inserts: no duplicate row,
      stable monotonic order for the snapshot-free keyset contract, and explicit
      documentation that newly inserted earlier keys may appear only in a new
      traversal.
- [ ] Run `cargo test --test database check -- --nocapture`,
      `cargo test --test database pagination -- --nocapture`, and
      `cargo test --test database history -- --nocapture`.
      Expected: ordering, coalescing, context, cursor, and history tests pass.
- [ ] Commit with `feat: add Remindi check and query operations`.

### Task 10: Close the core integrity and performance gate

**Trace:** `G-01`–`G-06`, `NFR-01`, `NFR-03`, `NFR-05`, `TEST-01`,
`TEST-02`, `TEST-08`, `AC-C04`–`AC-C11`, `AC-I01`–`AC-I07`.

**Files:**

- Create: `tests/database/restart.rs`
- Create: `tests/database/property.rs`
- Create: `tests/database/performance.rs`
- Update: `tests/database.rs`

**Interfaces:**

- Consumes: the complete Task 5–9 core with disposable databases and
  deterministic test seams.
- Produces: G2 gate evidence only—restart/property/performance results and no
  new production interface.

**Steps:**

- [ ] Add crash/restart tests immediately before and after each mutation commit.
      Reopen the database and assert either the complete old state or complete
      new state, never a partial item/event/evidence/idempotency combination.
- [ ] Generate model-based sequences of add/check/update/snooze/complete/cancel
      and compare every database state with a pure state-machine model.
- [ ] Seed 100,000 active items with the production indexes and benchmark the
      indexed project check on documented local reference hardware after a warm
      run. Record p50/p95/max and query plans; p95 must be under 250 ms excluding
      adapters.
- [ ] Seed capacity-shaped data for 1 million item rows and a representative
      event multiplier in a separately marked long test. Verify query plans,
      bounded memory, and database/WAL growth without making the normal suite
      depend on a full 20-million-event fixture.
- [ ] Run
      `cargo test --test database restart -- --nocapture`,
      `cargo test --test database property -- --nocapture`, and
      `cargo test --test database performance -- --ignored --nocapture`.
- [ ] Run the Phase 2 gate commands from Section 6 and record G2 evidence,
      including hardware details and any unaccepted performance failure.
- [ ] Commit with `test: prove Remindi core integrity`.

## 9. Phase 3 — MCP transport and eight tools

### Task 11: Define exact MCP input/output schemas and tool metadata

**Trace:** `FR-21`–`FR-24`, `MCP-COMMON-01`, `MCP-SCHEMA-01`,
`MCP-ADD-01`–`MCP-HISTORY-01`, `MCP-RESP-01`, `NFR-06`, `TEST-03`,
`AC-C01`–`AC-C03`.

**Files:**

- Create: `src/mcp/mod.rs`
- Create: `src/mcp/schemas.rs`
- Create: `src/mcp/responses.rs`
- Create: `src/mcp/tools/mod.rs`
- Update: `src/lib.rs`
- Create: `tests/contract/mcp_schemas.rs`
- Create: `tests/contract/mcp_catalog.rs`
- Update: `tests/contract.rs`

**Interfaces:**

- Consumes: Task 5 domain DTOs/results, Section 4.2 service signatures, and the
  rmcp/Schemars probes from Task 1.
- Produces: exact eight-tool input/output schema types, `tool_catalog()`, common
  MCP response/error conversion, and stable discovery metadata.

**Steps:**

- [ ] Transcribe the shared schema and eight input schemas from `SPEC.md` into
      transport DTOs with `serde(deny_unknown_fields)` and `JsonSchema`.
      Preserve exact names, descriptions, formats, bounds, defaults, null
      semantics, and cross-field semantic validators.
- [ ] Define root-object output schemas for success and error envelopes. Every
      tool output includes `ok`, `request_id`, and exactly one of `data` or
      `error`; mutation data includes the current item/version; check, list, and
      history use their exact bounded result shapes.
- [ ] Create the exact tool catalog and lock this annotation matrix:

      | Tool | Read only | Destructive | Idempotent | Open world |
      |---|---:|---:|---:|---:|
      | `remindi_add` | false | false | true | false |
      | `remindi_check` | false | false | true | true |
      | `remindi_complete` | false | true | true | false |
      | `remindi_snooze` | false | true | true | false |
      | `remindi_update` | false | true | true | false |
      | `remindi_list` | true | false | true | false |
      | `remindi_cancel` | false | true | true | false |
      | `remindi_history` | true | false | true | false |

      `open_world=true` on check reflects configured read-only condition
      adapters; it does not permit an item-supplied target.
- [ ] Because the rmcp attribute macro does not itself declare output schemas,
      use its typed tool route API or attach
      `Tool::with_output_schema::<OutputType>()` to each generated route before
      registration. Keep one implementation path and delete compatibility
      probe alternatives.
- [ ] Generate schemas, serialize the discovery catalog, and validate all input
      and output schemas with a Draft 2020-12 validator. Validate every positive
      and negative example from `SPEC.md`.
- [ ] Snapshot only stable semantic fields: eight names, descriptions,
      schemas, titles, and annotations. Do not snapshot SDK ordering or
      incidental `$defs` layout.
- [ ] Run
      `cargo test --test contract mcp_schemas -- --nocapture` and
      `cargo test --test contract mcp_catalog -- --nocapture`.
      Expected: exactly eight tools and all schema/metadata assertions pass.
- [ ] Commit with `feat: define exact Remindi MCP contract`.

### Task 12: Implement bearer/origin enforcement and the drainable MCP runtime

**Trace:** `FR-21`–`FR-25`, `SEC-01`, `SEC-02`, `CONC-02`,
`OPS-WORKLOAD-01`, `HEALTH-01`, `AC-C02`, `AC-C03`.

**Files:**

- Update: `src/auth/mcp.rs`
- Create: `src/mcp/server.rs`
- Create: `src/mcp/runtime.rs`
- Update: `src/mcp/mod.rs`
- Update: `src/http/router.rs`
- Update: `src/http/middleware.rs`
- Update: `src/app.rs`
- Create: `tests/contract/mcp_transport.rs`
- Update: `tests/contract.rs`

**Interfaces:**

- Consumes: Task 4 router/middleware/auth shell and Task 11 tool catalog.
- Produces: `McpRuntime::{start, stop, restart, status, service}`,
  bearer/Host/Origin enforcement, bounded rmcp transport sessions, and the
  persistent `/mcp` delegate.

**Steps:**

- [ ] Add failing raw HTTP tests for initialize, subsequent protocol-version
      headers, POST/GET behavior, malformed JSON-RPC, missing/wrong bearer,
      query/cookie credentials, invalid Host, invalid present Origin, unknown
      session, session deletion, stopped workload, and oversize input.
- [ ] Parse only `Authorization: Bearer`. Compare a fixed-size SHA-256 digest of
      the presented token with the configured digest using `subtle`; derive the
      actor pseudonym through a separate domain `remindi/v1/mcp-actor`.
- [ ] Validate Host before authentication. If an Origin header is present,
      require it in the configured MCP origin allowlist; a non-browser client
      may omit Origin. Make the outer application middleware authoritative:
      a non-empty `REMINDI_HTTP_ALLOWED_HOSTS` requires an exact normalized
      match, while an empty value accepts a syntactically valid normalized
      request Host after the Origin policy, exactly as `SPEC.md` requires.
      Do not depend on or expose any SDK empty-allowlist bypass. Never enable
      permissive CORS.
- [ ] Normalize configured Origins once, including default ports, and enforce
      the configured MCP Origin policy exactly.
- [ ] Construct `StreamableHttpService` in stateful mode with cancellation and
      the supported protocol version and rmcp's in-memory session manager.
      Runtime restart invalidates transport sessions. Keep logical work-session
      fields exclusively in tool input.
- [ ] Enforce the 60-second bound inside POST/tool execution, not as a blanket
      timeout around a negotiated streaming GET. Let rmcp own stream heartbeat/
      reconnect semantics while `McpRuntime` cancellation and the 30-second
      drain bound all long-lived transport sessions; keep DELETE and protocol
      errors short.
- [ ] Implement `McpRuntime` with actual state, admission flag, in-flight count,
      cancellation token, task tracker, and owned rmcp service/session manager.
      Start installs a fresh service; stop rejects new requests, drains for 30
      seconds, cancels remaining work, drops all transport sessions, and leaves
      the control plane running.
- [ ] Make `/mcp` always route through `McpRuntime`. Stopped/starting/stopping
      states return bounded `503` plus `Retry-After`; authentication and Host
      checks still run before the status response.
- [ ] Run
      `cargo test --test contract mcp_transport -- --nocapture`.
      Expected: transport, authentication, Origin, lifecycle, and session tests
      pass against the real Axum/rmcp service.
- [ ] Commit with `feat: serve authenticated MCP transport`.

### Task 13: Implement all eight MCP handlers over RemindiService

**Trace:** `FR-01`–`FR-24`, `MCP-ADD-01`–`MCP-HISTORY-01`,
`MCP-RESP-01`, `ERR-01`, `ERR-02`, `AGENT-01`, `AGENT-02`,
`AC-C01`, `AC-C05`–`AC-C11`.

**Files:**

- Create: `src/mcp/tools/add.rs`
- Create: `src/mcp/tools/check.rs`
- Create: `src/mcp/tools/complete.rs`
- Create: `src/mcp/tools/snooze.rs`
- Create: `src/mcp/tools/update.rs`
- Create: `src/mcp/tools/list.rs`
- Create: `src/mcp/tools/cancel.rs`
- Create: `src/mcp/tools/history.rs`
- Update: `src/mcp/tools/mod.rs`
- Update: `src/mcp/responses.rs`
- Update: `src/mcp/server.rs`
- Create: `tests/contract/mcp_tools.rs`
- Update: `tests/contract.rs`

**Interfaces:**

- Consumes: all eight Section 4.2 `RemindiService` methods, Task 11 DTOs/
  response conversion, and Task 12 runtime registration.
- Produces: one handler module per exact MCP tool and a single registered
  eight-route rmcp service with structured plus text-fallback output.

**Steps:**

- [ ] Write failing calls for every success path and every mapped business
      error. Include idempotent replay, version conflict, invalid state,
      evidence rejection, cursor rejection, and condition-adapter unavailable.
- [ ] Give every tool a concise LLM-facing description that says when to use it,
      required evidence/reason/version behavior, and that the server supplies
      the owner. Do not add aliases.
- [ ] Convert validated DTOs to domain commands once, call the shared service,
      and convert service results once to `CallToolResult`.
- [ ] Return the same JSON object in `structuredContent` and as the single text
      JSON fallback. Validate success structured content against the advertised
      output schema in tests.
- [ ] Return business failures with structured tool-error content and
      `isError=true`. Reserve JSON-RPC/HTTP errors for framing, method,
      authentication, and protocol failures.
- [ ] Propagate request ID and authenticated actor through every service call
      and event. Do not log raw parameters or results.
- [ ] Run `cargo test --test contract mcp_tools -- --nocapture`.
      Expected: all eight handlers, response forms, errors, and idempotent
      retries pass.
- [ ] Commit with `feat: expose eight Remindi MCP tools`.

### Task 14: Prove Streamable HTTP, restart, and agent lifecycle behavior

**Trace:** `G-01`, `G-10`, `FR-21`–`FR-25`, `AGENT-01`, `AGENT-02`,
`TEST-03`, `TEST-05`, `AC-C02`, `AC-C04`, `AC-O04`, `DOD-03`,
`DOD-06`.

**Files:**

- Create: `tests/e2e/mcp_lifecycle.rs`
- Create: `tests/e2e/agent_lifecycle.rs`
- Create: `tests/e2e/restart.rs`
- Update: `tests/e2e.rs`

**Interfaces:**

- Consumes: the complete Task 11–13 MCP surface and Task 5–9 core.
- Produces: G3 gate evidence only—real protocol, process-restart, pull-lifecycle,
  idempotency, and session-invalidation results.

**Steps:**

- [ ] Start the real binary on an ephemeral test listener with a file-backed
      database and generated credentials.
- [ ] Drive raw Streamable HTTP initialize, tool discovery, add, check,
      snooze, complete, cancel, list, and history. Assert there is no SSE-only
      or stdio production route.
- [ ] On initialize send
      `Accept: application/json, text/event-stream`; on later requests send both
      `MCP-Session-Id` and `MCP-Protocol-Version`. Assert notification `202`,
      unsupported version `400`, invalid Origin `403`, missing session `404`,
      GET SSE-or-405, DELETE termination-or-405, and MCP cancellation
      notification behavior.
- [ ] Restart only `McpRuntime`: the old `Mcp-Session-Id` must fail and a newly
      initialized session must see the same durable items. Prove tool input
      `session_id` remains independent.
- [ ] Execute `task_start`, `checkpoint`, `continuation`, and `final_review`
      checks with stable project/task/lineage IDs. Assert next-session and
      next-continuation semantics and that pull checks surface ready work.
- [ ] Restart the process after add and before check; restart again after
      snooze and before completion. Assert data, audit, and idempotency survive.
- [ ] Run
      `cargo test --test e2e mcp_lifecycle -- --nocapture`,
      `cargo test --test e2e agent_lifecycle -- --nocapture`, and
      `cargo test --test e2e restart -- --nocapture`.
- [ ] Run the Phase 3 gate commands from Section 6, record G3 evidence, and
      commit with `test: prove Remindi MCP lifecycle`.

## 10. Phase 4 — Scheduler and safe condition adapters

### Task 15: Implement adapter contracts, typed configuration, and atomic snapshots

**Trace:** `FR-14`, `FR-30`, `ADP-CONTRACT-01`, `ADP-01`–`ADP-04`,
`ADP-X-01`, `CFG-03`, `CFG-04`.

**Files:**

- Create: `src/triggers/adapters/mod.rs`
- Create: `src/triggers/adapters/registry.rs`
- Create: `src/triggers/adapters/network_policy.rs`
- Update: `src/triggers/mod.rs`
- Update: `src/triggers/evaluator.rs`
- Create: `tests/adapters/registry.rs`
- Create: `tests/adapters/configuration.rs`
- Update: `tests/adapters.rs`

**Interfaces:**

- Consumes: Task 5 `ConditionAdapter`/`AdapterResult`, Task 3
  `AdapterConfigStore`/`AdminAuditWriter`, database transactions, and the exact
  four pre-seeded adapter rows.
- Produces: immutable `AdapterRegistrySnapshot`, serialized configuration
  activation, typed alias configs, and shared `NetworkPolicy`.

**Steps:**

- [ ] Add failing tests for exactly four unique names, non-empty versions,
      Draft 2020-12 parameter schemas, disabled-by-default I/O adapters, malformed
      config rejection, expected-version conflicts, and no partial activation.
- [ ] Define immutable typed configurations and result types. A result contains
      only status, UTC observation time, bounded non-secret summary, adapter
      name/version, latency, and safe metadata.
- [ ] Build a `RwLock<Arc<AdapterRegistrySnapshot>>`. Validate and fully
      construct the candidate replacement snapshot before opening the database
      transaction; then persist the candidate and immutable admin event
      atomically, and publish the already-built snapshot under one short write
      lock only after commit. Invalid or unbuildable configuration must neither
      persist nor activate.
- [ ] Serialize configuration build/commit/swap with one adapter-configuration
      mutex so concurrent updates to different adapters cannot publish stale
      whole-registry snapshots. Keep evaluation lock-free on cloned immutable
      snapshots; test same-adapter conflicts and different-adapter concurrent
      updates.
- [ ] Keep item parameters separate from admin configuration. Item data can
      select only an existing alias and adapter-specific safe values such as an
      expected status; it cannot supply URL, host, IP, port, or filesystem path.
- [ ] Ensure adapters cannot receive repositories, database pools, process
      handles, environment access, or mutation clients.
- [ ] Run
      `cargo test --test adapters registry -- --nocapture` and
      `cargo test --test adapters configuration -- --nocapture`.
      Expected: registration, schema, version, and activation tests pass.
- [ ] Commit with `feat: add safe adapter registry`.

### Task 16: Implement DNS policy, HTTPS health, and TCP reachability

**Trace:** `G-07`, `FR-14`, `ADP-02`, `ADP-03`, `ADP-X-01`,
`SEC-05`, `TEST-04`, `AC-S01`–`AC-S04`.

**Files:**

- Update: `src/triggers/adapters/network_policy.rs`
- Create: `src/triggers/adapters/http_health.rs`
- Create: `src/triggers/adapters/tcp_reachable.rs`
- Update: `src/triggers/adapters/mod.rs`
- Create: `tests/adapters/network_policy.rs`
- Create: `tests/adapters/http_health.rs`
- Create: `tests/adapters/tcp_reachable.rs`
- Update: `tests/adapters.rs`

**Interfaces:**

- Consumes: Task 15 registry/configuration and `NetworkPolicy`, injected DNS/
  transport seams, and runtime deadlines/cancellation.
- Produces: registered `HttpHealthAdapter` and `TcpReachableAdapter` that accept
  aliases only and return bounded `AdapterResult`.

**Steps:**

- [ ] Add an exhaustive IP-policy table covering IPv4/IPv6 loopback, RFC1918,
      ULA, link-local, multicast, unspecified, broadcast, documentation,
      IPv4-mapped IPv6, carrier-grade NAT, and metadata endpoints. Always deny
      link-local/metadata/multicast/unspecified/documentation; admit private or
      loopback only through its explicit admin flag. Normalize IPv4-mapped IPv6
      before classification and validate URL/IP literals directly instead of
      assuming the DNS resolver will see them.
- [ ] Resolve on every evaluation through a reqwest `Resolve` wrapper, validate
      every returned address, and reject the whole target if any address is
      denied. Build a request-scoped client, or an equivalent request-scoped
      pinned connector, from only that validated set with
      `pool_max_idle_per_host(0)` so connection reuse cannot skip a later
      evaluation's DNS policy and no second ambient lookup occurs.
- [ ] Build the HTTP client with rustls validation, no proxy, no referer, no
      cookies, a connect timeout, a total deadline, and streaming body cap.
      Redirects are disabled by default. If an administrator enables the
      design-permitted redirect policy, allow at most three same-origin HTTPS
      redirects and re-resolve/revalidate every hop through the same guarded
      path; reject cross-origin, downgraded, or otherwise unsafe redirects.
      Accept only configured HTTPS GET targets without URL userinfo or fragment.
      Never return or persist response bytes.
- [ ] For TCP, resolve and validate through the same policy, connect to a
      validated address under the deadline, send no bytes, and close
      immediately.
- [ ] Inject DNS answers proving all-address validation, one guarded resolution
      on every evaluation, no unvalidated second lookup or pooled connection
      reuse, and rebinding resistance. Inject same-origin and cross-origin
      redirects to denied addresses and assert the configured policy is
      enforced at every hop.
- [ ] Use a test CA/server to prove trusted TLS succeeds, invalid/untrusted TLS
      fails, response cap and content-type/status rules work, and timeouts and
      cancellation terminate promptly.
- [ ] Run
      `cargo test --test adapters network_policy -- --nocapture`,
      `cargo test --test adapters http_health -- --nocapture`, and
      `cargo test --test adapters tcp_reachable -- --nocapture`.
- [ ] Commit with `feat: add contained network adapters`.

### Task 17: Implement observation-window and contained file-existence adapters

**Trace:** `FR-14`, `ADP-01`, `ADP-04`, `ADP-X-01`, `SEC-03`,
`SEC-04`, `TEST-04`, `AC-S01`–`AC-S03`, `AC-S07`.

**Files:**

- Create: `src/triggers/adapters/observation_window_ended.rs`
- Create: `src/triggers/adapters/file_exists.rs`
- Update: `src/triggers/adapters/mod.rs`
- Create: `tests/adapters/observation_window.rs`
- Create: `tests/adapters/file_exists.rs`
- Update: `tests/adapters.rs`

**Interfaces:**

- Consumes: Task 15 registry contract, `Clock`, immutable file-alias config,
  and Task 1 rustix API proof.
- Produces: registered `ObservationWindowEndedAdapter` and Linux-contained
  `FileExistsAdapter`, each returning only bounded `AdapterResult`.

**Steps:**

- [ ] Add exact-before/exact-at/after clock tests for
      `observation_window_ended`; it requires no alias or I/O.
- [ ] At file-alias activation, require an absolute canonical allowlisted root
      and a normalized relative target with no empty, dot, parent, or platform
      prefix component. On Linux, open the canonical root once as an owned
      directory descriptor and retain that descriptor in the immutable alias.
- [ ] At evaluation on Linux, call `rustix::fs::openat2` relative to the owned
      root descriptor with path-only/close-on-exec flags and
      `ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS |
      ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV`; then `fstat` the
      returned descriptor and close it without reading content. A missing
      component is `unsatisfied`; traversal, symlink, mount crossing,
      permission, or race errors are bounded `error`. Do not use a
      canonicalize-and-stat fallback with a race window. Any existing
      non-symlink object, including a directory, is `satisfied`; expose only its
      coarse type in bounded metadata.
- [ ] Probe `openat2` capability when an enabled `file_exists` alias is built.
      On Linux kernels without `openat2`, and on non-Linux targets, fail that
      adapter closed as unsupported and surface bounded readiness/configuration
      evidence; the required Linux container acceptance path must support it.
- [ ] Add traversal, absolute item path, symlink file, symlink directory,
      concurrent symlink-swap race, missing file, regular file, directory,
      permissions, unsupported-kernel, deadline, and cancellation tests.
- [ ] Confirm neither adapter receives a repository or can create, modify,
      execute, or delete a file.
- [ ] Run
      `cargo test --test adapters observation_window -- --nocapture` and
      `cargo test --test adapters file_exists -- --nocapture`.
- [ ] Commit with `feat: add local read-only adapters`.

### Task 18: Implement scheduler lease, candidates, evaluation, and runtime

**Trace:** `G-10`, `FR-03`, `FR-09`–`FR-15`, `CONC-01`, `CONC-02`,
`OPS-START-01`, `OPS-STOP-01`, `AC-O03`, `AC-O04`.

**Files:**

- Create: `src/scheduler/mod.rs`
- Create: `src/scheduler/lease.rs`
- Create: `src/scheduler/runner.rs`
- Create: `src/scheduler/runtime.rs`
- Update: `src/lib.rs`
- Update: `src/remindi/repository.rs`
- Update: `src/app.rs`
- Create: `tests/database/scheduler_lease.rs`
- Create: `tests/adapters/scheduler.rs`
- Update: `tests/database.rs`
- Update: `tests/adapters.rs`

**Interfaces:**

- Consumes: Task 3 database permits, `RuntimeSettingsStore`, and initial desired
  workload rows; Task 9 candidate/check operations; Tasks 15–17 adapter
  snapshot; and root cancellation.
- Produces: `LeaseRepository`, `SchedulerRunner`, and
  `SchedulerRuntime::{start, stop, restart, status}` with deterministic reload/
  cancellation behavior.

**Steps:**

- [ ] Add failing lease tests for acquire, renew before half-life, competing
      holder, expiry takeover, version race, clean release, and immediate halt
      after lease loss. Use the exact `lease_name` column.
- [ ] Query bounded candidates ordered by evaluation time and ID. Include time,
      interval, snooze expiry, overdue, due condition, and manual-verification
      deadlines; exclude session, continuation, and goal triggers.
- [ ] Implement one runner loop: load settings, acquire/renew lease, evaluate
      pure candidates, evaluate condition candidates through a bounded
      semaphore, apply results in short CAS transactions, update health, and
      wait on next deadline, reload signal, or cancellation.
- [ ] Hold no write transaction during adapter work. After each result, re-read
      item version and trigger state inside `BEGIN IMMEDIATE`; discard stale
      observations without overwriting newer work.
- [ ] Implement `SchedulerRuntime` with fresh cancellation token/task tracker
      per start. Stop cancels admission, waits through active adapter deadlines,
      releases the lease, and reports actual state without stopping the control
      plane.
- [ ] Add bounded housekeeping for expired idempotency rows without deleting
      events or Remindi rows. Do not couple automatic backups to scheduler
      availability; scheduling and execution of those backups remain owned by
      the control-plane `BackupRunner`.
- [ ] Run
      `cargo test --test database scheduler_lease -- --nocapture` and
      `cargo test --test adapters scheduler -- --nocapture`.
- [ ] Commit with `feat: run leased Remindi scheduler`.

### Task 19: Close scheduler and adapter security/restart acceptance

**Trace:** `G-07`, `G-10`, `TEST-04`, `TEST-05`, `AC-S01`–`AC-S08`,
`AC-O03`, `AC-O04`, `DOD-05`, `DOD-07`.

**Files:**

- Create: `tests/adapters/security.rs`
- Create: `tests/adapters/pull_check.rs`
- Create: `tests/e2e/scheduler_restart.rs`
- Create: `tests/e2e/condition_flow.rs`
- Update: `src/remindi/service.rs`
- Update: `src/triggers/evaluator.rs`
- Update: `tests/adapters.rs`
- Update: `tests/e2e.rs`

**Interfaces:**

- Consumes: the complete Task 15–18 adapter/scheduler workstream plus MCP/core.
- Produces: adapter-aware `RemindiService::check` and G4 security/restart/
  condition-flow evidence.

**Steps:**

- [ ] Run a condition end to end: configured alias, scheduled evaluation,
      satisfied readiness, MCP check, separate evidence, completion. Assert the
      adapter result is never accepted as completion evidence by itself.
- [ ] Complete the pull path after the adapters exist. With
      `evaluate_conditions=true`, `RemindiService::check` snapshots due
      condition candidates, evaluates them through the current registry outside
      a write transaction, then applies each result through a short version/
      trigger/config recheck and CAS transaction. With
      `evaluate_conditions=false`, it performs no adapter call. Disabled,
      missing, timeout, `unknown`, and manual-check deadlines follow the exact
      manual-verification rules.
- [ ] Stop the scheduler and test both pull modes through MCP: an enabled due
      condition can become ready when evaluation is requested; the false flag
      leaves it unevaluated; stale adapter results cannot overwrite newer item
      or configuration state.
- [ ] Stop/restart the scheduler while MCP and health remain responsive; restart
      the process during evaluation; expire/take over the lease; assert no
      duplicate transition and desired state recovery.
- [ ] Attempt arbitrary shell text, SQL, URL, host, IP, port, absolute/relative
      path, redirect escape, private/metadata DNS, response-body secret, and
      mutable target. Assert rejection and absence from logs/database.
- [ ] Run
      `cargo test --test adapters security -- --nocapture`,
      `cargo test --test adapters pull_check -- --nocapture`,
      `cargo test --test e2e scheduler_restart -- --nocapture`, and
      `cargo test --test e2e condition_flow -- --nocapture`.
- [ ] Run the Phase 4 gate commands from Section 6 and record G4 evidence.
- [ ] Commit with `test: prove scheduler and adapter containment`.

## 11. Phase 5 — WebUI authentication, JSON API, and Remindi surface

### Task 20: Implement in-app authentication, sessions, CSRF, and browser headers

**Trace:** `FR-27`, `FR-28`, `SEC-02`, `SEC-06`, `API-02`, `TEST-06`,
`AC-W01`, `AC-W02`, `AC-W09`.

**Files:**

- Create: `src/auth/web_session.rs`
- Create: `src/auth/csrf.rs`
- Create: `src/auth/rate_limit.rs`
- Update: `src/auth/mod.rs`
- Update: `src/admin/audit.rs`
- Create: `src/http/api/mod.rs`
- Create: `src/http/api/auth.rs`
- Update: `src/http/mod.rs`
- Update: `src/http/middleware.rs`
- Update: `src/http/router.rs`
- Update: `src/app.rs`
- Create: `tests/webui/auth.rs`
- Create: `tests/webui/csrf.rs`
- Create: `tests/webui/security_headers.rs`
- Update: `tests/webui.rs`

**Interfaces:**

- Consumes: `BootstrapConfig`, Task 3 database transactions and
  `AdminAuditWriter`, Task 4 router/middleware, `Clock`, and `IdSource`.
- Produces: `WebSessionStore`, CSRF/Origin guards, bounded login limiter, and
  `/api/v1/{session,auth/login,auth/logout}` handlers for all three WebUI modes.

**Steps:**

- [ ] Add failing tests for the three supported modes: WebUI disabled, enabled
      with authentication, and enabled with authentication disabled plus the
      persistent warning state.
- [ ] Implement `WebSessionStore` with a bounded Tokio map, `getrandom::fill`
      generated
      32-byte session IDs and CSRF secrets, issued/expiry/reauthentication times,
      lazy expiry pruning, explicit logout, and process-restart invalidation.
- [ ] Use a fixed cookie name and `tower-cookies`. Set `HttpOnly`,
      `SameSite=Strict`, `Path=/`, bounded `Max-Age`, and optional `Secure`.
      Never place username, password, or CSRF secret in a cookie.
- [ ] Make unauthenticated `GET /api/v1/session` issue a single-use, 10-minute
      pre-session nonce bound to an HttpOnly nonce cookie and return the matching
      login token. Consume both on login.
- [ ] Hash presented/configured username and password into fixed-size digests
      before constant-time comparison. Rate-limit five failed attempts per
      hashed remote-address/username tuple per five minutes; cap and evict the
      limiter; return a generic failure.
- [ ] On login, replace the nonce cookie with a session cookie and return the
      session-bound CSRF token in JSON. On logout, require same-origin and CSRF,
      revoke server state, and expire the cookie.
- [ ] Use `AdminAuditWriter` to append `login_succeeded`, `login_failed`, and
      `logout` with bounded actor pseudonyms and request/outcome fields in the
      same database transaction as any persistent auth-side effect. If
      maintenance prevents the required audit write, reject before changing
      session state rather than silently losing the event.
- [ ] For browser mutations, require the normalized Origin authority to equal
      the validated request Host and require its scheme to be HTTPS when
      `REMINDI_WEBUI_COOKIE_SECURE=true`, otherwise HTTP. Ignore stripped
      forwarding headers. Test direct HTTP, TLS-proxy configuration, scheme/
      port mismatch, duplicate Host, and forged forwarding headers.
- [ ] Apply restrictive CSP, frame denial, MIME-sniffing denial, strict referrer
      policy, permissions policy, and no-store headers on authentication/admin
      responses. Never emit `WWW-Authenticate`.
- [ ] When auth is disabled, create only the synthetic
      `webui:unauthenticated` actor, but still issue a bounded anonymous
      server-side session and CSRF token so every mutation retains same-origin
      and CSRF protection. Keep restore disabled without configured credentials
      and password-gated when credentials exist.
- [ ] Run
      `cargo test --test webui auth -- --nocapture`,
      `cargo test --test webui csrf -- --nocapture`, and
      `cargo test --test webui security_headers -- --nocapture`.
- [ ] Commit with `feat: secure Remindi WebUI sessions`.

### Task 21: Implement the same-origin Remindi JSON API

**Trace:** `FR-26`–`FR-29`, `API-01`, `API-02`, `ERR-01`, `ERR-02`,
`AC-W03`.

**Files:**

- Create: `src/http/api/remindi.rs`
- Update: `src/http/api/mod.rs`
- Update: `src/http/router.rs`
- Create: `tests/webui/remindi_api.rs`
- Update: `tests/webui.rs`

**Interfaces:**

- Consumes: Task 20 session/CSRF guards and the exact Task 8–9
  `Arc<RemindiService>` methods/DTO validators.
- Produces: same-origin JSON handlers for all eight operations with the common
  response envelope and no transport-specific business logic.

**Steps:**

- [ ] Add failing tests for every method and route:
      `GET/POST /api/v1/remindi`,
      `POST /api/v1/remindi/check`,
      `GET/PATCH /api/v1/remindi/{id}`, and the complete, snooze, cancel, and
      history subroutes.
- [ ] Use the same MCP/domain DTO definitions or exact shared field modules;
      do not create divergent WebUI validation. Browser handlers call the same
      `Arc<RemindiService>`.
- [ ] Require session authentication for every API route and Origin plus
      session-bound CSRF for every mutation. Require and preserve a stable retry
      key for the same explicit mutations as MCP and for the specified
      administrative actions; `check` remains naturally repeat-safe and accepts
      no idempotency key. Retain expected-version, evidence, reason, and
      recurrence rules exactly.
- [ ] Return the common `{ok, request_id, data}` or
      `{ok, request_id, error}` envelope and map the exact service codes to
      bounded HTTP statuses. Return JSON for unknown `/api/*` routes.
- [ ] Escape data only at rendering; JSON remains data. Apply no permissive CORS
      headers and expose no owner selector.
- [ ] Run
      `cargo test --test webui remindi_api -- --nocapture`.
      Expected: all eight shared operations, authentication, CSRF, idempotency,
      and error mappings pass.
- [ ] Commit with `feat: add Remindi WebUI API`.

### Task 22: Embed and validate the PHrK WebUI shell

**Trace:** `FR-26`, `NFR-11`, `NFR-12`, `UI-01`–`UI-03`, `SEC-03`,
`SEC-04`, `AC-W07`, `AC-W08`.

**Files:**

- Create: `src/webui/mod.rs`
- Create: `src/webui/assets.rs`
- Create: `src/webui/static/index.html`
- Create: `src/webui/static/app.css`
- Create: `src/webui/static/app.js`
- Create: `src/webui/static/logo.svg`
- Create: `src/webui/static/favicon.svg`
- Update: `src/lib.rs`
- Update: `src/http/router.rs`
- Create: `tests/webui/assets.rs`
- Create: `tests/webui/accessibility_static.rs`
- Update: `tests/webui.rs`

**Interfaces:**

- Consumes: `BootstrapConfig` asset/title paths, Task 4 router, and the pinned
  PHrK source commit/tokens.
- Produces: immutable `WebAssets`, embedded `/` and `/assets/*` handlers, and a
  semantic no-framework HTML/CSS/ES-module shell.

**Steps:**

- [ ] Import the visual baseline from
      `https://git.phrk.org/pub/cdn-phrk-org` commit
      `8314e6b8b0b36b360fe9b60c01cde7653bd93dbe` with provenance in CSS comments.
      Fetch read-only, verify the commit object, inspect applicable licensing,
      and copy only the required logo/icon/token material. Use the exact tokens
      in `DESIGN.md`.
- [ ] Build semantic HTML with a skip link, landmarks, labelled authentication
      modal, navigation for eight views, `aria-live`, empty/loading/error
      regions, and no inline event handlers or third-party resources.
- [ ] Implement the design's dark PHrK panels/gradients/logo, visible focus,
      text-plus-icon states, reduced motion, browser zoom, and the below-760 px
      labelled-block layout. Color alone never conveys state.
- [ ] Embed assets with `include_str!`/`include_bytes!` and serve exact content
      types, ETags, nosniff, and cache policy. Keep API/auth responses no-store.
- [ ] Validate optional CSS/logo/favicon paths at startup: absolute,
      regular, readable, non-world-writable, size-capped, and accepted by
      UTF-8/magic bytes. Require protected parent directories, open with
      no-follow/close-on-exec semantics, `fstat` the opened descriptor, and read
      the bounded bytes from that descriptor exactly once into immutable
      memory; never validate by path and reopen it. Reject custom SVG.
- [ ] Serve custom CSS after default CSS. Keep CSP authoritative so CSS cannot
      enable remote scripts, fonts, frames, or network destinations.
- [ ] Add static accessibility assertions for landmarks, labels, focus targets,
      no external URLs, no inline script/style, and safe asset types, plus
      missing/oversize/wrong-magic/symlink/swap-race asset path tests.
- [ ] Run
      `cargo test --test webui assets -- --nocapture` and
      `cargo test --test webui accessibility_static -- --nocapture`.
- [ ] Commit with `feat: embed PHrK Remindi WebUI`.

### Task 23: Implement all Remindi views and automate real-browser checks

**Trace:** `FR-29`, `UI-01`, `NFR-11`, `TEST-06`, `AC-W02`, `AC-W03`,
`AC-W08`, `AC-W09`.

**Files:**

- Update: `src/webui/static/index.html`
- Update: `src/webui/static/app.css`
- Update: `src/webui/static/app.js`
- Create: `package.json`
- Create: `package-lock.json`
- Create: `.node-version`
- Create: `playwright.config.ts`
- Create: `tests/webui/start-server.mjs`
- Create: `tests/webui/remindi.spec.ts`
- Create: `tests/webui/accessibility.spec.ts`
- Create: `tests/webui/responsive.spec.ts`

**Interfaces:**

- Consumes: Task 21 JSON API, Task 22 WebUI shell/assets, and authenticated
  session/CSRF semantics.
- Produces: complete eight-operation browser views, in-memory API client,
  pinned Node/Playwright harness, and G5 browser evidence.

**Steps:**

- [ ] Pin Node.js 24.18.0 in `.node-version` and package engines and pin
      `@playwright/test` 1.61.1 as a development-only dependency. The Node
      toolchain must not enter the production image or runtime.
- [ ] Implement a small ES-module API client that keeps the CSRF token only in
      memory, generates a new idempotency key per new mutation, preserves it
      across an explicit retry, handles expiry by reopening the modal, and never
      uses localStorage, sessionStorage, IndexedDB, or a service worker.
- [ ] Implement dashboard, filtered/paginated list, item detail/history, add,
      check, update, snooze, complete-with-evidence, and cancel workflows.
      Encode filters/cursor/selected tab/expanded item in the query string.
- [ ] Implement focus trapping/restoration, Escape where safe, keyboard
      navigation, confirmations, version-conflict refresh, inline actionable
      errors, long-content wrapping, and locale-aware date/number formatting.
- [ ] Make `start-server.mjs` create a clean `target/playwright` data directory,
      generate ephemeral credentials without printing them, spawn the real
      binary, wait on `/health/live`, forward termination signals, wait for the
      child to exit, and fail on leaked/reused state. Let Playwright's
      `webServer` configuration own this script for the entire suite lifecycle.
- [ ] Automate valid/invalid login, no native Basic Auth prompt, all eight
      operations, evidence enforcement, version conflict, logout/expiry/restart,
      browser-storage inspection, keyboard-only traversal, focus behavior,
      reduced motion, and desktop/mobile breakpoints.
- [ ] Run `npm ci` and `npx playwright install chromium`.
- [ ] Run `npx playwright test tests/webui/remindi.spec.ts`,
      `npx playwright test tests/webui/accessibility.spec.ts`, and
      `npx playwright test tests/webui/responsive.spec.ts`.
      Expected: all real Chromium checks pass with no console/page errors.
- [ ] Run the Phase 5 Rust and browser gate commands, record G5 evidence, and
      commit with `feat: complete Remindi WebUI workflows`.

## 12. Phase 6 — Administration and workload control

### Task 24: Implement runtime settings, adapter administration, and admin audit

**Trace:** `FR-30`, `FR-31`, `FR-37`, `CFG-03`, `CFG-04`, `AUD-03`,
`API-02`, `AC-W04`, `AC-W05`.

**Files:**

- Update: `src/admin/mod.rs`
- Update: `src/admin/settings.rs`
- Update: `src/admin/adapters.rs`
- Update: `src/admin/audit.rs`
- Create: `src/http/api/settings.rs`
- Create: `src/http/api/adapters.rs`
- Create: `src/http/api/admin_events.rs`
- Update: `src/http/api/mod.rs`
- Update: `src/http/router.rs`
- Create: `tests/webui/admin_api.rs`
- Update: `tests/webui.rs`
- Update: `tests/database/admin_audit.rs`
- Update: `tests/database.rs`

**Interfaces:**

- Consumes: Task 3 `RuntimeSettingsStore`, `AdapterConfigStore`, and
  `AdminAuditWriter`, Task 7 `IdempotencyStore`, Task 15 adapter activation,
  and Task 20 session/CSRF guards.
- Produces: versioned settings/adapter mutation services, bounded admin-event
  pagination, and settings/adapter/audit JSON handlers.

**Steps:**

- [ ] Use the Task 3 seeded stores and reject any unknown runtime or adapter
      key. Implement the exact bounds in Section 2.1 plus cross-setting
      `lease >= 3 * poll`. Updates require expected version and idempotency key,
      use `BEGIN IMMEDIATE`, increment once, append one redacted admin event,
      and signal affected runners only after commit.
- [ ] Implement:
      `GET /api/v1/settings`,
      `PUT /api/v1/settings/{key}`,
      `GET /api/v1/adapters`, and
      `PUT /api/v1/adapters/{name}`.
      Adapter PUT carries enabled/config/expected-version/idempotency fields and
      publishes the validated snapshot only after commit.
- [ ] Implement `GET /api/v1/admin-events` with keyset pagination and bounded
      filters. Admin events are append-only; there is no update/delete route.
- [ ] Return bootstrap settings as a fixed redacted/read-only object. Never
      return credential presence beyond required status, full paths, token
      fingerprints, complete adapter configuration secrets, or owner ID.
- [ ] Add sentinel tests proving credentials, raw usernames, tokens, item
      content, uploaded paths, and complete configs do not enter audit details,
      API responses, or logs.
- [ ] Run
      `cargo test --test webui admin_api -- --nocapture` and
      `cargo test --test database admin_audit -- --nocapture`.
- [ ] Commit with `feat: add audited Remindi administration`.

### Task 25: Implement persisted, concrete MCP/scheduler workload control

**Trace:** `FR-32`, `FR-33`, `CONC-02`, `OPS-WORKLOAD-01`, `AC-W06`.

**Files:**

- Update: `src/admin/mod.rs`
- Update: `src/admin/workloads.rs`
- Create: `src/http/api/workloads.rs`
- Update: `src/http/api/mod.rs`
- Update: `src/http/router.rs`
- Update: `src/app.rs`
- Update: `tests/database/workload_state.rs`
- Create: `tests/e2e/workloads.rs`
- Update: `tests/database.rs`
- Update: `tests/e2e.rs`

**Interfaces:**

- Consumes: Task 12 `McpRuntime`, Task 18 `SchedulerRuntime`, Task 7
  `IdempotencyStore`, Task 3 `AdminAuditWriter`/`WorkloadStateStore`, and the
  persisted desired-state rows.
- Produces: concrete `WorkloadController::{start, stop, restart, status}` plus
  workload GET/action handlers and durable desired-state acceptance.

**Steps:**

- [ ] Load the two Task 3 `service_runtime` rows; actual state remains in
      memory and `all` is an API target only. Test that a fresh database starts
      both workloads and later explicit stopped state survives restart.
- [ ] Implement one administrative mutex and concrete `McpRuntime` and
      `SchedulerRuntime` handles. Do not add a generalized workload/plugin
      framework.
- [ ] Implement
      `GET /api/v1/workloads` and
      `POST /api/v1/workloads/{component}/{action}` for
      `start|stop|restart`. Mutations require CSRF, expected version,
      idempotency key, and valid component/action, and use the shared
      `IdempotencyStore`.
- [ ] Persist desired state and an immutable `phase=desired_state` admin event
      in one transaction before transition. In that same transaction, use the
      shared idempotency store to persist the exact response: an operation ID,
      requested action, desired states, and resulting persisted versions, with
      no volatile actual-state claim. A replay returns that durable acceptance;
      the client reads `GET /api/v1/workloads` for current actual state. After
      the runtime transition,
      append a separate immutable `phase=runtime_transition` event containing
      only bounded success/failure evidence and publish the actual state; on
      failure keep desired state explicit and set actual `failed`. Never update
      an earlier `admin_events` row or pretend rollback.
- [ ] Continue an accepted transition as a tracked controller operation if its
      HTTP waiter disconnects. Serialize it under the administrative mutex and
      make startup reconcile any accepted desired state left between commit and
      transition.
- [ ] For `all`, update both desired rows in one transaction; stop scheduler
      then MCP and start MCP then scheduler. Restart is ordered stop/start.
- [ ] On process start, attempt desired `running` components after the listener
      is bound. Intentionally stopped components remain stopped and are
      distinguished from failures in health.
- [ ] Prove the WebUI/API/liveness remain responsive while either/both workloads
      are stopped, and old MCP sessions fail after restart.
- [ ] Run
      `cargo test --test database workload_state -- --nocapture` and
      `cargo test --test e2e workloads -- --nocapture`.
- [ ] Commit with `feat: control Remindi workloads in process`.

### Task 26: Implement administration views and bounded operational health

**Trace:** `FR-30`–`FR-35`, `UI-01`, `HEALTH-01`, `NFR-02`, `NFR-09`,
`AC-W04`–`AC-W06`.

**Files:**

- Update: `src/webui/static/index.html`
- Update: `src/webui/static/app.css`
- Update: `src/webui/static/app.js`
- Update: `src/http/health.rs`
- Create: `tests/webui/admin.spec.ts`

**Interfaces:**

- Consumes: Task 24 settings/adapter/audit APIs, Task 25 workload controller,
  Task 4 health state, and Task 23 browser shell/client.
- Produces: settings/adapters/workloads/audit views and bounded authenticated
  operational summaries; no new backend mutation surface.

**Steps:**

- [ ] Implement settings, adapters, workload, and administrative-audit views
      with expected-version handling, typed fields, bounds/help text, changed
      field names, desired/actual state, and no secret/path rendering.
- [ ] Add authenticated operational summaries for database/WAL size, item
      counts, oldest ready age, scheduler iteration/lease, adapter counts and
      bounded failures, MCP sessions, and last backup/restore outcome.
- [ ] Keep public liveness invariant while workloads stop. Keep detailed
      `/health/ready` MCP-bearer protected and return `503` only for actual
      readiness failure/maintenance, not an intentionally stopped workload.
- [ ] Automate setting update/conflict, adapter validation/activation, workload
      stop/start/restart, audit pagination, redaction, and control-plane
      continuity in Chromium.
- [ ] Run `npx playwright test tests/webui/admin.spec.ts`.
      Expected: all admin workflows pass and no secret sentinel is present in
      DOM, network bodies, console, or storage.
- [ ] Commit with `feat: add Remindi administration views`.

### Task 27: Close administration persistence, conflict, and security acceptance

**Trace:** `FR-30`–`FR-35`, `FR-37`, `TEST-06`, `AC-W04`–`AC-W06`,
`AC-S06`, `AC-S08`.

**Files:**

- Create: `tests/e2e/administration.rs`
- Create: `tests/e2e/admin_restart.rs`
- Update: `tests/e2e.rs`

**Interfaces:**

- Consumes: the complete Task 24–26 administration surface.
- Produces: G6 atomicity/conflict/restart/redaction evidence only and no new
  production interface.

**Steps:**

- [ ] Race setting, adapter, and workload expected versions; prove one winner,
      retryable conflict, exact version increments, and atomic admin events.
- [ ] Restart after settings/adapter/desired-state commits and before/after
      runtime activation. Assert persisted state and deterministic startup
      reconciliation.
- [ ] Verify no API accepts bootstrap/bind/credential/path mutation and no
      workload route controls Docker, another process, or a host service.
- [ ] Run
      `cargo test --test e2e administration -- --nocapture` and
      `cargo test --test e2e admin_restart -- --nocapture`.
- [ ] Run the Phase 6 gate commands from Section 6, record G6 evidence, and
      commit with `test: prove Remindi administration`.

## 13. Phase 7 — Backup, upload, guarded restore, and recovery

### Task 28: Implement verified manual/automatic backup and reconciliation

**Trace:** `FR-34`, `FR-35`, `NFR-13`, `OPS-BACKUP-01`, `TEST-07`,
`AC-O01`, `DOD-04`.

**Files:**

- Create: `src/admin/backup.rs`
- Create: `src/admin/backup_runner.rs`
- Create: `src/admin/backup_manifest.rs`
- Update: `src/admin/mod.rs`
- Update: `src/app.rs`
- Create: `tests/restore/backup.rs`
- Create: `tests/restore/reconcile.rs`
- Create: `tests/restore/retention.rs`
- Update: `tests/restore.rs`

**Interfaces:**

- Consumes: `DatabaseManager` ordinary/maintenance permits, Task 7
  `IdempotencyStore`, `AdminAuditWriter`, `Clock`, `IdSource`, and protected
  backup paths.
- Produces: `BackupManager::{create, reconcile, retain}`,
  `BackupRunner::{run, cancel, reload}`, versioned manifests, and one serialized
  verified backup pipeline.

**Steps:**

- [ ] Define a versioned sidecar manifest with the safe generated filename,
      source, digest, size, schema/application version, verification time, and
      only the bounded metadata required by `DESIGN.md`. It contains no raw
      idempotency key, owner value, username, secret, path, or item content.
- [ ] Reserve operation ID and safe temp/final filenames in memory. Because the
      normative status enum has no pending value and every row requires a real
      digest, positive size, and schema version, insert `backup_records` only
      when all required metadata is genuinely known. Early create/upload
      failures atomically append a bounded immutable `admin_events` outcome and,
      when request-scoped, the failed idempotency response, then remove or
      quarantine the temp file; they must not create fake `failed` rows or
      placeholder metadata.
- [ ] Create a non-existent temp destination under `REMINDI_BACKUP_DIR`; execute
      parameterized `VACUUM INTO` through an ordinary database permit; fsync the
      database and parent directory.
- [ ] Open the stable candidate through a dedicated read-only, immutable,
      query-only SQLite connection with `trusted_schema=OFF` and no extensions;
      run header/page checks, `quick_check`, full `integrity_check`,
      `foreign_key_check`, supported schema/migration parity, owner, and
      application invariants. Hash final
      bytes and write/fsync a temporary sidecar. Rename the database and sidecar
      individually to generated final names with a parent-directory fsync after
      each rename, then write the record, audit, and manual-request idempotency
      response in one immediate transaction. Do not claim the two renames are
      one atomic operation; startup reconciliation must safely finish or
      quarantine every crash-between-renames state.
- [ ] Implement `BackupRunner` as a control-plane task with cancellation and
      settings-reload signal. It calls the same create pipeline at the configured
      interval and pauses behind exclusive maintenance.
- [ ] Reconcile inventory only when file, sidecar, digest, metadata, schema, and
      configured-owner validation agree and a read-only candidate check confirms
      application invariants. Rebuild a missing record and a redacted
      `backup_verified` reconciliation event in one transaction. Mark a known
      bad record `invalid` with a failed verification event only when its real
      required metadata remains available; otherwise ignore/quarantine the
      orphan without exposing its contents.
- [ ] Retain all manual/pre-restore backups. Expire only oldest automatic/upload
      files beyond the count, remove file and sidecar after an `expired` record
      transition/admin event, and expose no manual-delete method.
- [ ] Run
      `cargo test --test restore backup -- --nocapture`,
      `cargo test --test restore reconcile -- --nocapture`, and
      `cargo test --test restore retention -- --nocapture`.
- [ ] Commit with `feat: add verified Remindi backups`.

### Task 29: Implement authorized backup list, download, and bounded upload

**Trace:** `FR-34`, `SEC-03`, `SEC-04`, `API-02`, `OPS-BACKUP-01`,
`TEST-07`.

**Files:**

- Create: `src/http/api/backups.rs`
- Update: `src/http/api/mod.rs`
- Update: `src/http/router.rs`
- Update: `src/admin/backup.rs`
- Create: `tests/restore/upload.rs`
- Create: `tests/webui/backups.rs`
- Update: `tests/restore.rs`
- Update: `tests/webui.rs`

**Interfaces:**

- Consumes: Task 28 `BackupManager`, Task 20 session/CSRF, Task 7 idempotency,
  and Task 4 route-specific body/timeout controls.
- Produces: backup list/create/upload/download handlers and the shared hardened
  candidate verifier; no delete endpoint.

**Steps:**

- [ ] Implement:
      `GET /api/v1/backups`,
      `POST /api/v1/backups`,
      `POST /api/v1/backups/upload`, and
      `GET /api/v1/backups/{id}/download`.
      There is no DELETE route.
- [ ] Require session, same Origin, CSRF, and idempotency key for create/upload.
      Use the shared `IdempotencyStore`; list/download require session. Hide
      absent versus unauthorized records.
- [ ] Shape list/create/upload responses from an explicit allowlist: backup ID,
      generated display filename, source/status, digest, size, schema version,
      and bounded timestamps/outcome. Never return filesystem paths, sidecar
      internals, or raw actor identifiers.
- [ ] Stream one multipart SQLite file directly to a generated temp path while
      enforcing `backups.upload_max_bytes`; do not buffer it in memory or use
      the submitted filename. Disable Axum's default body limit only on this
      route, replace it with a route-scoped hard cap that accounts for bounded
      multipart overhead, and still stop the file stream at the configured
      payload limit. Apply a bounded route-specific timeout and remove partial
      files on timeout or disconnect.
- [ ] Validate header/page size/integrity/foreign keys/schema/owner/application
      invariants through the shared hardened candidate connection without
      attaching it to the live connection. Rename/record/audit only after
      complete verification.
- [ ] Test the configured-owner validation required by `SPEC.md`, including
      mismatched and multiple-owner candidates returning `BACKUP_INVALID`.
- [ ] Download only `ready|restored` verified records with
      `application/vnd.sqlite3`, exact length, digest/ETag, nosniff, and a safe
      generated attachment filename plus `Cache-Control: no-store`. Revalidate
      its digest and metadata from the protected backup directory immediately
      before streaming; a mismatch marks the record invalid and sends no
      database bytes.
- [ ] Add negative tests for multipart confusion, excess bytes, non-SQLite,
      corrupt pages, foreign-key failures, newer schema, wrong owner,
      application-invariant failure, symlink/path escape, and digest mismatch.
- [ ] Race identical and different-payload same-key create/upload calls and
      inject process loss between each file/sidecar rename and the final
      database transaction. Assert one visible backup, exact replay or
      `IDEMPOTENCY_KEY_REUSED`, and safe startup reconciliation.
- [ ] Run
      `cargo test --test restore upload -- --nocapture` and
      `cargo test --test webui backups -- --nocapture`.
- [ ] Commit with `feat: add guarded backup transfer`.

### Task 30: Implement journaled restore, atomic replacement, and rollback

**Trace:** `FR-36`, `NFR-13`, `CONC-02`, `OPS-RESTORE-01`, `API-02`,
`TEST-07`, `AC-O07`, `DOD-04`, `DOD-07`.

**Files:**

- Create: `src/admin/restore.rs`
- Create: `src/admin/restore_journal.rs`
- Update: `src/admin/mod.rs`
- Update: `src/admin/backup.rs`
- Update: `src/http/api/backups.rs`
- Update: `src/app.rs`
- Create: `tests/restore/state_machine.rs`
- Create: `tests/restore/rollback.rs`
- Create: `tests/restore/process_loss.rs`
- Update: `tests/restore.rs`

**Interfaces:**

- Consumes: Tasks 28–29 backup/candidate APIs, Task 3 database maintenance,
  Task 25 runtimes/controller, persisted desired state, and the administrative
  mutex.
- Produces: `RestoreManager::restore(RestoreRequest)`,
  `RestoreJournal::{load, transition, clear}`, bootstrap recovery, atomic swap,
  verified rollback, and durable restore replay/audit outcome.

**Steps:**

- [ ] Implement the exact journal phases from `DESIGN.md` as an fsync-backed
      JSON state machine containing operation IDs/phases and safe local
      filenames only. Every phase transition writes temp, syncs, renames, and
      syncs the directory. Keep one mode-0600
      `<database-file-name>.restore-journal` beside the live database and
      generate all staging/rollback files in that same protected parent with
      no-follow, create-new semantics; never place them in `/tmp` or across a
      filesystem boundary.
- [ ] Generate a safe opaque operation ID and never put the raw idempotency key,
      actor, phrase, password, or request in the journal. Resolve replay or key
      reuse through the shared idempotency store and serialize concurrent
      attempts under the administrative mutex.
- [ ] Before opening the live database at startup, inspect the journal and
      deterministically finish replacement verification or reinstall/verify the
      pre-restore database. Do not serve traffic until recovery is conclusive.
- [ ] `POST /api/v1/backups/{id}/restore` requires session, Origin, CSRF,
      expected backup digest, current password, exact phrase
      `RESTORE REMINDI`, and idempotency key. Compare/drop password before
      creating audit data; require successful reauthentication within five
      minutes.
- [ ] After authorization and precondition validation, run restore as one
      tracked control-plane operation owned by the administrative mutex. Once
      replacement begins, persist every phase so disconnect or process loss can
      be recovered deterministically at startup.
- [ ] Revalidate the selected candidate. Under the administrative mutex, append
      the redacted `restore_started` event with only the random operation UUID
      component, then close state-changing database and backup admission with
      `503 MAINTENANCE_ACTIVE`, pause scheduler/backup dispatch, and drain
      active writers. From that frozen live state, create and verify the
      `pre_restore` backup before any replacement; no acknowledged write may
      occur after its snapshot. Then stop scheduler and MCP, cancel/drain
      remaining reads/adapters/backup work, and release the lease. Acquire the
      exclusive maintenance permit, run
      `wal_checkpoint(TRUNCATE)`, inspect its busy/log/checkpoint result columns,
      require a non-busy fully checkpointed result, and close the pool.
- [ ] Copy the already verified candidate to a generated same-directory staging
      file and fsync it. Immediately before swap, rehash that staged file and
      compare it in constant time with the selected record digest; only then
      journal the names, atomically rename the closed live database to the
      rollback name and staging file to the live name. After the verified
      checkpoint and pool close, require stale `-wal`/`-shm` sidecars to be
      absent or remove them before installing the replacement, fsyncing the
      parent directory; any inability to establish that state aborts into the
      journaled rollback path.
- [ ] Reopen with production pragmas, migrate supported prior schemas forward,
      verify both migration ledgers, integrity, foreign keys, owner and
      application invariants, clear transient scheduler leases, reconcile
      backup manifests, reload/validate runtime settings and a complete adapter
      snapshot, and restart only the replacement's persisted desired workloads
      plus the backup runner. Any invalid restored configuration enters
      rollback rather than partially activating.
- [ ] In the final active database, use one immediate transaction to ensure the
      correlated `restore_started` marker exists and persist the terminal
      outcome. On success, mark the selected backup `restored` and append
      `restore_succeeded`; after verified rollback, leave the candidate's
      validity status accurate and append `restore_failed`. Persist the standard
      idempotency response and use the journaled operation ID to reconcile the
      same terminal audit/outcome after crash recovery.
- [ ] Keep the HTTP maintenance flag set throughout reopen and workload
      reconciliation. Release the database-exclusive permit only after a
      verified pool is installed, restart/hold workloads while external
      database routes remain gated, commit the terminal outcome, then remove
      and directory-fsync the journal before clearing maintenance. Recovery
      treats an already-committed terminal operation as an idempotent finish.
- [ ] On any failure after quiescence, journal rollback, close the failed pool,
      reinstall the verified pre-restore file atomically, reopen/revalidate it,
      restart or explicitly hold workloads, record bounded failure in the active
      database, and retain evidence files.
- [ ] Run
      `cargo test --test restore state_machine -- --nocapture`,
      `cargo test --test restore rollback -- --nocapture`, and
      `cargo test --test restore process_loss -- --nocapture`.
- [ ] Commit with `feat: add atomic Remindi restore`.

### Task 31: Close backup/restore browser and failure-injection acceptance

**Trace:** `FR-34`–`FR-36`, `TEST-07`, `AC-W09`, `AC-O01`, `AC-O07`,
`DOD-04`, `DOD-07`.

**Files:**

- Update: `src/webui/static/index.html`
- Update: `src/webui/static/app.css`
- Update: `src/webui/static/app.js`
- Create: `tests/webui/restore.spec.ts`
- Create: `tests/e2e/backup_restore.rs`
- Update: `tests/e2e.rs`

**Interfaces:**

- Consumes: Task 30 restore state machine, Task 29 backup APIs, and Task 26
  administration UI/health summaries.
- Produces: G7 real-browser and process-loss recovery evidence only plus the
  backup/restore views.

**Steps:**

- [ ] Implement backup inventory/create/upload/download and guarded restore UI.
      Show metadata/digest/status, no path, no delete control, password field,
      exact phrase confirmation, maintenance progress, and actionable rollback
      result.
- [ ] Automate invalid/valid upload, manual/automatic backup, retention,
      download digest, wrong password, wrong phrase, expired reauthentication,
      successful restore, and UI availability during maintenance in Chromium.
- [ ] Inject process loss before swap, after old rename, after new rename,
      during reopen, after migration, during reconciliation, and before workload
      restart. Restart the process and assert verified success or rollback,
      exactly one correlated outcome, same-key replay, and different-payload
      key-reuse rejection.
- [ ] Disconnect and time out the initiating browser at each destructive phase;
      prove the server-owned operation still reaches success or verified
      rollback and the reconnecting control plane reports the durable outcome.
- [ ] Restore every supported schema fixture and verify forward migration,
      data/audit preservation, cleared leases, backup reconciliation, and desired
      workload restart.
- [ ] Run `npx playwright test tests/webui/restore.spec.ts` and
      `cargo test --test e2e backup_restore -- --nocapture`.
- [ ] Run the Phase 7 gate commands from Section 6, record G7 evidence, and
      commit with `test: prove Remindi backup recovery`.

## 14. Phase 8 — Docker, operating guidance, and full acceptance

### Task 32: Build the hardened image and Compose boundary

**Trace:** `FR-25`, `CFG-02`, `DEPLOY-01`, `DEPLOY-02`, `HEALTH-01`,
`AC-O06`.

**Files:**

- Create: `Dockerfile`
- Create: `.dockerignore`
- Create: `compose.yaml`
- Update: `src/main.rs`
- Create: `tests/e2e/container.rs`
- Update: `tests/e2e.rs`

**Interfaces:**

- Consumes: the accepted Tasks 1–31 binary, startup/shutdown contract, health
  route, filesystem ownership, and exact environment surface.
- Produces: immutable `remindi` runtime image, exact `compose.yaml`,
  pre-bootstrap `remindi healthcheck`, and container acceptance harness.

**Steps:**

- [ ] Add an internal `remindi healthcheck` command that sends a bounded local
      HTTP request to `127.0.0.1:8000/health/live` and exits success only on the
      expected minimal `200` response. It may parse only the non-secret Host
      policy and use its first normalized authority as the request Host (or the
      loopback authority when the list is empty), so explicit Host policy does
      not break container health. It must not load or print credentials.
      Dispatch this subcommand before service bootstrap; it must not open
      SQLite or inspect the restore journal.
- [ ] Resolve and commit immutable official manifest-list digests for the
      `rust:1.97.1-bookworm` builder and a minimal Debian Bookworm runtime;
      record their tag-to-digest evidence. Run
      `cargo build --release --locked`, and copy only CA certificates and the
      binary into the runtime with a fixed numeric non-root UID/GID and an empty
      mode-0700 `/data` owned by that identity; document the required ownership
      for bind mounts. Do not
      include Cargo, source, Node, Playwright, added
      debug/network tooling, package-manager caches, or test fixtures.
- [ ] Set the image healthcheck to the binary command. Run as non-root; expose
      only 8000; create no writable location outside `/data` and a bounded
      noexec/nosuid/nodev `/tmp` tmpfs owned by the runtime identity.
- [ ] Implement the exact Compose port interpolation:
      `${REMINDI_WEBUI_HOST:-127.0.0.1}:${REMINDI_WEBUI_PORT:-8000}:8000`.
      Keep these two names out of application configuration.
- [ ] Configure one data volume, read-only root, bounded tmpfs, all capability
      drop, no-new-privileges, process/file-descriptor limits, restart policy,
      `stop_grace_period: 120s`, exec-form entrypoint, and no Docker socket or
      unrelated host mount. Prove the real workload remains below the selected
      process and file-descriptor limits. Environment values remain
      interpolation inputs and no credential file is committed.
- [ ] Add container tests for numeric non-root user, fixed internal listener,
      loopback default, persistence, restart, read-only root, health semantics,
      CA/TLS outbound support, one-writer protection, and absence of build/test
      tooling.
- [ ] Run `docker compose config --quiet`,
      `docker build --pull -t remindi:test .`, and
      `cargo test --test e2e container -- --ignored --nocapture`.
- [ ] Commit with `build: package hardened Remindi container`.

### Task 33: Add concise operating, agent, and CI guidance

**Trace:** `AGENT-01`, `AGENT-02`, `OPS-START-01`, `OPS-STOP-01`,
`DEPLOY-01`, `DEPLOY-02`, `AC-O05`, `DOD-06`, `DOD-08`.

**Files:**

- Create: `README.md`
- Create: `AGENTS.md`
- Create: `deny.toml`
- Create: `.forgejo/workflows/ci.yml`

**Interfaces:**

- Consumes: the complete runtime/configuration/operations behavior and exact
  local verification commands from prior tasks.
- Produces: operator `README.md`, project `AGENTS.md`, pinned dependency-policy
  config, and Forgejo CI implementing the same verification ladder.

**Steps:**

- [ ] Document one-container setup, all application/Compose variables,
      loopback default, TLS reverse-proxy requirement, protected data/backup
      paths, auth modes, MCP client URL/header, runtime settings, four adapter
      alias policies, workload behavior, health, backup/restore, update/rollback,
      and troubleshooting without sample secrets. State that an enabled
      `file_exists` alias requires host kernel Linux 5.6 or later for
      fail-closed `openat2` containment.
- [ ] Make the wake-up limitation prominent: the scheduler evaluates readiness
      but MCP alone cannot wake a disconnected client; startup/checkpoint/
      continuation/final pull checks remain required.
- [ ] Put the exact project-level Remindi workflow from `SPEC.md` Section 16 in
      `AGENTS.md`, plus concise repository build/test rules. Do not copy this
      entire implementation plan into it.
- [ ] Configure `cargo-deny` for advisories, compatible licenses, duplicate
      review, and allowed registries/Git sources. Fail unknown/unlicensed
      dependencies unless reviewed with evidence. Pin CI installation to
      `cargo-deny` 0.19.4 with
      `cargo install cargo-deny --version 0.19.4 --locked`.
- [ ] Configure Forgejo CI to run formatting, locked check/clippy/test, cargo
      deny, `npm ci`, Playwright Chromium, release build, Docker build, and the
      non-destructive container acceptance lane. Pin the browser job to Node
      24.18.0 and verify `node --version` before `npm ci`. Cache artifacts,
      never secrets or test databases.
- [ ] Validate README/AGENTS internal links and commands, Compose snippets, and
      absence of real credentials or unsupported features.
- [ ] Commit with `docs: add Remindi operating guidance`.

### Task 34: Run real MCP, browser, security, and recovery acceptance

**Trace:** `AC-C01`–`AC-C11`, `AC-S01`–`AC-S08`, `AC-W01`–`AC-W09`,
`AC-O01`–`AC-O07`, `DOD-01`–`DOD-07`.

**Files:**

- Create: `ACCEPTANCE.md`
- Create: `tests/e2e/acceptance.rs`
- Create: `tests/webui/acceptance.spec.ts`
- Update: `tests/e2e.rs`

**Interfaces:**

- Consumes: the built Task 32 container, Task 33 guidance/CI contract, all
  earlier test seams, and the full Section 15 trace matrix.
- Produces: reproducible `ACCEPTANCE.md` command/artifact evidence and complete
  MCP/browser/security/recovery acceptance suites.

**Steps:**

- [ ] Start the built image through Compose using shell-local generated
      credentials and a fresh volume. Record image digest, commit, toolchain,
      Compose version, browser version, and hardware in `ACCEPTANCE.md`; record
      no secret values.
- [ ] Drive raw MCP Streamable HTTP through initialize/discovery and all eight
      tools. Complete the Appendix A workflow across process/container restarts.
- [ ] Drive Chromium through all three WebUI modes, all Remindi/admin/backup
      workflows, keyboard-only navigation, mobile/desktop layouts, default and
      custom branding, session expiry/restart, CSRF/Origin failure, rate limit,
      security headers, and browser-storage inspection. Capture and genuinely
      inspect desktop/mobile screenshots for clipping, overlap, focus,
      contrast/state cues, long content, and branding; link the accepted
      artifacts from `ACCEPTANCE.md`.
- [ ] Stop/restart MCP, scheduler, and both while the WebUI/control plane remain
      available. Verify old MCP sessions invalidate and desired state survives.
- [ ] Execute all four adapters, SSRF/path containment, arbitrary-execution
      negatives, TLS/timeout/cap tests, private-log sentinel scan, insecure-path
      startup refusal, and owner-selector rejection.
- [ ] Demonstrate manual/automatic backup, upload, download digest, retention,
      guarded restore, injected rollback, and interrupted-journal recovery in
      the built container.
- [ ] Run
      `cargo test --test e2e acceptance -- --ignored --nocapture` and
      `npx playwright test tests/webui/acceptance.spec.ts`.
- [ ] Fill every `AC-*` and `DOD-*` row in `ACCEPTANCE.md` with command,
      result, and CI/local artifact reference. Any missing or failed row keeps
      G8 closed.
- [ ] Commit with `test: record Remindi acceptance evidence`.

### Task 35: Prove performance, reconcile the release, and close G8

**Trace:** `G-01`–`G-11`, `NFR-01`–`NFR-13`, `AC-O08`,
`DOD-01`–`DOD-08`.

**Files:**

- Update: `ACCEPTANCE.md`
- Update only if implementation evidence requires correction: `README.md`
- Update only with explicit owner approval for a verified governing-document erratum: `SPEC.md`
- Update only with explicit owner approval for a verified governing-document erratum: `DESIGN.md`

**Interfaces:**

- Consumes: every accepted workstream, Task 34 evidence, documented reference
  hardware, and the exact source/contract inventories.
- Produces: final performance/capacity evidence, reconciled G8 acceptance,
  release commit, and a pushed CI-verified branch; no deployment or image
  publication.

**Steps:**

- [ ] On documented Linux reference hardware of at least 2 vCPU, 4 GiB RAM,
      and local SSD-class storage, seed 100,000 active items, warm the database,
      and record repeated indexed project-check p50/p95/max. Require p95 under
      250 ms excluding adapters.
- [ ] Build a disposable capacity fixture with 1,000,000 active items and
      20,000,000 events. Record database/WAL/disk usage, migration/integrity
      duration, representative bounded list/history/check query plans, and
      startup/backup behavior; do not weaken the schema or retain the generated
      fixture in Git.
- [ ] Record adapter latency/cancellation, scheduler throughput, backup/restore
      duration, database/WAL growth, and memory/CPU under representative load.
      A failed requirement needs an explicit owner-accepted deviation; absent
      that, status remains partial.
- [ ] Run the full verification ladder:

      ```bash
      cargo fmt --all -- --check
      cargo check --workspace --all-targets --all-features --locked
      cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
      cargo test --workspace --all-targets --all-features --locked
      cargo deny check
      npm ci
      npx playwright test
      cargo build --release --locked
      docker compose config --quiet
      docker build --pull -t remindi:acceptance .
      cargo test --test e2e acceptance -- --ignored --nocapture
      ```

      Expected: every command exits 0.
- [ ] Review `git diff` and `git status`; verify exact public inventories,
      no unknown routes/tools/settings/tables, no placeholders/conflict markers,
      no generated browser/database junk, no unrelated cleanup, and no secret
      material.
- [ ] Re-run schema validation, migration parity, SQLite quick/integrity/foreign
      key checks, exact tool catalog, internal Markdown links, Compose parsing,
      and acceptance-matrix completeness.
- [ ] Reconcile documentation only where verified behavior requires it.
      Never edit requirements merely to make a failing implementation pass;
      stop and obtain owner approval before any `SPEC.md` or `DESIGN.md` change.
- [ ] Mark every trace row in Section 15 accepted, record G8 evidence, and
      commit with `release: complete Remindi version 1 acceptance`.
- [ ] After local G8 passes, verify the intended remote/branch, push the
      accepted commit, and require the pushed CI run to pass before merge or
      release. Do not deploy or publish an image without separate authority.

## 15. Requirement-to-evidence traceability

Test names use lowercase Rust/Playwright-safe forms of these IDs. The
implementation acceptance record links each ID to its test, command, and
artifact. A group mapping does not permit skipping an individual requirement.
`G-*` and `FR-*` are the identifiers printed in `SPEC.md`. Because the source
does not number its Section 7 rows, Section 27 checklist items, or Section 28
done items, this plan assigns stable source-order labels `NFR-01`–`NFR-13`,
`AC-C01`–`AC-C11`, `AC-I01`–`AC-I07`, `AC-S01`–`AC-S08`,
`AC-W01`–`AC-W09`, `AC-O01`–`AC-O08`, and `DOD-01`–`DOD-08`. These labels
are trace aids, not amendments to the governing requirements.

### 15.1 Goals

| Goal | Owning tasks | Acceptance evidence |
|---|---|---|
| `G-01` restart durability | 3, 7, 10, 14, 30, 34 | Database restart/crash suite and container lifecycle |
| `G-02` attributable audit | 3, 7, 24, 28–30 | Atomic event/admin-event tests and immutable history |
| `G-03` evidence-gated completion | 5, 8, 13, 19 | Evidence negative/positive tests through service, MCP, adapter flow |
| `G-04` anchor-preserving snooze | 6, 8, 13 | State/recurrence property tests and MCP flow |
| `G-05` duplicate-retry safety | 7–9, 13, 25, 29 | Idempotency concurrency/replay tests |
| `G-06` deterministic evaluation | 5, 6, 9, 18 | Test-clock trigger/check/scheduler suites |
| `G-07` no arbitrary condition execution | 15–19, 34 | Adapter capability and security-negative suite |
| `G-08` safe single node | 1–4, 18, 25, 30, 32 | Database/workload/restore/container gates |
| `G-09` protected WebUI operation/recovery | 20–31, 34 | Auth, admin, browser, backup/restore acceptance |
| `G-10` scheduler independence | 18, 19, 25, 34 | Scheduler restart and workload-control scenarios |
| `G-11` environment-owned secrets | 2, 20, 24, 32–34 | Config fail-closed, redaction, API/UI/container inspection |

### 15.2 Functional requirements

| Requirements | Owning tasks | Primary suites |
|---|---|---|
| `FR-01`–`FR-08` lifecycle operations | 5–9, 13, 21–23 | `contract`, `database`, MCP, WebUI |
| `FR-09`–`FR-15` triggers/recurrence | 5, 6, 9, 15–19 | Trigger/recurrence/scheduler/adapter |
| `FR-16`–`FR-20` integrity | 3, 7–10 | Database atomicity/idempotency/property/restart |
| `FR-21`–`FR-25` transport/identity | 1–4, 11–14, 32 | Raw MCP transport and container |
| `FR-26`–`FR-29` WebUI/shared API | 20–23 | Rust router/session tests and Playwright |
| `FR-30`–`FR-33` adapter/admin/workloads | 15, 18, 24–27 | Adapter config, admin API, restart, browser |
| `FR-34`, `FR-35` backup/retention | 24, 26, 28, 29, 31 | Backup/upload/retention/browser |
| `FR-36` guarded restore | 20, 29–31 | Restore state/rollback/process-loss/browser |
| `FR-37` admin audit | 24–31 | Admin audit atomicity/redaction suites |

### 15.3 Non-functional requirements

| ID | Requirement | Owning tasks and evidence |
|---|---|---|
| `NFR-01` | FULL durability | 3, 7, 10, 30; crash/reopen and restore |
| `NFR-02` | Control-plane availability | 4, 12, 18, 25, 30; stopped workload/maintenance scenarios |
| `NFR-03` | 100k project check under 250 ms p95 | 10, 35; documented reference run |
| `NFR-04` | Default 5-second bounded adapter latency | 15–19; timeout/cancellation/no-write-transaction tests |
| `NFR-05` | 1m item/20m event target | 10, 35; capacity-shaped data, query plans, growth evidence |
| `NFR-06` | Draft 2020-12 compatibility | 1, 5, 11; schema validator/catalog |
| `NFR-07` | Linux-container portability | 32–35; built image/Compose |
| `NFR-08` | Ordered recoverable migrations | 3, 28–31; dual-ledger parity and verified restore |
| `NFR-09` | Request/actor observability | 2, 4, 13, 24, 26; spans/health/audit |
| `NFR-10` | Private default logs | 2, 13, 16, 24, 34; sentinel scans |
| `NFR-11` | Accessible WebUI | 22, 23, 26, 34; static plus real-browser checks |
| `NFR-12` | Embedded dependency-light WebUI | 22, 23, 32; binary/image inspection |
| `NFR-13` | Atomic restore recovery | 3, 28–31, 34; journal failure matrix |

### 15.4 Acceptance and Definition of Done

| IDs | Owning tasks | Gate |
|---|---|---|
| `AC-C01`–`AC-C11` core/MCP | 6–14, 34 | G2/G3/G8 |
| `AC-I01`–`AC-I07` integrity/audit | 3, 7–10, 24, 34 | G2/G6/G8 |
| `AC-S01`–`AC-S08` security | 2, 15–20, 24, 32, 34 | G4/G5/G8 |
| `AC-W01`–`AC-W09` WebUI/admin | 20–31, 34 | G5/G6/G7/G8 |
| `AC-O01`–`AC-O08` operations | 3, 10, 14, 18, 25, 28–35 | G1/G4/G7/G8 |
| `DOD-01`–`DOD-08` release completeness | 32–35 plus all earlier gates | G8 |

### 15.5 Non-normative source-section labels

The labels below are local navigation shorthand introduced by this plan for
specific sections of `SPEC.md` and `DESIGN.md`; they are not additional
requirements and do not replace the formal `G-*`/`FR-*` identifiers or the
source-order `NFR-*`/`AC-*`/`DOD-*` trace labels defined above.

| Trace cluster | Meaning | Owning tasks |
|---|---|---|
| `SYS-01`, `V1-IN-01`, `ARCH-01`, `ARCH-02` | Exact v1 service and component/listener boundary | 1–4, 11–14, 20–35 |
| `V1-OUT-01`, `NG-01` | Explicit v1 exclusions | All diff reviews; negative acceptance in 19 and 34 |
| `DB-CONV-01`, `DB-SCHEMA-01`, `DB-INV-01`–`DB-INV-07` | SQLite conventions/schema/invariants | 3, 7–10, 28–31 |
| `SM-01`–`SM-03`, `TRIG-*`, `REC-01` | State, triggers, recurrence | 5, 6, 8, 9, 18, 19 |
| `ADP-CONTRACT-01`, `ADP-01`–`ADP-04`, `ADP-X-01` | Four read-only adapters and exclusions | 15–19 |
| `MCP-COMMON-01`, `MCP-SCHEMA-01`, `MCP-*-01`, `MCP-RESP-01` | Exact tool contract | 11–14 |
| `API-01`, `API-02`, `UI-01`–`UI-03` | Browser API/UI contract | 20–31 |
| `EVID-01`–`EVID-03` | Completion evidence | 5, 8, 13, 19 |
| `AGENT-01`, `AGENT-02` | Pull lifecycle integration | 14, 33, 34 |
| `SEC-01`–`SEC-06` | Trust, transport, paths, inputs, network, browser | 2–4, 11–20, 24, 28–34 |
| `CONC-01`–`CONC-03` | SQLite/CAS/lease/idempotency concurrency | 3, 7–10, 18, 25, 30 |
| `ERR-01`, `ERR-02`, `AUD-01`–`AUD-03` | Safe errors, logs, domain/admin audit | 2, 4, 7, 13, 24–31 |
| `CFG-01`–`CFG-04` | Bootstrap/runtime/adapter configuration | 2, 3, 15, 20, 24, 32 |
| `OPS-START-01`, `OPS-STOP-01`, `OPS-MIGRATE-01` | Process and migration lifecycle | 3, 4, 18, 25, 30, 32 |
| `OPS-BACKUP-01`, `OPS-RESTORE-01`, `OPS-WORKLOAD-01` | Backup/restore/workload operations | 25, 28–31 |
| `TEST-01`–`TEST-08` | Required test layers | All gates |
| `DEPLOY-01`, `DEPLOY-02`, `HEALTH-01` | Container/Compose/health | 4, 26, 32–35 |

## 16. Contract inventories that must remain exact

### 16.1 Database

Public tables:

```text
schema_migrations
remindi
remindi_links
remindi_events
completion_evidence
idempotency_records
scheduler_leases
runtime_settings
adapter_configs
service_runtime
backup_records
admin_events
```

Public indexes:

```text
idx_remindi_project_state_fire
idx_remindi_task_state
idx_remindi_trigger_state
idx_remindi_condition_evaluation
idx_remindi_due_since
idx_events_remindi_sequence
idx_links_lookup
idx_idempotency_expiry
idx_backups_created
idx_admin_events_sequence
```

SQLx's `_sqlx_migrations` is internal engine metadata and is the sole persisted
checksum authority. `schema_migrations` remains the exact three-column public
version/name ledger required by `SPEC.md`: versions 1 and 2 map to
`0001_initial.sql` and `0002_admin_webui.sql`. Startup and tests compare SQLx
versions to that compiled mapping and require one-to-one parity; a checksum
mismatch, missing mirror row, unknown row, or newer version stops startup. If
SQLx cannot apply and verify both ledgers atomically, G1 fails and
implementation stops for governing-document reconciliation; do not add a third
ledger or weaken checksum validation.

### 16.2 Application and Compose configuration

Application variables:

| Variable | Default |
|---|---|
| `REMINDI_DB_PATH` | `/data/remindi.db` |
| `REMINDI_OWNER_ID` | required |
| `REMINDI_MCP_TOKEN` | required |
| `REMINDI_BACKUP_DIR` | `/data/backups` |
| `REMINDI_HTTP_ALLOWED_HOSTS` | empty |
| `REMINDI_HTTP_ALLOWED_ORIGINS` | same origin |
| `REMINDI_LOG_LEVEL` | `info` |
| `REMINDI_LOG_CONTENT` | `false` |
| `REMINDI_WEBUI_ENABLE` | `true` |
| `REMINDI_WEBUI_AUTH` | `true` |
| `REMINDI_WEBUI_USERNAME` | empty; required when WebUI auth is enabled |
| `REMINDI_WEBUI_PASSWORD` | empty; required when WebUI auth is enabled |
| `REMINDI_WEBUI_SESSION_TTL_SECONDS` | `43200` |
| `REMINDI_WEBUI_COOKIE_SECURE` | `false` |
| `REMINDI_WEBUI_TITLE` | `Remindi` |
| `REMINDI_WEBUI_CUSTOM_CSS_FILE` | empty |
| `REMINDI_WEBUI_LOGO_FILE` | empty |
| `REMINDI_WEBUI_FAVICON_FILE` | empty |

Compose-only interpolation:

```text
REMINDI_WEBUI_HOST
REMINDI_WEBUI_PORT
```

Runtime keys:

| Key | Default |
|---|---:|
| `scheduler.poll_interval_seconds` | `30` |
| `scheduler.lease_seconds` | `90` |
| `adapters.timeout_seconds` | `5` |
| `adapters.max_concurrency` | `8` |
| `recurrence.max_catch_up_occurrences` | `10` |
| `remindi.default_overdue_seconds` | `0` |
| `remindi.max_snooze_seconds` | `31536000` |
| `idempotency.retention_days` | `30` |
| `backups.interval_seconds` | `86400` |
| `backups.retention_count` | `14` |
| `backups.upload_max_bytes` | `1073741824` |

### 16.3 HTTP surface

| Method | Route |
|---|---|
| GET | `/`, `/assets/*`, `/api/v1/session`, `/api/v1/remindi`, `/api/v1/remindi/{id}`, `/api/v1/remindi/{id}/history`, `/api/v1/adapters`, `/api/v1/settings`, `/api/v1/workloads`, `/api/v1/admin-events`, `/api/v1/backups`, `/api/v1/backups/{id}/download`, `/health/live`, `/health/ready` |
| POST | `/mcp`, `/api/v1/auth/login`, `/api/v1/auth/logout`, `/api/v1/remindi`, `/api/v1/remindi/check`, `/api/v1/remindi/{id}/complete`, `/api/v1/remindi/{id}/snooze`, `/api/v1/remindi/{id}/cancel`, `/api/v1/workloads/{component}/{action}`, `/api/v1/backups`, `/api/v1/backups/upload`, `/api/v1/backups/{id}/restore` |
| PATCH | `/api/v1/remindi/{id}` |
| PUT | `/api/v1/adapters/{name}`, `/api/v1/settings/{key}` |
| GET/DELETE as negotiated by rmcp | `/mcp` |

There is no globally permissive CORS policy, native Basic Auth challenge,
owner-selection route, backup DELETE route, bootstrap mutation route, arbitrary
asset catch-all, second listener, or second WebUI port.

### 16.4 Tool, enum, and error surface

MCP tools:

```text
remindi_add
remindi_check
remindi_complete
remindi_snooze
remindi_update
remindi_list
remindi_cancel
remindi_history
```

Exact domain strings:

| Domain | Values |
|---|---|
| State | `scheduled`, `due`, `overdue`, `snoozed`, `completed`, `cancelled` |
| Trigger | `at_time`, `after_elapsed`, `interval`, `next_session`, `next_continuation`, `goal_active`, `condition` |
| Priority | `low`, `normal`, `high`, `critical` |
| Missed policy | `coalesce`, `catch_up`, `skip` |
| Link | `goal`, `memory`, `issue`, `url`, `artifact` |
| Evidence | `observation`, `test_result`, `artifact`, `log_reference`, `change_reference`, `user_confirmation`, `external_reference` |
| Adapter result | `satisfied`, `unsatisfied`, `unknown`, `error` |
| Actor | `user`, `agent`, `scheduler`, `system` |
| Lifecycle event | `task_start`, `checkpoint`, `continuation`, `final_review` |
| Workload | `mcp`, `scheduler`; `all` is an API-only virtual target |

The `remindi_events` values are `created`, `checked`, `became_due`,
`became_overdue`, `condition_evaluated`, `occurrence_advanced`, `snoozed`,
`updated`, `completed`, `cancelled`, and the reserved `delivery_attempted`,
`delivery_succeeded`, `delivery_failed`. The reserved values have no v1
producer.

Public error mapping:

| Code | Retryability |
|---|---|
| `VALIDATION_ERROR` | false |
| `UNAUTHENTICATED` | false |
| `FORBIDDEN` | false |
| `NOT_FOUND` | false |
| `INVALID_STATE` | false |
| `VERSION_CONFLICT` | true |
| `IDEMPOTENCY_KEY_REUSED` | false |
| `DATABASE_BUSY` | true |
| `ADAPTER_NOT_FOUND` | false |
| `ADAPTER_DISABLED` | false |
| `ADAPTER_TIMEOUT` | true |
| `ADAPTER_ERROR` | contextual, based only on the bounded cause |
| `REAUTHENTICATION_REQUIRED` | false |
| `CSRF_REJECTED` | false |
| `WORKLOAD_CONFLICT` | true |
| `MAINTENANCE_ACTIVE` | true |
| `BACKUP_INVALID` | false |
| `RESTORE_FAILED` | contextual, based on verified rollback state |
| `LIMIT_EXCEEDED` | false |
| `INTERNAL_ERROR` | contextual; default false unless a safe retry is known |

## 17. Startup and shutdown order

### 17.1 Startup

1. Install redacted tracing.
2. Load/validate bootstrap config, secrets, allowed hosts/origins, paths,
   permissions, and custom assets.
3. Inspect/recover an fsync-backed restore journal before opening the live
   database.
4. Create the maintenance gate; open SQLite with explicit options; verify WAL,
   busy timeout, foreign keys, FULL sync, and quick check.
5. Refuse newer schema, run SQLx migrations, and verify both ledgers.
6. Validate generated shared/tool/adapter schemas.
7. Seed exact runtime settings, disabled adapter rows, and workload desired rows.
8. Build repositories, service, registry snapshot, sessions/rate limiter,
   runtimes, backup manager/runner, assets, app state, and persistent router.
9. Bind `0.0.0.0:8000`.
10. Start MCP and scheduler according to desired state.
11. Start the control-plane backup runner.

### 17.2 Shutdown

1. Stop admitting new HTTP and backup work.
2. Cancel the backup runner and active adapter evaluations.
3. Stop scheduler, release its lease, then drain/stop MCP.
4. Wait for or roll back active database transactions.
5. Run `wal_checkpoint(TRUNCATE)` and require a non-busy result.
6. Close the pool, tracked tasks, and listener.
7. Exit cleanly before Compose's 120-second grace period.

## 18. Risk controls and stop conditions

| Risk | Preventive evidence | Stop condition |
|---|---|---|
| rmcp lifecycle cannot sit behind a persistent route | Task 1 SDK probe and Task 12 drain/session tests | Starting/stopping requires a second listener or leaves sessions/in-flight work unbounded |
| Migration ledgers diverge | Atomic migration plus parity/checksum tamper tests | SQLx and public ledger cannot be applied and verified atomically |
| SDK schema changes weaken Draft 2020-12 | Generated-schema validation and exact public examples | Required formats, unknown-field rejection, output schemas, or annotations cannot be represented |
| DNS rebinding/redirect reaches denied address | Request-scoped guarded resolver/connector, all-address rejection, no pool reuse, hop validation, no proxy | Reqwest performs an unguarded second resolution, reuses an unvalidated connection, or connects outside the validated set |
| File alias escapes root | Root descriptor plus Linux `openat2` beneath/no-symlink resolution and race tests | The required Linux runtime lacks `openat2` or containment cannot be proved without a canonicalize/stat race |
| Workload stop harms control plane | Persistent route and concrete runtime tests | WebUI/API/liveness require restarting process/listener |
| Backup misses WAL or is corrupt | SQLite-native snapshot, fsync, full validation | Snapshot requires raw file copy or checkpoint is busy/unverified |
| Restore leaves partial live state | Same-filesystem staging, journal, pre-backup, failure matrix | Any tested phase can leave neither original nor replacement verified |
| Browser credential leakage | In-memory token, HttpOnly cookie, storage/network/DOM scan | Password/token appears in storage, response, log, audit, screenshot, or fixture |
| Performance misses target | Early 100k query plan and final reference run | p95 exceeds 250 ms without an explicit accepted deviation |

Additional mandatory stops:

- A task needs a new service, database, framework, transport, deployment surface,
  arbitrary execution capability, or production action not authorized here.
- A required secret/credential is absent for a real external action. Local tests
  continue with generated ephemeral values.
- A source-document contradiction changes product behavior rather than a narrow
  compatibility spelling or internal mechanism.
- A test failure is hidden, skipped, weakened, or reclassified merely to pass a
  gate.

## 19. Final implementation handoff checklist

- [ ] All eight workstream gates are accepted in order.
- [ ] Every goal, functional requirement, non-functional requirement,
      acceptance check, and Definition of Done row links to evidence.
- [ ] Exact tool/config/route/schema/error inventories match governing sources.
- [ ] Targeted, broad, browser, container, security, recovery, and performance
      checks pass.
- [ ] Real browser and MCP clients exercised the built container.
- [ ] Backup and restore succeeded, and failure rollback/process-loss recovery
      were demonstrated.
- [ ] The final diff has no secrets, unrelated changes, generated databases,
      browser artifacts, unsupported claims, or unrequested architecture.
- [ ] `README.md`, `AGENTS.md`, `ACCEPTANCE.md`, runtime behavior, and Compose
      agree.
- [ ] Verified commits are pushed only to the intended branch; no production
      deployment or image publication occurs without separate authority.
