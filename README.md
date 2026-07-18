# Remindi

Remindi is a single-owner, self-hosted reminder service. One container provides
the authenticated WebUI, Streamable HTTP MCP endpoint, scheduler, named
read-only condition adapters, administration, and verified backup/restore.

## Run with Docker Compose

Create a private, uncommitted environment file:

```dotenv
REMINDI_OWNER_ID=owner
REMINDI_MCP_TOKEN=replace-with-a-long-random-token
REMINDI_WEBUI_USERNAME=owner
REMINDI_WEBUI_PASSWORD=replace-with-a-strong-password
```

Then start the service:

```sh
docker compose --env-file .env up --build -d
docker compose ps
```

The WebUI is published at `http://127.0.0.1:8000` by default. The MCP endpoint
is `/mcp` and requires `Authorization: Bearer <REMINDI_MCP_TOKEN>` on every
request. The WebUI credentials do not authorize MCP, and the MCP token does not
create a browser session.

Only `/data` is persistent and writable. It contains `remindi.db` and the
`backups/` directory. The image runs as numeric UID/GID `10001:10001` with a
read-only root filesystem and no Docker socket or container-control access.

## Configuration boundary

Bootstrap settings are read from the environment once at startup. Identity,
credentials, bind and filesystem paths are deliberately not WebUI-editable.
Important variables are:

| Variable | Default | Purpose |
|---|---|---|
| `REMINDI_OWNER_ID` | required | Fixed owner for this container |
| `REMINDI_MCP_TOKEN` | required | Dedicated MCP bearer credential |
| `REMINDI_DB_PATH` | `/data/remindi.db` | Absolute SQLite path |
| `REMINDI_BACKUP_DIR` | `/data/backups` | Protected backup directory |
| `REMINDI_WEBUI_ENABLE` | `true` | Serve the WebUI and JSON API |
| `REMINDI_WEBUI_AUTH` | `true` | Require browser sign-in |
| `REMINDI_WEBUI_USERNAME` | required with auth | Browser username |
| `REMINDI_WEBUI_PASSWORD` | required with auth | Browser password |
| `REMINDI_WEBUI_COOKIE_SECURE` | `false` | Set `true` behind HTTPS |
| `REMINDI_HTTP_ALLOWED_HOSTS` | empty | Optional request Host allowlist |
| `REMINDI_HTTP_ALLOWED_ORIGINS` | same-origin policy | Optional MCP Origin allowlist |
| `REMINDI_LOG_LEVEL` | `info` | Structured-log filter |
| `REMINDI_LOG_CONTENT` | `false` | Explicitly allow content logging |

`REMINDI_WEBUI_HOST` and `REMINDI_WEBUI_PORT` are Compose interpolation inputs,
not application settings. The application always listens on
`0.0.0.0:8000` inside the container. SQLite-backed scheduler, adapter, reminder,
idempotency, and backup settings are the only runtime-editable configuration.

## Network and TLS

The default Compose mapping is loopback-only. For remote access, terminate TLS
at a trusted authenticated reverse proxy and set:

```dotenv
REMINDI_WEBUI_HOST=0.0.0.0
REMINDI_WEBUI_COOKIE_SECURE=true
```

Also configure explicit allowed hosts/origins. Do not expose plaintext browser
or MCP credentials across an untrusted network. Outbound network access is only
needed for enabled network adapters and should be restricted to approved
destinations.

## Health and workload control

`GET /health/live` only proves that the control plane responds. Detailed
readiness is authenticated and distinguishes an intentionally stopped MCP or
scheduler workload from failure. Stopping MCP returns `503` from `/mcp`;
stopping the scheduler halts background evaluation. The WebUI,
authentication, backup, and control APIs remain available. Desired workload
state survives container restart.

## Backup and restore

Create and download backups through the authenticated WebUI. Remindi uses
SQLite-aware backup, verifies integrity and application invariants, records a
SHA-256 digest, and stores a matching manifest under `/data/backups`. Do not
copy a live SQLite database file.

Before relying on restore, exercise it on the actual deployment. Restore
requires recent password reauthentication and the exact phrase
`RESTORE REMINDI`; it creates a verified pre-restore backup, quiesces only MCP
and scheduler, atomically swaps the database, and rolls back on failure.
Keep independent encrypted copies of verified backups outside this volume.

To stop the container without deleting data:

```sh
docker compose down
```

Do not add `--volumes` unless permanent data deletion is intended.

## File permissions and limits

The `/data` directory and backup directory should be mode `0700`; database and
backup files are owner-readable only (`0600`). Run one write-capable container
per database. The deployment is intentionally single-owner and does not provide
multi-tenancy, high availability, external notifications, a Docker socket, or
host-service control.

## Agent pull workflow

Add the following project-level guidance for clients that use Remindi:

```markdown
## Remindi workflow

For every non-trivial task:

1. Search Memory for relevant user, project, task, service, and prior decisions.
2. Call `remindi_check` for the current project at task start; the server supplies the configured owner identity.
3. Call `remindi_check` again at meaningful checkpoints and continuations.
4. Add due and overdue Remindi items to the task plan; do not silently defer them.
5. Do not mark a Remindi item complete without structured evidence.
6. Snooze only with a reason and an explicit next check time.
7. Before the final response, call `remindi_check` with `final_review`.
8. Complete or cancel Remindi items satisfied or invalidated by the work, with evidence or a cancellation reason.
9. If the Remindi service is unavailable, record that limitation in the final response and preserve the required follow-up in the task plan or Memory.
```

Use `task_start`, `checkpoint`, `continuation`, and `final_review` lifecycle
events at their corresponding agent checkpoints. Pass a stable `project_id` and
the available task, session, lineage, and active-goal identifiers.
