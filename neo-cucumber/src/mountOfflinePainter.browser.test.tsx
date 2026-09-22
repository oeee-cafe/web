import { describe, expect, it } from "vitest";
import { act } from "react";
import { mount } from "./public";
import { participantZIndex } from "./neo/canvasStack";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Pixels the drawing itself holds, read from the exported PNG.
 *
 * Counting the mounted canvases instead would count the cursor and the
 * preview overlays with them, and those do not follow undo.
 */
const inkPixels = async (png: Blob, width: number, height: number) => {
  const bitmap = await createImageBitmap(png);
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext("2d")!;
  context.drawImage(bitmap, 0, 0);
  const data = context.getImageData(0, 0, width, height).data;
  let ink = 0;
  for (let i = 0; i < data.length; i += 4) {
    if (data[i] < 200 || data[i + 1] < 200 || data[i + 2] < 200) ink += 1;
  }
  return ink;
};

describe("public painter lifecycle", () => {
  it("rejects invalid dimensions with a stable error code", () => {
    const element = document.createElement("div");
    expect(() =>
      mount(element, {
        width: 0,
        height: 100,
        mode: { kind: "standard" },
        controls: { kind: "none" },
      }),
    ).toThrow(expect.objectContaining({ code: "invalid-options" }));
  });

  it("accepts a 1024×768 canvas", async () => {
    const element = document.createElement("div");
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 1024,
        height: 768,
        mode: { kind: "standard" },
        controls: { kind: "none" },
      });
    });
    await act(async () => painter.ready);
    act(() => painter.unmount());
  });

  it("exports artifacts through a framework-neutral handle", async () => {
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 100,
        height: 100,
        mode: { kind: "standard" },
        controls: { kind: "none" },
      });
    });
    await act(async () => {
      await painter.ready;
    });
    expect(element.querySelector("canvas")).not.toBeNull();
    expect(element.textContent).not.toContain("Save Drawing");

    act(() => {
      painter.setInteractionEnabled(false);
      painter.undo();
      painter.redo();
      painter.setInteractionEnabled(true);
    });

    let png!: Blob;
    let replay!: Blob;
    let saved!: Awaited<ReturnType<typeof painter.save>>;
    await act(async () => {
      png = await painter.exportPng();
      replay = await painter.exportReplay();
      saved = await painter.save();
    });

    expect(png.type).toBe("image/png");
    expect(new TextDecoder().decode(await replay.slice(0, 4).arrayBuffer())).toBe("NEO ");
    expect(saved.width).toBe(100);
    expect(saved.height).toBe(100);
    expect(saved.png.type).toBe("image/png");
    expect(saved.replay.size).toBeGreaterThan(12);

    act(() => {
      painter.unmount();
      painter.unmount();
    });
    expect(() => painter.undo()).toThrow(expect.objectContaining({ code: "unmounted" }));
    await expect(painter.exportPng()).rejects.toMatchObject({ code: "unmounted" });
  });

  it("loads a continuation image through the public handle", async () => {
    const source = document.createElement("canvas");
    source.width = 100;
    source.height = 100;
    const context = source.getContext("2d")!;
    context.fillStyle = "#317842";
    context.fillRect(0, 0, 100, 100);
    const blob = await new Promise<Blob>((resolve) => source.toBlob((value) => resolve(value!)));

    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 100,
        height: 100,
        mode: { kind: "standard" },
        controls: { kind: "none" },
      });
    });
    await act(async () => {
      await painter.ready;
    });
    let replay!: Blob;
    await act(async () => {
      await painter.loadImage(blob);
      replay = await painter.exportReplay();
    });
    expect(replay.size).toBeGreaterThan(12);
    act(() => painter.unmount());
  });

  /**
   * Undo in a painter with no session reverts the canvas.
   *
   * The drawing hook decides which of the two histories owns undo by whether
   * it was given somewhere to send operations. A painter mounted without a
   * `synchronization` has no canonical stream and must fall back to its own
   * snapshot stack; when it was handed a sink that silently dropped
   * everything, undo sent a message nobody received and the stroke stayed on
   * the canvas.
   */
  it("undoes a stroke when there is no session to send undo to", async () => {
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 64,
        height: 48,
        mode: { kind: "standard" },
        controls: { kind: "none" },
      });
    });
    await act(async () => painter.ready);

    const canvas = element.querySelector("#canvas") as HTMLCanvasElement;
    // Where the canvas actually is on the page, since the painter lays itself
    // out and a press at a fixed client point can land beside it.
    const box = canvas.getBoundingClientRect();
    const send = async (type: string, x: number, y: number) => {
      await act(async () => {
        canvas.dispatchEvent(new PointerEvent(type, {
          pointerId: 1,
          pointerType: "mouse",
          button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: box.left + x,
          clientY: box.top + y,
          bubbles: true,
        }));
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
    };
    await send("pointerdown", 10, 12);
    await send("pointermove", 40, 30);
    await send("pointerup", 40, 30);

    let drawn = 0;
    await act(async () => {
      drawn = await inkPixels(await painter.exportPng(), 64, 48);
    });
    expect(drawn).toBeGreaterThan(0);

    let afterUndo = -1;
    await act(async () => {
      painter.undo();
      afterUndo = await inkPixels(await painter.exportPng(), 64, 48);
    });
    expect(afterUndo).toBe(0);

    let afterRedo = -1;
    await act(async () => {
      painter.redo();
      afterRedo = await inkPixels(await painter.exportPng(), 64, 48);
    });
    expect(afterRedo).toBe(drawn);

    act(() => painter.unmount());
  });

  it("emits framework-neutral operations for controlled hosts", async () => {
    const operations: import("./operations").LocalPainterOperation[] = [];
    let pointerUps = 0;
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 100,
        height: 100,
        mode: { kind: "standard" },
        controls: { kind: "none" },
        synchronization: {
          actorId: "alice",
          onOperation: (operation) => operations.push(operation),
          onPointerUp: () => { pointerUps += 1; },
        },
      });
    });
    await act(async () => painter.ready);

    const canvas = element.querySelector("#canvas") as HTMLCanvasElement;
    const send = async (type: string, x: number, y: number) => {
      await act(async () => {
        canvas.dispatchEvent(new PointerEvent(type, {
          pointerId: 1,
          pointerType: "mouse",
          button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: x,
          clientY: y,
          bubbles: true,
        }));
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
    };
    await send("pointerdown", 10, 12);
    await send("pointermove", 30, 32);
    expect(pointerUps).toBe(0);
    await send("pointerup", 30, 32);

    // A gesture's segments are gathered and sent as one stroke rather than one
    // message apiece; the release flushes whatever the chunk still holds. What
    // the host must be able to rely on is that everything drawn is emitted by
    // the time the pointer is up, and that the points are all there.
    expect(operations.map((entry) => entry.operation.kind)).toEqual([
      "undo-boundary",
      "stroke",
    ]);
    expect(pointerUps).toBe(1);
    expect(operations[0].actorId).toBe("alice");
    expect(operations[0].id).not.toBe(operations[1].id);
    expect(operations[1].operation).toMatchObject({
      kind: "stroke",
      layer: "background",
    });
    const stroke = operations[1].operation as Extract<
      import("./operations").PainterOperation,
      { kind: "stroke" }
    >;
    // Both the press and the move it was dragged to are in the one message.
    expect(stroke.points.length).toBeGreaterThanOrEqual(2);
    expect(stroke.points[0]).not.toEqual(stroke.points[stroke.points.length - 1]);
    act(() => painter.unmount());
  });

  it("stamps local operations with the identity the server assigned", async () => {
    // The mount-time actorId is a placeholder: a collaborative host only
    // learns which actor the canonical stream will call it once the server
    // says so. If the fork kept the placeholder, one person would hold two
    // identities -- and stroke continuation, which is keyed by actor, would
    // join an unrelated stroke to the next one.
    const operations: import("./operations").LocalPainterOperation[] = [];
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 100,
        height: 100,
        mode: { kind: "standard" },
        controls: { kind: "none" },
        synchronization: {
          actorId: "placeholder-uuid",
          onOperation: (operation) => operations.push(operation),
        },
      });
    });
    await act(async () => painter.ready);
    act(() => painter.setLocalActorId("7"));

    const canvas = element.querySelector("#canvas") as HTMLCanvasElement;
    const gesture = [
      ["pointerdown", 10, 12],
      ["pointermove", 30, 32],
      ["pointerup", 30, 32],
    ] as const;
    for (const [type, x, y] of gesture) {
      await act(async () => {
        canvas.dispatchEvent(new PointerEvent(type, {
          pointerId: 1, pointerType: "mouse", button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: x, clientY: y, bubbles: true,
        }));
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
    }

    expect(operations.length).toBeGreaterThan(0);
    expect(operations.every((entry) => entry.actorId === "7")).toBe(true);
    expect(operations.every((entry) => entry.id.startsWith("7:"))).toBe(true);

    expect(() => painter.setLocalActorId("")).toThrow(/non-empty/);
    act(() => painter.unmount());
  });

  it("puts every applied region on the visible canvas", async () => {
    // Repaints upload only the region an operation drew, so what reaches the
    // screen is assembled from many partial uploads. A region that is too
    // small, or landed at the wrong offset, leaves the canvas showing
    // something the layer buffer does not say -- and no export would notice,
    // because exports read the buffers rather than these canvases.
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 64, height: 48,
        mode: { kind: "standard" },
        controls: { kind: "none" },
        synchronization: { actorId: "1", onOperation: () => {} },
      });
    });
    await act(async () => painter.ready);

    // Every participant has their own pair, so a canvas is found by whose it
    // is and which layer -- not by a z-index this test would have to keep in
    // step with the stacking rule.
    const layerCanvas = (rank: number, layer: "background" | "foreground") =>
      Array.from(element.querySelectorAll("canvas")).find(
        (canvas) => canvas.style.zIndex === String(participantZIndex(rank, layer)),
      )!;
    const frame = () =>
      act(async () => {
        await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
        await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
      });
    const pixel = (canvas: HTMLCanvasElement, x: number, y: number) =>
      Array.from(canvas.getContext("2d")!.getImageData(x, y, 1, 1).data);

    const dot = (at: { x: number; y: number }, color: { r: number; g: number; b: number }) => ({
      kind: "stroke" as const, layer: "foreground" as const, brushSize: 1,
      brush: "solid" as const, color: { ...color, a: 255 },
      points: [at], mask: { type: 0, r: 0, g: 0, b: 0 },
    });

    await act(async () => {
      await painter.applyCanonicalOperation({
        id: "b1", actorId: "2", sequence: 1, operation: { kind: "undo-boundary" },
      });
      await painter.applyCanonicalOperation({
        id: "s1", actorId: "2", sequence: 2,
        operation: dot({ x: 8, y: 8 }, { r: 255, g: 0, b: 0 }),
      });
    });
    await frame();

    const foreground = layerCanvas(1, "foreground");
    expect(pixel(foreground, 8, 8)).toEqual([255, 0, 0, 255]);
    expect(pixel(foreground, 50, 40)).toEqual([0, 0, 0, 0]);

    // A second mark far away: its own region must reach the canvas without
    // taking the first one off it.
    await act(async () => {
      await painter.applyCanonicalOperation({
        id: "b2", actorId: "2", sequence: 3, operation: { kind: "undo-boundary" },
      });
      await painter.applyCanonicalOperation({
        id: "s2", actorId: "2", sequence: 4,
        operation: dot({ x: 50, y: 40 }, { r: 0, g: 0, b: 255 }),
      });
    });
    await frame();

    expect(pixel(foreground, 50, 40)).toEqual([0, 0, 255, 255]);
    expect(pixel(foreground, 8, 8)).toEqual([255, 0, 0, 255]);

    // A fill covers the whole layer, which no drawn region describes
    await act(async () => {
      await painter.applyCanonicalOperation({
        id: "b3", actorId: "2", sequence: 5, operation: { kind: "undo-boundary" },
      });
      await painter.applyCanonicalOperation({
        id: "f1", actorId: "2", sequence: 6,
        operation: {
          kind: "fill", layer: "background", at: { x: 1, y: 1 },
          color: { r: 0, g: 255, b: 0, a: 255 }, mask: { type: 0, r: 0, g: 0, b: 0 },
        },
      });
    });
    await frame();

    const background = layerCanvas(1, "background");
    expect(pixel(background, 1, 1)).toEqual([0, 255, 0, 255]);
    expect(pixel(background, 63, 47)).toEqual([0, 255, 0, 255]);

    act(() => painter.unmount());
  });

  it("lists a participant who has joined but not drawn", async () => {
    // Layers come into being when an operation first names their owner, so a
    // toolbox built from those alone would leave out exactly the person you
    // most want to see arrive. The host's roster is what fills that gap.
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 32, height: 24,
        mode: { kind: "standard" },
        controls: { kind: "toolbox" },
        synchronization: { actorId: "1", onOperation: () => {} },
      });
    });
    await act(async () => painter.ready);

    const rows = () =>
      Array.from(document.querySelectorAll("button"))
        .map((button) => button.getAttribute("aria-label") ?? "")
        .filter((label) => label.startsWith("Draw on "));

    // Nobody else yet: one participant is not worth a panel.
    expect(rows()).toEqual([]);

    act(() => {
      painter.setParticipants([
        { actorId: "1", name: "alice" },
        { actorId: "2", name: "bob" },
      ]);
    });

    // Bob has drawn nothing, and is listed regardless.
    expect(rows()).toEqual([
      "Draw on alice's layers",
      "Draw on bob's layers",
    ]);

    act(() => painter.unmount());
  });

  it("applies canonical operations and round-trips public checkpoints", async () => {
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 24,
        height: 16,
        mode: { kind: "standard" },
        controls: { kind: "none" },
        synchronization: { actorId: "alice", onOperation: () => {} },
      });
    });
    await act(async () => painter.ready);

    await act(async () => {
      await painter.applyCanonicalOperation({
        id: "bob:1",
        actorId: "bob",
        sequence: 1,
        operation: {
          kind: "fill",
          layer: "background",
          at: { x: 2, y: 2 },
          color: { r: 49, g: 120, b: 66, a: 255 },
          mask: { type: 0, r: 0, g: 0, b: 0 },
        },
      });
    });
    const checkpoint = await painter.exportCheckpoint(1);
    expect(checkpoint).toMatchObject({ sequence: 1, width: 24, height: 16 });
    // The local participant and the one whose operation was applied, each
    // with their own pair.
    expect(checkpoint.layers.map((entry) => entry.actorId).sort()).toEqual([
      "alice",
      "bob",
    ]);
    for (const entry of checkpoint.layers) {
      expect(entry.background.type).toBe("image/png");
      expect(entry.foreground.type).toBe("image/png");
    }
    const archiveBeforeReset = await painter.exportSessionArchive();
    expect(archiveBeforeReset.operations.map((entry) => entry.id)).toEqual(["bob:1"]);
    await painter.compactCanonicalHistory(1);
    const compactedArchive = await painter.exportSessionArchive();
    expect(compactedArchive.operations).toEqual([]);
    expect(compactedArchive.checkpoint).toMatchObject({ sequence: 1, width: 24, height: 16 });
    expect(painter.isSynchronizationSettled()).toBe(true);

    await act(async () => {
      await painter.applyCanonicalOperation({
        id: "bob:2",
        actorId: "bob",
        sequence: 2,
        operation: { kind: "clear-layer", layer: "background" },
      });
      await painter.applyCheckpoint(checkpoint);
    });
    const archive = await painter.exportSessionArchive();
    expect(archive).toMatchObject({
      format: "neo-cucumber-session",
      version: 1,
      checkpoint: { sequence: 1 },
      operations: [],
    });

    const png = await painter.exportPng();
    const bitmap = await createImageBitmap(png);
    const sample = document.createElement("canvas");
    sample.width = bitmap.width;
    sample.height = bitmap.height;
    const context = sample.getContext("2d")!;
    context.drawImage(bitmap, 0, 0);
    expect([...context.getImageData(2, 2, 1, 1).data]).toEqual([49, 120, 66, 255]);
    bitmap.close();
    act(() => painter.unmount());
  });
});

it("records no replay when the host says not to", async () => {
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 32, height: 24,
      mode: { kind: "standard" },
      controls: { kind: "none" },
      recordReplay: false,
      synchronization: { actorId: "1", onOperation: () => {} },
    });
  });
  await act(async () => painter.ready);

  // Refused rather than answered with an empty file that looks like a drawing.
  await expect(painter.exportReplay()).rejects.toMatchObject({
    code: "export-failed",
  });
  // The image is still there: that is what a session saves.
  const png = await painter.exportPng();
  expect(png.type).toBe("image/png");

  act(() => painter.unmount());
});

it("still records a replay by default", async () => {
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 32, height: 24,
      mode: { kind: "standard" },
      controls: { kind: "none" },
    });
  });
  await act(async () => painter.ready);
  const replay = await painter.exportReplay();
  expect(new TextDecoder().decode(await replay.slice(0, 4).arrayBuffer())).toBe("NEO ");
  act(() => painter.unmount());
});

it("switches to the eraser and back, and to the tool before, on a pen's command", async () => {
  const element = document.createElement("div");
  document.body.appendChild(element);
  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: 64,
      height: 64,
      mode: { kind: "standard" },
      controls: { kind: "toolbox" },
    });
  });
  await act(async () => painter.ready);
  // The tool whose toolbox button is pressed, by its name: a button's title
  // starts with the tool's name and goes on to say what clicking again does.
  // NEO calls its eraser "White" (消しペン).
  const tool = () =>
    Array.from(element.querySelectorAll<HTMLButtonElement>('button[aria-pressed="true"]'))
      .map((button) => button.title.split(" — ")[0])
      .find((name) => !name.startsWith("Palette"));

  const first = tool();
  expect(first).not.toBe("White");

  act(() => painter.command("toggle-eraser"));
  expect(tool()).toBe("White");

  act(() => painter.command("toggle-eraser"));
  expect(tool()).toBe(first);

  // Back to the eraser, then "previous" returns to the tool before it, and
  // "previous" again to the eraser: a pen's switch goes both ways.
  act(() => painter.command("toggle-eraser"));
  act(() => painter.command("previous-tool"));
  expect(tool()).toBe(first);
  act(() => painter.command("previous-tool"));
  expect(tool()).toBe("White");

  act(() => painter.unmount());
  element.remove();
});
