# Customizing Remindi

Remindi supports four startup-time WebUI customizations:

| Customization | Environment variable | Starter file |
|---|---|---|
| Page and brand title | `REMINDI_WEBUI_TITLE` | None; this is text set directly in the environment |
| Additional CSS | `REMINDI_WEBUI_CUSTOM_CSS_FILE` | [`app.css`](app.css) |
| Header and login logo | `REMINDI_WEBUI_LOGO_FILE` | [`logo.svg`](logo.svg) |
| Browser favicon | `REMINDI_WEBUI_FAVICON_FILE` | [`favicon.svg`](favicon.svg) |

The three files in this directory are copies of the embedded defaults in
`src/webui/static/`. Editing them does not change Remindi until you mount or
otherwise expose the files to the process and set the matching environment
variables.

Remindi reads the title and asset files once during startup. Restart or recreate
the service after every change.

## Docker Compose

The paths passed to Remindi must exist inside the container. Host paths in
`.env` are not automatically mounted.

### 1. Set the page title

Add the title to the repository's `.env` file:

```dotenv
REMINDI_WEBUI_TITLE="Home Reminders"
```

The default is `Remindi`. The title is HTML-escaped and appears in:

- the browser tab title;
- the WebUI header and brand accessibility label; and
- the login dialog.

It does not rename MCP tools, API routes, the Remindi item model, or static
interface labels that explicitly use the product name.

The supplied Compose expression treats an unset or blank title as `Remindi`.
A directly launched native process can receive an explicitly empty title and
will render it blank.

### 2. Mount the customization files

Create `compose.override.yaml` beside the main `compose.yaml`:

```yaml
services:
  remindi:
    environment:
      REMINDI_WEBUI_CUSTOM_CSS_FILE: /customization/app.css
      REMINDI_WEBUI_LOGO_FILE: /customization/logo.svg
      REMINDI_WEBUI_FAVICON_FILE: /customization/favicon.svg
    volumes:
      - ./customization/app.css:/customization/app.css:ro
      - ./customization/logo.svg:/customization/logo.svg:ro
      - ./customization/favicon.svg:/customization/favicon.svg:ro
```

Omit an environment entry and its mount when you do not want to override that
asset.

Ensure the files are readable by the container and not world-writable:

```bash
chmod 0644 customization/app.css \
  customization/logo.svg \
  customization/favicon.svg
```

### 3. Validate and recreate Remindi

```bash
docker compose config
docker compose up -d --build --force-recreate remindi
docker compose ps remindi
```

Using `--force-recreate` applies both changed environment values and changed
startup-loaded files. A plain container restart does not apply a changed title
or environment path.

Open the WebUI and perform a hard refresh. If Remindi does not become healthy,
inspect the startup error:

```bash
docker compose logs --tail=100 remindi
```

## Native process

For a native process, use absolute host paths:

```bash
export REMINDI_WEBUI_TITLE="Home Reminders"
export REMINDI_WEBUI_CUSTOM_CSS_FILE="$PWD/customization/app.css"
export REMINDI_WEBUI_LOGO_FILE="$PWD/customization/logo.svg"
export REMINDI_WEBUI_FAVICON_FILE="$PWD/customization/favicon.svg"

cargo +1.97.1 run --release --locked
```

Set the other required Remindi variables before starting the process. See the
repository [`.env.example`](../.env.example) for the complete environment
contract.

## Custom CSS

`app.css` is an exact copy of Remindi's embedded stylesheet. Remindi serves the
custom stylesheet after the embedded stylesheet, so the copied rules initially
produce the same appearance and become overrides when edited.

The fastest way to change the color scheme is to edit the custom properties at
the top of `app.css`:

```css
:root {
  --bg: #080a0f;
  --panel: #11151f;
  --text: #edf2ff;
  --accent: #56b6ff;
  --accent-2: #7ce7c6;
}
```

You can keep the full starter stylesheet or replace it with a smaller file that
contains only the selectors and variables you want to override.

Requirements:

- maximum size: 256 KiB;
- valid UTF-8 text;
- absolute path at the Remindi process boundary;
- regular, readable, non-world-writable file.

An empty custom CSS file is accepted and produces no additional styles.

## Logo

Edit or replace `logo.svg`, keeping the configured filename extension
consistent with its real content.

Supported formats are:

- SVG;
- PNG;
- JPEG;
- GIF; and
- WebP.

The maximum size is 2 MiB. SVG files must not contain scripts,
`javascript:` URLs, or `foreignObject`. The header displays the logo within a
`7rem` by `2rem` contained area; the login dialog displays it at `9rem` wide.

An empty logo file retains the embedded default logo.

## Favicon

Edit or replace `favicon.svg`. Supported formats are SVG, PNG, JPEG, GIF, WebP,
and ICO. The maximum size is 512 KiB.

The filename extension and file signature must agree. An empty favicon file
retains the embedded default favicon.

## Validation and startup behavior

Every configured asset must be:

- an absolute path from the running process's perspective;
- a regular file rather than a directory, device, or socket;
- readable by the Remindi process;
- not world-writable; and
- within its size and content-type limits.

Remindi validates and loads all configured assets before serving requests. A
missing, unreadable, unsafe, oversized, or unsupported asset prevents startup.
Changing a mounted file while Remindi is running has no effect until restart.

Database and backup paths are runtime data locations, not WebUI customization
templates. `index.html`, `app.js`, and the embedded base stylesheet cannot be
replaced directly through environment variables.

## Reset to the defaults

To restore the embedded assets:

1. Remove the three file environment entries from `compose.override.yaml`.
2. Remove their bind mounts.
3. Set `REMINDI_WEBUI_TITLE=Remindi` or remove the title from `.env`.
4. Recreate the service:

   ```bash
   docker compose up -d --force-recreate remindi
   ```

## Upgrades

The files in this directory are snapshots of the embedded assets at the
repository revision you checked out. Remindi does not automatically update
custom copies during an upgrade.

Review upstream asset changes before deploying a new version:

```bash
diff -u src/webui/static/app.css customization/app.css || true
diff -u src/webui/static/logo.svg customization/logo.svg || true
diff -u src/webui/static/favicon.svg customization/favicon.svg || true
```

A fully copied `app.css` can continue overriding newer embedded layout rules.
After an upgrade, either merge relevant upstream changes or reduce the custom
stylesheet to only the rules you intentionally override.
