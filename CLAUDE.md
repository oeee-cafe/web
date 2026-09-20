# oeee-cafe

## Main Rust server (`./src`)

Don't try to run the development server. Just run `cargo check` if you need to check if the code compiles.

Don't run `cargo sqlx prepare`.

When running cargo commands, use the environment variable `DATABASE_URL=postgresql:///oeee_cafe`.

When running psql commands, specify the database name like `psql oeee_cafe`.

When creating SQLx migrations, use the command `sqlx migrate add`.

Never use a POSIX character class — `[[:alnum:]]`, `[[:alpha:]]`, `[[:punct:]]`
— in SQL that touches user text. PostgreSQL delegates them to the host's C
library, and on the production Mac mini `iswalnum()` answers **false** for
Hangul, kana and Han:

```sql
SELECT regexp_replace('그림', '[^[:alnum:]_]', '', 'g');  -- ''
```

`20260828204049_merge_and_normalize_hashtags` normalised tag names that way and
deleted every Korean, Japanese and Chinese tag on the site, because a name that
normalises to nothing was treated as a name made only of punctuation. It was
tested against a local database seeded with ASCII, which is the same shape of
mistake as not testing it at all. Write the range out (`[a-zA-Z0-9_]`, as every
other constraint in `migrations/` does) when ASCII really is what you mean, and
when it is not, do the work in Rust — `char::is_alphanumeric` is Unicode-aware —
or check the expression against real non-ASCII rows on the *server*, not on a
Linux CI box whose locale data disagrees with the one that will run it.

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

### The browser floor is Firefox 56

Every bundle here is built for **Firefox 56**, which is Waterfox Classic's
engine — the last Gecko with NPAPI, and so the browser oekaki users keep
around for the original PaintBBS and ShiPainter applets. Somebody opening this
painter in it is usually that same person. `vite/legacyBrowsers.ts` holds both
halves and the reasoning; all six vite configs import it.

Two things there are easy to undo by accident:

- **`build.target`.** Without it Vite emits ES2022, and Firefox 56 rejects the
  file whole — the page is blank with one `SyntaxError: missing : after
  property id`, which is what a class field looks like to that parser. Class
  fields, `??`, `?.` and optional catch binding all arrive this way from React
  and from our own source; esbuild lowers them, so nothing needs writing in an
  older style.
- **`legacyCss()`.** Tailwind v4 puts 86% of its output inside `@layer`, which
  is Firefox 97, and an engine that does not know an at-rule drops the block
  entire — the painter arrives with a palette and no chrome at all. The plugin
  unwraps the layers in place, which also frees the `@supports` fallback that
  seeds every `--tw-*` property.

`vite/legacyBrowsers.test.ts` builds the real offline bundle and parses it, so
a dependency bump that reintroduces either one fails the node project rather
than the browser nobody here runs. Check it fails when you expect it to: an
earlier version of that test quietly loaded `vite.config.ts` instead of the
config under test and passed no matter what.

Flexbox `gap` is Firefox 63, so `gap-*` utilities would do nothing there and
every toolbox row would sit flush. `addFlexGapFallback()` spaces them the way
NEO does — NEO never asks a container to distribute space, it hangs the
spacing on the item (`.toolTipOff` carries `margin-top: 3px`, `.colorTipOff`
carries `margin-right: 4px`), and the fallback is that same margin, selected
for with `> * + *` instead of written onto every element. Grids get
`grid-gap`, which Firefox 56 has had since Grid shipped in 52 and which gets
a new row's first item right where a sibling margin would indent it.

All of it sits inside `@supports not (row-gap:1px)`, so no engine that has
`gap` reads a rule of it and the modern cascade is untouched — which is why
this is generated rather than written into the 49 call sites.

Two things it does not reproduce, both pinned in
`vite/legacyBrowsers.browser.test.ts`: a wrapping row keeps the leading
margin `gap` would have dropped on its second line, and on the axis being
spaced the fallback outranks a child's own `ml-*`/`mt-*` (so `NeoWindow`'s
title sits 3px from the dots rather than 7px). The other axis is untouched,
which is what keeps the modals' `mb-*` intact.

Runtime APIs are a separate matter from syntax: esbuild lowers grammar and
nothing else. `structuredClone` (94), `Array.prototype.flatMap` (62) and
`ResizeObserver` (69) each needed handling in source. React's own uses of
`queueMicrotask`, `reportError` and `AbortController` are already
feature-detected, so they need nothing.

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
