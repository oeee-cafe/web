import { afterEach, describe, expect, it } from "vitest";
import { Profiler, act, createRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import Painter from "../Painter";
import { DefaultI18n } from "../components/DefaultI18n";
import { PainterLabelContext } from "../hooks/usePainterLabels";
import { setupI18n } from "../utils/i18n";
import type { PainterHandle } from "../public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * What a message from the canonical stream costs the rest of the painter.
 *
 * A room where three people are drawing delivers something like eighty
 * canonical messages a second, and every one of them lands on this component
 * before it reaches a canvas. The history reports its undo/redo flags after
 * each one, and for a while that report was a fresh object -- so React
 * re-rendered the whole painter, both toolbox columns and every button in
 * them, eighty times a second, on the same thread that was meant to be
 * following somebody's pen. The flags themselves move a handful of times in a
 * session.
 *
 * So this counts commits rather than measuring time: the cost being guarded
 * against is React's, and a burst of remote marks that changes nothing about
 * what the local user can undo must not reach React at all.
 */

let host: HTMLElement | null = null;
let root: Root | null = null;

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  host?.remove();
  host = null;
});

const remoteDot = (x: number, y: number) => ({
  kind: "stroke" as const,
  layer: "foreground" as const,
  brushSize: 1,
  brush: "solid" as const,
  color: { r: 20, g: 30, b: 40, a: 255 },
  points: [{ x, y }],
  mask: { type: 0, r: 0, g: 0, b: 0 },
});

describe("the cost of a canonical message", () => {
  it("does not re-render the painter for remote marks that change nothing it shows", async () => {
    setupI18n("en");
    const area = document.createElement("div");
    area.style.cssText = "position:absolute;inset:0";
    document.body.appendChild(area);
    host = area;

    let commits = 0;
    const handle = createRef<PainterHandle>();
    root = createRoot(area);
    // Deliberately not under StrictMode: it commits everything twice, and this
    // test is counting commits.
    act(() => {
      root!.render(
        <Profiler id="painter" onRender={() => { commits += 1; }}>
          <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
            <PainterLabelContext.Provider value={undefined}>
              <Painter
                ref={handle}
                config={{
                  width: 64,
                  height: 48,
                  mode: { kind: "standard" },
                  controls: { kind: "toolbox" },
                  recordReplay: false,
                  synchronization: { actorId: "1", onOperation: () => {} },
                }}
              />
            </PainterLabelContext.Provider>
          </I18nProvider>
        </Profiler>,
      );
    });
    await act(async () => handle.current!.ready);

    // One remote mark first, on its own. The participant it belongs to is new,
    // and a pair coming into existence is a genuine change to what the toolbox
    // lists -- it is the *repetition* that has to be free, not the first one.
    await act(async () => {
      await handle.current!.applyCanonicalOperation({
        id: "warm", actorId: "2", sequence: 1, operation: remoteDot(4, 4),
      });
    });
    // And the windows settled. A `ResizeObserver` reports once on the first
    // frame after it starts observing, and in this small page that report
    // moves the toolbox window up to fit -- a real change, made once, but on
    // whichever frame comes next. The bursts below take a millisecond or two
    // and yield nothing to a frame, so it landed in the first or the second
    // depending on when the frame fell, and failed this about one run in
    // seven for something no message caused.
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    });

    const marks = 40;
    let sequence = 2;
    const burst = async () => {
      const before = commits;
      await act(async () => {
        for (let i = 0; i < marks; i++) {
          await handle.current!.applyCanonicalOperation({
            // An undo point of their own between marks, as a real stream has:
            // it bounds *their* strokes, and says nothing about ours.
            id: `boundary-${sequence}`, actorId: "2", sequence: sequence++,
            operation: { kind: "undo-boundary" },
          });
          await handle.current!.applyCanonicalOperation({
            id: `mark-${sequence}`, actorId: "2", sequence: sequence++,
            operation: remoteDot(8 + (i % 40), 20),
          });
        }
      });
      return commits - before;
    };

    // Two identical bursts. Whatever the first one settles -- a participant
    // appearing in the toolbox, an opening zoom -- it settles once, and the
    // second burst is the steady state a session spends its life in. Eighty
    // more messages that change nothing this component shows must be eighty
    // canvas writes and no render at all.
    const first = await burst();
    const second = await burst();
    expect(second).toBe(0);
    // And the first is a fixed cost, not a per-message one.
    expect(first).toBeLessThanOrEqual(2);
  });

  it("still re-renders when the local user's own undo becomes available", async () => {
    setupI18n("en");
    const area = document.createElement("div");
    area.style.cssText = "position:absolute;inset:0";
    document.body.appendChild(area);
    host = area;

    let commits = 0;
    const handle = createRef<PainterHandle>();
    root = createRoot(area);
    act(() => {
      root!.render(
        <Profiler id="painter" onRender={() => { commits += 1; }}>
          <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
            <PainterLabelContext.Provider value={undefined}>
              <Painter
                ref={handle}
                config={{
                  width: 64,
                  height: 48,
                  mode: { kind: "standard" },
                  controls: { kind: "toolbox" },
                  recordReplay: false,
                  synchronization: { actorId: "1", onOperation: () => {} },
                }}
              />
            </PainterLabelContext.Provider>
          </I18nProvider>
        </Profiler>,
      );
    });
    await act(async () => handle.current!.ready);
    handle.current!.setLocalActorId("1");

    const before = commits;
    // Our own undo point, echoed back by the server: the undo button has to
    // come alive, which is the one thing this path must still do.
    await act(async () => {
      await handle.current!.applyCanonicalOperation({
        id: "ours", actorId: "1", sequence: 1, operation: { kind: "undo-boundary" },
      });
      await handle.current!.applyCanonicalOperation({
        id: "ours-mark", actorId: "1", sequence: 2, operation: remoteDot(9, 9),
      });
    });

    expect(commits).toBeGreaterThan(before);
  });
});
