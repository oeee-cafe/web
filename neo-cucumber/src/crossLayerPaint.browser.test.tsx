import { expect, it } from "vitest";
import { act } from "react";
import { mount } from "./public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const litPixels = (host: HTMLElement, w: number, h: number) =>
  Array.from(host.querySelectorAll("canvas")).reduce((total, canvas) => {
    const data = canvas.getContext("2d")!.getImageData(0, 0, w, h).data;
    let lit = 0;
    for (let i = 3; i < data.length; i += 4) if (data[i] !== 0) lit += 1;
    return total + lit;
  }, 0);

/**
 * A canonical operation has to reach the screen on its own.
 *
 * Every participant's pair is re-keyed when the server names them, and the
 * repaint that follows a write looks the region up by owner. A write filed
 * under the name the pair had before the rename is a write nothing repaints:
 * the pixels are in the buffer and not on the canvas, and they stay that way
 * until something unrelated forces a full redraw -- another participant
 * arriving, or the local user touching the canvas. That is invisible in a
 * session where you are always drawing, and very visible in one where you are
 * watching, or catching up after a rejoin.
 */
it("paints a canonical stroke aimed at the local participant's own layers", async () => {
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 64, height: 48,
      mode: { kind: "standard" },
      controls: { kind: "none" },
      // Mounted under a placeholder, then named by the server, exactly as a
      // collaborative client is.
      synchronization: { actorId: "placeholder", onOperation: () => {} },
    });
  });
  await act(async () => painter.ready);
  act(() => painter.setLocalActorId("2"));

  expect(litPixels(element, 64, 48)).toBe(0);

  // Somebody else draws on our layers.
  await act(async () => {
    await painter.applyCanonicalOperation({
      id: "a:1", actorId: "1", sequence: 1, operation: { kind: "undo-boundary" },
    });
    await painter.applyCanonicalOperation({
      id: "a:2", actorId: "1", sequence: 2,
      operation: {
        kind: "stroke", targetActorId: "2", layer: "background",
        brushSize: 1, brush: "solid",
        color: { r: 0, g: 0, b: 0, a: 255 },
        points: [{ x: 16, y: 24 }, { x: 48, y: 24 }],
        mask: { type: 0, r: 0, g: 0, b: 0 },
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 150));
  });

  expect(litPixels(element, 64, 48)).toBeGreaterThan(0);
  act(() => painter.unmount());
});

/**
 * A savepoint keeps each participant's layers under their name, and that name
 * changes when the server names them. If the savepoints keep the old name,
 * restoring one puts the old name back into the engine as a participant of its
 * own -- a collaborator called "local" appears in the toolbox, holding
 * whatever those layers held when the savepoint was taken, and the drawing
 * jumps back to it.
 */
it("does not conjure a participant out of the name we used to have", async () => {
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 32, height: 24,
      mode: { kind: "standard" },
      controls: { kind: "toolbox" },
      // Named by the server only once it speaks, as a collaborative client is.
      synchronization: { actorId: "placeholder", onOperation: () => {} },
    });
  });
  await act(async () => painter.ready);
  act(() => painter.setLocalActorId("1"));
  await act(async () => {
    painter.setParticipants([
      { actorId: "1", name: "alice" },
      { actorId: "2", name: "bob" },
    ]);
    await new Promise((resolve) => setTimeout(resolve, 80));
  });

  const fill = (sequence: number, red: number) => ({
    id: `a:${sequence}`, actorId: "1", sequence,
    operation: {
      kind: "fill" as const, layer: "background" as const,
      at: { x: 16, y: 12 }, color: { r: red, g: 0, b: 0, a: 255 },
      mask: { type: 0, r: 0, g: 0, b: 0 },
    },
  });

  await act(async () => {
    await painter.applyCanonicalOperation({ id: "a:1", actorId: "1", sequence: 1, operation: { kind: "undo-boundary" } });
    await painter.applyCanonicalOperation(fill(2, 200));
    await painter.applyCanonicalOperation({ id: "a:3", actorId: "1", sequence: 3, operation: { kind: "undo-boundary" } });
    await painter.applyCanonicalOperation({
      id: "a:4", actorId: "1", sequence: 4,
      operation: { kind: "clear-layer", layer: "background", targetActorId: "1" },
    });
    // Then a mark on somebody else's layers, which is what gets undone.
    await painter.applyCanonicalOperation({ id: "a:5", actorId: "1", sequence: 5, operation: { kind: "undo-boundary" } });
    await painter.applyCanonicalOperation({
      id: "a:6", actorId: "1", sequence: 6,
      operation: {
        kind: "stroke", targetActorId: "2", layer: "foreground",
        brushSize: 1, brush: "solid", color: { r: 0, g: 0, b: 255, a: 255 },
        points: [{ x: 8, y: 8 }], mask: { type: 0, r: 0, g: 0, b: 0 },
      },
    });
    await painter.applyCanonicalOperation({
      id: "a:7", actorId: "1", sequence: 7, operation: { kind: "undo", redo: false },
    });
    await new Promise((resolve) => setTimeout(resolve, 200));
  });

  // No participant we never had.
  const rows = Array.from(document.querySelectorAll("button"))
    .map((b) => b.getAttribute("aria-label") ?? "")
    .filter((label) => label.startsWith("Draw on "));
  expect(rows).toEqual(["Draw on alice's layers", "Draw on bob's layers"]);

  // And the clear survives the undo: it reverted the stroke, not the session.
  // Only the participants' layers: the interaction canvas and the overlays
  // above them are not the drawing.
  const engineCanvases = Array.from(element.querySelectorAll("canvas")).filter(
    (canvas) => {
      const z = Number((canvas as HTMLCanvasElement).style.zIndex);
      return Number.isFinite(z) && z > 0 && z < 10000;
    },
  );
  expect(engineCanvases.length).toBeGreaterThan(0);
  const anyRed = engineCanvases.some((canvas) => {
    const data = canvas.getContext("2d")!.getImageData(0, 0, 32, 24).data;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i] > 150 && data[i + 3] > 0) return true;
    }
    return false;
  });
  expect(anyRed).toBe(false);

  act(() => painter.unmount());
});

/** Session undo previews canonical history without using the offline stack. */
it("previews session undo before the server echoes it", async () => {
  const sent: string[] = [];
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 32, height: 24,
      mode: { kind: "standard" },
      controls: { kind: "none" },
      synchronization: {
        actorId: "1",
        onOperation: (entry) => sent.push(entry.operation.kind),
      },
    });
  });
  await act(async () => painter.ready);
  act(() => painter.setLocalActorId("1"));

  await act(async () => {
    await painter.applyCanonicalOperation({
      id: "s:1", actorId: "1", sequence: 1, operation: { kind: "undo-boundary" },
    });
    await painter.applyCanonicalOperation({
      id: "s:2", actorId: "1", sequence: 2,
      operation: {
        kind: "fill", layer: "background", targetActorId: "1",
        at: { x: 16, y: 12 }, color: { r: 200, g: 0, b: 0, a: 255 },
        mask: { type: 0, r: 0, g: 0, b: 0 },
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 150));
  });
  const before = litPixels(element, 32, 24);
  expect(before).toBeGreaterThan(0);

  // Undo previews the canonical gesture, with nothing echoed back yet.
  act(() => painter.undo());
  await act(async () => new Promise((resolve) => setTimeout(resolve, 150)));

  expect(sent).toContain("undo");
  expect(litPixels(element, 32, 24)).toBe(0);

  act(() => painter.unmount());
});

/**
 * There are two histories and one pair of undo buttons.
 *
 * In a session the canonical stream is in charge; the offline snapshot stack
 * is empty there, so its answer is "nothing to undo" over the top of the real
 * one. Whichever spoke last used to win.
 */
it("takes undo state from the history that is in charge", async () => {
  const seen: string[] = [];
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 32, height: 24,
      mode: { kind: "standard" },
      controls: { kind: "none" },
      synchronization: { actorId: "1", onOperation: () => {} },
      onChange: (state) => seen.push(`${state.canUndo}`),
    });
  });
  await act(async () => painter.ready);
  act(() => painter.setLocalActorId("1"));

  await act(async () => {
    await painter.applyCanonicalOperation({
      id: "c:1", actorId: "1", sequence: 1, operation: { kind: "undo-boundary" },
    });
    await painter.applyCanonicalOperation({
      id: "c:2", actorId: "1", sequence: 2,
      operation: {
        kind: "fill", layer: "background", targetActorId: "1",
        at: { x: 16, y: 12 }, color: { r: 200, g: 0, b: 0, a: 255 },
        mask: { type: 0, r: 0, g: 0, b: 0 },
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 150));
  });

  // The canonical history has something to undo, and nothing has said otherwise.
  expect(seen[seen.length - 1]).toBe("true");
  act(() => painter.unmount());
});

/**
 * Readiness must come from the engine existing, not from something unrelated
 * happening to re-render. The engine lives in a ref, so building it moves
 * nothing on its own; it says so explicitly now.
 */
it("becomes ready without needing an unrelated render", async () => {
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 16, height: 16,
      mode: { kind: "standard" },
      controls: { kind: "none" },
      // A session, where the offline history is not the authority and so says
      // nothing at all -- leaving no other reason for a render to happen.
      synchronization: { actorId: "1", onOperation: () => {} },
    });
  });
  await act(async () => painter.ready);
  expect(element.querySelector("canvas")).not.toBeNull();
  act(() => painter.unmount());
});
