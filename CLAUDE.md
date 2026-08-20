# oeee-cafe

## Main Rust server (`./src`)

Don't try to run the development server. Just run `cargo check` if you need to check if the code compiles.

Don't run `cargo sqlx prepare`.

When running cargo commands, use the environment variable `DATABASE_URL=postgresql:///oeee_cafe`.

When running psql commands, specify the database name like `psql oeee_cafe`.

When creating SQLx migrations, use the command `sqlx migrate add`.

### Deploys

`deploy.sh` is blue/green: `oeee-cafe-blue` and `oeee-cafe-green` take turns,
and the `proxy` container (Caddy) owns the published port so it is never
rebound. Which colour is live is written in `proxy/upstream.caddy` — that file
is generated and gitignored, and reading it is how anything else (`cli.sh`, a
rollback) finds the serving container.

Two consequences for anything that touches the database:

- The new colour runs `sqlx::migrate!()` on boot while the old one is still
  serving, so a migration has to leave the *previous* release able to run for
  the length of a deploy. Split anything destructive across two deploys.
- Both colours are briefly up at once, each with its own pool, against a
  Postgres shared with other apps on that host. Watch `db_max_connections`.

Collaborative session recordings (`handlers/collaborate/archive.rs`) go to
`archive_s3_bucket`, which has to be a **separate bucket with no public
access**. `aws_s3_bucket` is served straight to browsers from
`r2_public_endpoint_url`, and a public session's id is printed on the lobby for
anyone to read — so a recording written there would be downloadable by anyone
who loaded `/collaborate`. Unset means nothing is recorded, which is the
intended default for a deployment that has not been given somewhere private to
put them. `config/` is gitignored, so this lives only on the server.

Rolling back before the next deploy: point `proxy/upstream.caddy` at the other
colour, `docker compose start` it, and
`docker compose exec proxy caddy reload --config /etc/caddy/Caddyfile`. The
previous colour's container and image are kept until the next deploy
recreates it.

### Templates

Templates are loaded and evaluated at runtime, so `cargo check` says nothing
about them — a mistake surfaces as a 500 when someone requests the page. After
editing anything under `templates/`, run:

```bash
DATABASE_URL=postgresql:///oeee_cafe cargo test --lib template_tests
```

`every_template_parses` covers syntax for all of them. Parsing is not enough on
its own: the context hands templates strings, so `{{ post.image_width + 24 }}`
parses fine and fails at render. Catching that needs a fixture whose types
match the real context, as in `replay_pages_render_and_mount_the_viewer`. Add
one when a template starts doing more than interpolate.

When connecting to PostgreSQL via command line, use `psql oeee_cafe`.

## neo-cucumber (`./neo-cucumber`)

Don't try to run the development server. Just run `pnpm run build` if you need to check if the code compiles.

`dist/`, `dist-viewer/`, `dist-offline/`, and `dist-replay/` are build output and are not tracked. The Rust
server serves `neo-cucumber/dist-viewer` at `/static/viewer/`, which the replay
templates request, so a checkout that has never been built will 404 there until
`pnpm run build:viewer` has run once. The normal drawing routes similarly serve
`neo-cucumber/dist-offline` at `/static/neo-cucumber/`, and the staff-only
session replay viewer serves `neo-cucumber/dist-replay` at `/static/replay/`;
`pnpm run build` builds all of them, and Docker builds them itself.

Always run and check linting:

```bash
pnpm run lint
```

Run the tests (vitest) after changing collaboration logic, the drawing engine,
or replay recording:

```bash
pnpm run test            # both projects
pnpm run test:node       # pure logic (replay format, action bookkeeping)
pnpm run test:browser    # real Chromium: canvas rasterisation, React hooks
```

The browser project needs Chromium once: `pnpm exec playwright install chromium`.

### Fidelity to NEO

`./neo` is the canonical PaintBBS NEO implementation and is the reference for
anything touching drawing or replay. `src/test/neoHarness.ts` loads
`neo/src/painter.js` into the test page, so our engine and our `.pch` files are
checked against NEO itself rather than against a description of it:

- `src/DrawingEngine.browser.test.ts` — our rasterisation vs NEO's, pixel for pixel.
- `src/utils/replayRoundTrip.browser.test.ts` — recorded replays re-rendered by NEO.
- `src/hooks/offlineDrawing.browser.test.tsx` — real pointer events through the
  offline hook, then the resulting replay re-rendered by NEO.

A replay that renders differently from the canvas it was recorded on is the
worst failure this codebase can produce. Prefer matching NEO's behaviour, quirks
included, over "fixing" it — a divergence breaks every file we have already
written.

When extracting and compiling Lingui locales, use these commands:

```bash
pnpm run extract
pnpm run compile
```

### Icons

Icons are drawn by `src/components/Icon.tsx`, never by `@iconify/react`
directly — it registers bundled artwork so an icon paints with the frame that
asks for it instead of after a request to Iconify's API, which the offline
bundle cannot make at all. Hosts under `frontend/` import `Icon` from
`neo-cucumber`. After using a `material-symbols:` name that was not used
before, regenerate the bundled set:

```bash
pnpm run build:icons
```

`materialSymbols.test.ts` fails when a referenced icon is not bundled.
