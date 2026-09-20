# neo-cucumber

NEO-compatible drawing canvas for offline, collaborative, two-tone, relay,
banner, and replay workflows.

## Library boundary

The public contract is defined in [`src/public.ts`](src/public.ts). The canvas
internals and imperative mount implement this lifecycle and are distributed as
the package's root export.

The library's core is the drawing canvas and this lifecycle:

```ts
import { mount } from "neo-cucumber";
import "neo-cucumber/style.css";

const painter = mount(element, {
  width: 640,
  height: 480,
  mode: { kind: "standard" },
  controls: { kind: "toolbox" },
});

await painter.ready;
await painter.loadImage(parentImage);

painter.setInteractionEnabled(false);
painter.undo();
painter.redo();
painter.setInteractionEnabled(true);

const { png, replay, strokeCount } = await painter.save();
painter.unmount();
```

`save()` is an atomic local export. It never uploads, redirects, or calls a
native bridge. Hosts own persistence and completion behavior.

The built-in toolbox is an optional maintained preset. Its React components,
props, drawing state, and callbacks are not part of the public API. Consumers
that do not request it receive the canvas alone.

## Chrome for host controls

A host renders controls the painter does not own -- a chat panel, a Save
button, a session header -- and they sit beside the toolbox, where anything
close to NEO's chrome but not equal to it reads as broken. The package exports
the chrome as class names so a host can wear it without copying values:

```ts
import { NEO_PANEL, NEO_TITLEBAR, NEO_PANEL_BUTTON } from "neo-cucumber";
import "neo-cucumber/style.css";

<div className={`${NEO_PANEL} flex flex-col`}>
  <div className={NEO_TITLEBAR}>Chat</div>
  …
  <button className={NEO_PANEL_BUTTON}>Save</button>
</div>
```

A host panel that should behave like one of the painter's windows -- a title
bar with three dots and no title, grabbed to move it -- uses
`NEO_TITLEBAR_HANDLE`, three `NEO_TITLEBAR_DOT` spans, and `attachWindowDrag`,
which is the gesture the painter's own windows use:

```ts
useEffect(
  () => attachWindowDrag(frameRef.current!, handleRef.current!, {
    minimumY: 70,
    onPosition: setPosition,
  }),
  [],
);
```

It reports positions rather than applying them, so a React host can keep them
in state and a plain DOM host can write them to `style`. It returns the
function that detaches it.

`NEO_PANEL`, `NEO_TITLEBAR`, `NEO_TITLEBAR_HANDLE`, `NEO_TITLEBAR_DOT`,
`NEO_BUTTON`, `NEO_ICON_BUTTON`, `NEO_PANEL_BUTTON`, `NEO_BUTTON_ON`,
`NEO_FIELD`, `NEO_WELL`, and `NEO_KBD` are defined in
[`src/styles.ts`](src/styles.ts). They name rules carried by
`neo-cucumber/style.css`, so importing that stylesheet is the whole
requirement -- a host that does not use Tailwind gets the same chrome, because
the utilities are already compiled into it. Colours beyond what these names
cover are available as the `--neo-*` custom properties the same stylesheet
defines, which follow the light and dark palettes.

A host that does compile its own Tailwind gets the toolbox's utilities from
the same import: `src/App.css` declares the package's sources itself, so no
consumer has to name a path inside neo-cucumber. Import it by package
specifier (`@import "neo-cucumber/style.css"`) rather than by relative path,
and the CSS reads identically whether it resolves through a workspace alias or
through an installed package.

## Browser support

The published bundles are built for **Firefox 56** and newer, which is what
Waterfox Classic runs. That is a deliberate floor rather than an accident of
tooling: the browsers still carrying NPAPI are the ones oekaki users keep for
the original PaintBBS and ShiPainter applets, and the painter should open for
them too. Syntax is lowered by esbuild and the stylesheet ships without
cascade layers; see `vite/legacyBrowsers.ts`.

Flexbox `gap` landed in Firefox 63, so on older engines `gap-*` is spaced the
way NEO spaces things instead — a margin on the item rather than a gutter
distributed by the container — and grids fall back to `grid-gap`. This is
generated into the stylesheet behind `@supports not (row-gap:1px)`, so a
browser that has `gap` never reads it and the cascade there is unchanged.

A wrapping row is the one case it does not reproduce exactly: its second line
keeps a leading margin that `gap` would have dropped.

Hosts that compile their own Tailwind should know that `neo-cucumber/style.css`
is emitted unlayered, so its rules take part in the normal cascade.

## Contract principles

- No React types in the public interface.
- No host routes, identifiers, submission formats, or native bridge.
- Standard and two-tone behavior are core canvas modes.
- Continuation images are core through `loadImage()` and are self-contained in
  exported `.pch` files by default.
- PNG and replay exports do not mutate visible artwork.
- `unmount()` is idempotent and all later asynchronous operations reject.
- Public compatibility applies only to exports from `src/public.ts` once a
  package build is introduced.

## Current application builds

```bash
pnpm run build
```

In this monorepo, this builds the host collaborative application into `dist/`,
the standalone example into `dist-example/`, the replay viewer, a host-owned
offline auto-mounting adapter, and the package in `dist-lib/`. The host-specific
adapter lives outside the library source at
[`../frontend/painter/entry.ts`](../frontend/painter/entry.ts).

[`example.html`](example.html) is the library usage example. It mounts the
painter through `src/public.ts`, opts into the built-in toolbox, exports PNG and
replay data, and opens the replay viewer without relying on private components.
Run `pnpm dev` and open `/example.html`. The host application has a separate
`pnpm dev:collaborate` server and retains its `/collaborate/` base path.
Open the example link in another window to draw collaboratively through its
in-browser canonical sequencer; a `?room=` query parameter selects the room.

The package exposes only `neo-cucumber` -- the painter lifecycle, the
transport-neutral operation types, and the chrome class names -- and
`neo-cucumber/style.css`. React and React DOM are peer dependencies; private
canvas and toolbox modules are not package exports.

## Collaborative consumers

The example host collaborative application lives in
[`../frontend/collaborate`](../frontend/collaborate). Authentication, room
metadata, WebSocket lifecycle, chat, participants, and session UI are consumer
responsibilities.

The root package also exports transport-neutral `PainterOperation`,
`CanonicalPainterOperation`, `PainterCheckpoint`, and `PainterSessionArchive`
types. A host enables controlled mode with a stable actor ID and sends emitted
operations through its own transport:

```ts
const painter = mount(element, {
  width: 640,
  height: 480,
  mode: { kind: "standard" },
  controls: { kind: "toolbox" },
  synchronization: {
    actorId: currentParticipantId,
    onOperation: (entry) => socket.send(encodeForMyProtocol(entry)),
  },
});

socket.onmessage = async (event) => {
  const canonical = decodeMyProtocol(event.data);
  await painter.applyCanonicalOperation(canonical);
};
```

The host may use `exportCheckpoint(sequence)` and `applyCheckpoint(checkpoint)`
to compact or join a room. `exportSessionArchive()` preserves actor IDs and
canonical ordering since the last applied checkpoint.

`.pch` remains a NEO-compatible, single-canvas replay. It has no actor or
concurrency model, so simultaneous participants are represented by their
canonical server order rather than as simultaneous tracks. Consumers that need
authorship, exact interleaving, or later collaborative editing should persist
the session archive (or a custom serialized form of it) alongside the flattened
`.pch` export.
