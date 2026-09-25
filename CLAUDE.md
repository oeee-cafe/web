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

`mise run deploy` (`deploy.py`, standard-library Python) runs on the
development Mac, not the server: it builds `oeee-cafe:<commit>` from
origin/main in a clean checkout of its own, ships the image over ssh (host
alias `oeee-cafe-deploy` in `~/.ssh/config`), and drives the switch there one
ssh command at a time. Nothing compiles on the server, and its compose file
has no `build:` on purpose.

The server's `~/oeee-cafe-data` is not a checkout and runs no script of its
own. Each deploy copies in `docker-compose.yml` and `proxy/Caddyfile` from
the commit being deployed, and the production config from
`~/.config/oeee-cafe/production` on the Mac into the idle colour's
`config-blue/` or `config-green/`. That Mac directory is the source of truth:
an edit made on the server is replaced by the next deploy. Each colour keeps
its own copy so that a rollback boots with the config its release shipped
with.

The server reads every key file its config names (`*_path`) when it loads the
config, into a `KeyFile`, and never from disk at a request: one that is
missing or does not parse stops it booting, which a deploy sees as a colour
that never answers `/health`, so the colour already serving stays. A new key
file belongs in `AppConfig::load_keys`, not in the code that uses it.

The image's binary carries no debug info: the Dockerfile splits it off and
`deploy.py` uploads it to Sentry (`sentry-cli` must be logged in), which is
where file and line come back. Each deploy is a Sentry release named by its
full commit, which is also what the server reports as its release
(`build_info::git_commit`), with the deploy or rollback recorded against it.
`mise install` provides `sentry-cli`, `sqlx-cli` and `ruff` at the versions
`deploy.py` expects; `zstd` comes from Homebrew. The admin CLI is a subcommand of the one
binary, `./oeee-cafe cli ...`, not a second binary — that one cost 200MB of
every image. `mise run cli -- <command>` runs it on the server, in whichever
colour is serving.

The switch is blue/green: `oeee-cafe-blue` and `oeee-cafe-green` take turns,
and the `proxy` container (Caddy) owns the published port so it is never
rebound. Which colour is live is written in `proxy/upstream.caddy` — that file
is generated and gitignored, and reading it is how anything else (`mise run cli`,
a rollback) finds the serving container.

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
put them. It is set in the production config on the development Mac.

Rolling back before the next deploy is `mise run rollback`: it starts the
stopped colour as it was, with its own image and config, and hands the proxy
back to it the way a deploy does. The previous colour's container, image and
config are kept until the next deploy recreates it.

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
match the real context, as in `a_replay_is_on_the_stage_and_the_drawing_is_not_a_link`. Add
one when a template starts doing more than interpolate.

When connecting to PostgreSQL via command line, use `psql oeee_cafe`.

## neo-cucumber (`./neo-cucumber`)

Don't try to run the development server. Just run `pnpm run build` if you need to check if the code compiles.

`dist/`, `dist-viewer/`, `dist-offline/`, and `dist-replay/` are build output and are not tracked. The Rust
server serves `neo-cucumber/dist-viewer` at `/static/viewer/`, which the post
page's inline replay requests, so a checkout that has never been built will 404 there until
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
- `src/neo/shapeReplay.browser.test.tsx` and `src/neo/copyPaste.browser.test.tsx`
  — the same, for the region tools and for copy and paste.

Test a tool through the gesture, not only through the recorder. For six weeks
every rectangle and ellipse drawn here replayed as blank, in NEO and in our
own viewer, because the drag left the shape type off the `fill` frame; the
round-trip suite passed throughout, since it called the recorder itself and
supplied the type. The frames worth checking are the ones a drag produces.

Two NEO behaviours that look like bugs and are load-bearing: zoom above 1x
only ever moves in whole steps (a fractional step makes some pixels one
screen pixel wide and some two, and one pixel lines wobble), and paste
*replaces* its rectangle, transparent pixels included — a copy of empty
canvas cuts a hole. Copy hands over to paste by itself; there is no paste
button in NEO and there is none here.

When matching what NEO *shows*, remember it has no preview layer. It XORs
outlines straight into its display and leaves them until something redraws
it, so what is on screen depends on which handlers redraw and which do not.
After a copy the selection rectangle stays up (`EffectToolBase.upHandler`
skips its redraw when the new tool is paste); the paste press XORs the same
rectangle again and erases it; the drag redraws with the copy on top and no
outline. `PasteDisplay` models that as marks combined by XOR parity rather
than as a preview drawn fresh each frame — read the handlers, not the
screenshots, and it comes out the same.

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

The same plugin repairs three more things that each look like a different bug
and are all one shape — modern CSS that an old engine discards rather than
degrades:

- **Selector lists.** One unparseable selector voids the *whole* list, by the
  spec, not by quirk. Tailwind declares its theme on `:root,:host`, and
  `:host` is Firefox 63, so 56 lost `--color-white` and `--spacing` together:
  the canvas painted in the page's own background because `bg-white` had
  nothing to resolve. `addLegacyFallbacks()` re-emits the readable selectors
  as their own rule just before the original. `:is()`, `:where()`, `:has()`
  and `::file-selector-button` get the same treatment.
- **Shorthands newer than the engine.** `inset`, `padding-inline`,
  `padding-block`, `margin-inline` and `margin-block` are Firefox 66;
  unprefixed `user-select` is 69 and `tab-size` is 91. Each is spelled out
  into longhands or a `-moz-` prefix immediately before itself — never
  hoisted, because `padding:1px;padding-inline:4px` does not mean what
  `padding-inline:4px;padding:1px` means.
- **Gradient colour stops.** Two positions on one stop (`transparent 0 14px`)
  is Firefox 83, and a gradient that will not parse takes the whole
  `background-image` with it. That is why `.neo-ground` in `src/App.css`
  writes each stop twice; it is the grid behind the canvas, and the symptom
  was a flat field.
- **`image-rendering`.** `pixelated` is Firefox 93 and `crisp-edges` is 65;
  `-moz-crisp-edges` has been there since 3.6. Losing this does not make a
  painter slightly worse, it makes it smoothed — the drawing canvas and the
  replay canvas are both scaled up and both ask for hard pixels.

- **Whitespace a minifier is entitled to drop.** `var(--neo-bk2)14px` is two
  tokens by the spec, so esbuild removes the space between them. Gecko's
  first custom-property implementation substituted by re-serialising the
  value and parsing the text again, which joins them back into one run.
  `separateAfterFunctions()` puts the space back — never before `-`, `+`,
  `*` or `/`, because inside `calc()` that whitespace is part of the grammar.

That last one is the lesson worth keeping, because of how it presented: "the
grid is missing in light mode; dark mode is fine". A symptom in one theme and
not the other reads like a colour or contrast problem, and it is worth
measuring luminance exactly once before noticing that `.neo-ground` is a
single rule that mentions neither theme. What differed was the *length* of
the substituted value: `#bbf` and `14px` join into `#bbf14px`, a five
character hex run and not a colour, where `#22223f` and `14px` join into
`#22223f14px`, whose run of eight is one. Light lost the whole
`background-image`; dark kept its line.

So when a theme-specific symptom has no theme-specific rule behind it, stop
looking at the colours and look at what their token text does to the
characters beside it.

`static/style.css` is served as written and never sees this build pass, so
the prefixed spelling is in that file by hand for
`.neo-cucumber-replay-canvas`.

Every pass only ever *adds*. No rule is dropped and no declaration rewritten
in place, so an engine that understood the input still computes exactly what
it did before.

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
