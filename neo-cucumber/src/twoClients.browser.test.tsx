import { describe, expect, it } from "vitest";
import { act } from "react";
import { mount } from "./public";
import { DrawingEngine } from "./DrawingEngine";
import { deflateCoverage } from "./utils/rasterCodec";
import type { LocalPainterOperation, PainterOperation } from "./operations";
import {
  decodeMessage,
  decodePainterOperation,
  encodePainterOperation,
  isCanvasHistoryMessage,
} from "../../frontend/collaborate/binaryProtocol";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const WIDTH = 64;
const HEIGHT = 48;

/**
 * Two painters and one wire between them.
 *
 * Every bug this feature has had lived in a seam rather than in a piece: an
 * encoder that threw for one kind, a client that routed every kind but one, a
 * repaint that named the wrong participant. Each piece had tests; nothing ran
 * a mark from the hand that made it to the screen that had to show it.
 *
 * So this relays what a painter emits through the real encoder, the real
 * routing predicate and the real decoder, and gives it to both of them the way
 * a server would: the author included, since a client applies its own work
 * only when it comes back sequenced.
 */
function twoClients() {
  const relayed: string[] = [];
  const sentOperations: PainterOperation[] = [];
  let sequence = 0;
  const clients = new Map<string, ReturnType<typeof mount>>();

  const relay = (from: string, entry: LocalPainterOperation) => {
    const bytes = encodePainterOperation(Number(from), entry.operation);
    const message = decodeMessage(bytes);
    if (!message) throw new Error(`${entry.operation.kind} did not decode`);
    if (!isCanvasHistoryMessage(message)) {
      throw new Error(`${entry.operation.kind} would never reach the canvas`);
    }
    const operation = decodePainterOperation(message);
    if (!operation) throw new Error(`${entry.operation.kind} lost its meaning`);
    sequence += 1;
    relayed.push(operation.kind);
    sentOperations.push(operation);
    const at = sequence;
    return Promise.all(
      [...clients].map(([id, painter]) =>
        painter.applyCanonicalOperation({
          // The author recognises its own work by the id it gave it.
          id: id === from ? entry.id : `${from}:${at}`,
          actorId: from,
          sequence: at,
          operation,
        }),
      ),
    );
  };

  const pending: Promise<unknown>[] = [];
  const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
  const open = (id: string, controls: "none" | "toolbox" = "none") => {
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: WIDTH, height: HEIGHT,
        mode: { kind: "standard" },
        controls: { kind: controls },
        recordReplay: false,
        synchronization: {
          actorId: id,
          onOperation: (entry) => pending.push(relay(id, entry)),
        },
      });
    });
    clients.set(id, painter);
    return { painter, element };
  };

  /** Opens a painter, waits for it, and names it on the stream. */
  const join = async (id: string, controls: "none" | "toolbox" = "none") => {
    const client = open(id, controls);
    await act(async () => {
      await client.painter.ready;
    });
    act(() => {
      client.painter.setLocalActorId(id);
    });
    return client;
  };
  /** Lets every relayed operation land, and the painters draw it. */
  const rest = (ms = 200) =>
    act(async () => {
      await Promise.all(pending);
      await sleep(ms);
    });
  /** Ctrl+Z on the window, and time for the room to answer it. */
  const pressUndo = async () => {
    await act(async () => {
      window.dispatchEvent(new KeyboardEvent("keydown", {
        key: "z", ctrlKey: true, bubbles: true, cancelable: true,
      }));
    });
    await rest(180);
  };
  const undosSent = () => relayed.filter((kind) => kind === "undo").length;

  return {
    open,
    join,
    rest,
    pressUndo,
    undosSent,
    relayed,
    sentOperations,
    /** Sends an operation the way a painter's own would go. */
    send: (from: string, operation: LocalPainterOperation["operation"]) =>
      relay(from, { id: `${from}:sent:${sequence + 1}`, actorId: from, operation }),
    settle: () => Promise.all(pending),
  };
}

/**
 * What is actually on the screen.
 *
 * Composited from the mounted canvases rather than exported from the buffers.
 * The two are not the same question, and the difference is a whole class of
 * bug: pixels can be correct in the layer and never repainted onto the canvas
 * showing it, which an export would call perfectly healthy.
 */
function onScreen(element: HTMLElement) {
  const canvas = document.createElement("canvas");
  canvas.width = WIDTH;
  canvas.height = HEIGHT;
  const context = canvas.getContext("2d")!;
  context.fillStyle = "white";
  context.fillRect(0, 0, WIDTH, HEIGHT);

  const layers = Array.from(element.querySelectorAll("canvas"))
    .filter((c) => {
      const z = Number(c.style.zIndex);
      // The participants' layers: not the interaction canvas below them and
      // not the cursor and preview overlays above.
      return Number.isFinite(z) && z > 0 && z < 10000 && c.style.display !== "none";
    })
    .sort((a, b) => Number(a.style.zIndex) - Number(b.style.zIndex));
  for (const layer of layers) context.drawImage(layer, 0, 0);

  return Array.from(context.getImageData(0, 0, WIDTH, HEIGHT).data);
}

/** A pointer on a painter's canvas, placed in the canvas's own screen box. */
function pointerOn(element: HTMLElement) {
  const canvas = element.querySelector("#canvas") as HTMLCanvasElement;
  const box = canvas.getBoundingClientRect();
  const dispatch = (type: string, x: number, y: number, buttons?: number) =>
    act(async () => {
      canvas.dispatchEvent(new PointerEvent(type, {
        pointerId: 1, pointerType: "mouse", button: 0,
        buttons: buttons ?? (type === "pointerup" ? 0 : 1),
        clientX: box.left + x, clientY: box.top + y,
        bubbles: true, cancelable: true,
      }));
    });
  return { box, dispatch };
}

/** A short horizontal mark in one participant's background. */
const stroke = (target: string, x: number, y: number, blue: number) => ({
  kind: "stroke" as const,
  layer: "background" as const,
  targetActorId: target,
  brushSize: 2,
  brush: "solid" as const,
  color: { r: 0, g: 0, b: blue, a: 255 },
  points: [{ x, y }, { x: x + 6, y }],
  mask: { type: 0, r: 0, g: 0, b: 0 },
});

/** Pixels with any ink on the canvas the stack shows at this z-index. */
function inkedAt(element: HTMLElement, zIndex: number): number {
  const layer = Array.from(element.querySelectorAll("canvas"))
    .find((c) => Number(c.style.zIndex) === zIndex);
  if (!layer) throw new Error(`no canvas at z ${zIndex}`);
  const data = layer.getContext("2d")!.getImageData(0, 0, WIDTH, HEIGHT).data;
  let count = 0;
  for (let i = 3; i < data.length; i += 4) if (data[i] > 0) count++;
  return count;
}

describe("two clients and the wire between them", () => {
  /**
   * A stroke used to put one message on the wire per pointer sample, so a
   * gesture of a few seconds became hundreds of canonical messages -- and
   * every one of them is a sequence number, a history entry and a fork entry
   * for the whole room. Drawpile packs thousands of dabs into a message
   * instead.
   *
   * What must survive the packing is the join: consecutive stroke messages
   * meet at a shared point, and a chunk that dropped its last point would
   * leave a gap in the line for everyone but its author.
   */
  it("sends a long stroke as a few messages, and the line still joins", async () => {
    const room = twoClients();
    const alice = room.open("1");
    const bob = room.open("2");
    await act(async () => {
      await alice.painter.ready;
      await bob.painter.ready;
    });
    act(() => {
      alice.painter.setLocalActorId("1");
      bob.painter.setLocalActorId("2");
    });

    const canvas = alice.element.querySelector("#canvas") as HTMLCanvasElement;
    const box = canvas.getBoundingClientRect();
    await act(async () => {
      window.dispatchEvent(new Event("resize"));
      await new Promise((resolve) => setTimeout(resolve, 30));
    });

    const at = (type: string, x: number, y: number) =>
      canvas.dispatchEvent(
        new PointerEvent(type, {
          pointerId: 1, pointerType: "mouse", button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: box.left + x, clientY: box.top + y,
          bubbles: true,
        }),
      );

    const SAMPLES = 40;
    await act(async () => {
      at("pointerdown", 4, 4);
      for (let i = 1; i <= SAMPLES; i++) at("pointermove", 4 + i, 4 + i);
      at("pointerup", 4 + SAMPLES, 4 + SAMPLES);
      await room.settle();
      await new Promise((resolve) => setTimeout(resolve, 120));
    });

    const strokes = room.relayed.filter((kind) => kind === "stroke").length;
    expect(strokes).toBeGreaterThan(0);
    expect(strokes).toBeLessThan(SAMPLES / 2);

    // And the whole line reached him, joins and all.
    expect(onScreen(bob.element)).toEqual(onScreen(alice.element));

    act(() => {
      alice.painter.unmount();
      bob.painter.unmount();
    });
  });

  it("agree on a stroke one of them made", async () => {
    const room = twoClients();
    const alice = room.open("1");
    const bob = room.open("2");
    await act(async () => {
      await alice.painter.ready;
      await bob.painter.ready;
    });
    act(() => {
      alice.painter.setLocalActorId("1");
      bob.painter.setLocalActorId("2");
    });

    // Alice fills her own background, through the pointer, as a person would.
    const canvas = alice.element.querySelector("#canvas") as HTMLCanvasElement;
    const box = canvas.getBoundingClientRect();
    await act(async () => {
      window.dispatchEvent(new Event("resize"));
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    await act(async () => {
      await alice.painter.applyCanonicalOperation({
        id: "seed:1", actorId: "1", sequence: 1000,
        operation: { kind: "undo-boundary" },
      });
    });

    // A fill emitted by the painter itself, so the whole send path runs.
    await act(async () => {
      for (const type of ["pointerdown", "pointerup"]) {
        canvas.dispatchEvent(new PointerEvent(type, {
          pointerId: 1, pointerType: "mouse", button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: box.left + box.width / 2,
          clientY: box.top + box.height / 2,
          bubbles: true,
        }));
        await new Promise((resolve) => setTimeout(resolve, 40));
      }
      await room.settle();
      await new Promise((resolve) => setTimeout(resolve, 120));
    });

    expect(room.relayed).toContain("stroke");
    // Whatever she drew, he has too.
    expect(onScreen(bob.element)).toEqual(onScreen(alice.element));

    act(() => {
      alice.painter.unmount();
      bob.painter.unmount();
    });
  });

  it("agree on a fill, which travels as coverage rather than as a seed", async () => {
    const room = twoClients();
    const alice = room.open("1");
    const bob = room.open("2");
    await act(async () => {
      await alice.painter.ready;
      await bob.painter.ready;
    });
    act(() => {
      alice.painter.setLocalActorId("1");
      bob.painter.setLocalActorId("2");
    });

    // A real flood, computed by a real engine, sent the way the painter sends
    // one: this is the path where the encoder threw for a day, and where the
    // client routed every kind but this one.
    const scratch = new DrawingEngine(WIDTH, HEIGHT);
    scratch.setLocalOwner("1");
    const region = scratch.floodFillCapturingRegion(
      scratch.layersFor("1").background, 32, 24, 200, 100, 50, 255,
    )!;

    await act(async () => {
      await room.send("1", { kind: "undo-boundary" });
      await room.send("1", {
        kind: "fill-region",
        layer: "background",
        targetActorId: "1",
        at: { x: region.x, y: region.y },
        width: region.width,
        height: region.height,
        color: { r: 200, g: 100, b: 50, a: 255 },
        coverage: await deflateCoverage(region.coverage),
        mask: { type: 0, r: 0, g: 0, b: 0 },
      });
      await room.settle();
      await new Promise((resolve) => setTimeout(resolve, 150));
    });

    expect(room.relayed).toContain("fill-region");
    const painted = onScreen(bob.element);
    // It is on his screen, and it is the same drawing as hers.
    expect(painted).toEqual(onScreen(alice.element));
    const centre = ((HEIGHT / 2) * WIDTH + WIDTH / 2) * 4;
    expect(painted.slice(centre, centre + 3)).toEqual([200, 100, 50]);

    act(() => {
      alice.painter.unmount();
      bob.painter.unmount();
    });
  });

  it("answers one Ctrl+Z with one undo", async () => {
    // Two listeners used to answer this key -- the shortcut table's and one
    // the painter bound on its own -- and both read the same render's
    // `canUndo`, so a single press sent the room two undo operations and
    // took back two strokes.
    const room = twoClients();
    await room.join("2");
    await act(async () => {
      await room.send("2", { kind: "undo-boundary" });
      await room.send("2", stroke("2", 8, 30, 128));
    });
    await room.rest(120);

    await room.pressUndo();
    expect(room.undosSent()).toBe(1);
  });

  it("finishes the stroke under the pen when the host disables drawing", async () => {
    // A dropped socket or a replay disables drawing, and it can do so
    // mid-stroke. The release that follows is ignored while disabled, so the
    // gesture stayed open: nothing under the pen was handed over, the painter
    // reported itself unsettled, and once re-enabled a plain hover kept
    // drawing with no button down.
    const room = twoClients();
    const bob = await room.join("2");
    const pointer = pointerOn(bob.element).dispatch;

    await pointer("pointerdown", 8, 8);
    await pointer("pointermove", 20, 8);
    act(() => { bob.painter.setInteractionEnabled(false); });
    await room.rest();
    expect(room.relayed).toContain("stroke");
    expect(bob.painter.isSynchronizationSettled()).toBe(true);

    const sent = room.relayed.length;
    act(() => { bob.painter.setInteractionEnabled(true); });
    await pointer("pointermove", 30, 8, 0);
    await room.rest();
    expect(room.relayed.length).toBe(sent);
  });

  it("puts text in the pair the painter is aimed at", async () => {
    // The operation sent for the text named the selected participant's
    // pair; the text itself was rasterised into our own. Everyone else saw
    // it in one place and the author in another.
    const room = twoClients();
    const bob = await room.join("2", "toolbox");
    act(() => {
      bob.painter.setParticipants([
        { actorId: "1", name: "Alice" },
        { actorId: "2", name: "Bob" },
      ]);
    });
    await room.rest(50);

    const row = bob.element.querySelector<HTMLButtonElement>(
      'button[aria-label="Draw on Alice\'s layers"]',
    );
    if (!row) throw new Error("no layers row for Alice");
    await act(async () => { row.click(); });
    // The shortcut table's T, rather than a tip in the toolbox: the pen tip
    // cycles through its tools on each press, and its title is a phrase.
    await act(async () => {
      window.dispatchEvent(new KeyboardEvent("keydown", {
        key: "t", bubbles: true, cancelable: true,
      }));
    });

    // A toolbox mount opens at a fitted zoom, so the press is placed by the
    // canvas's on-screen size rather than in artwork pixels.
    const { box, dispatch: pointer } = pointerOn(bob.element);
    await pointer("pointerdown", box.width * 0.1, box.height * 0.5);
    await pointer("pointerup", box.width * 0.1, box.height * 0.5);
    const editor = bob.element.querySelector<HTMLDivElement>("[contenteditable]");
    if (!editor) throw new Error("no text editor opened");
    await act(async () => {
      editor.textContent = "Hi";
      editor.dispatchEvent(new KeyboardEvent("keydown", {
        key: "Enter", bubbles: true, cancelable: true,
      }));
    });
    await room.rest();
    expect(room.relayed).toContain("text");

    // Alice joined first, so her pair sits on top: z 9000 and 9001. Bob's is
    // the band below. See participantZIndex.
    expect(inkedAt(bob.element, 9000)).toBeGreaterThan(0);
    expect(inkedAt(bob.element, 8990)).toBe(0);
  });

  it("ignores Ctrl+Z while the host has drawing disabled", async () => {
    // The pointer was already refused then; the shortcut and the toolbox
    // buttons were not, so an undo clicked during a replay or a save went
    // out to the room behind the export it changed.
    const room = twoClients();
    const bob = await room.join("2");
    await act(async () => {
      await room.send("2", { kind: "undo-boundary" });
      await room.send("2", stroke("2", 8, 30, 128));
    });
    await room.rest(120);

    act(() => { bob.painter.setInteractionEnabled(false); });
    await room.pressUndo();
    expect(room.undosSent()).toBe(0);

    act(() => { bob.painter.setInteractionEnabled(true); });
    await room.pressUndo();
    expect(room.undosSent()).toBe(1);
  });

  it("ignores Ctrl+Z while the pen is down", async () => {
    // An undo sent mid-stroke was sequenced ahead of the stroke's own tail,
    // which then went out after it with no boundary of its own, while the
    // pointer kept drawing onto a canvas the replay had just rolled back.
    const room = twoClients();
    const bob = await room.join("2");
    const pointer = pointerOn(bob.element).dispatch;

    // One stroke on the record, so there is something to undo.
    await pointer("pointerdown", 8, 8);
    await pointer("pointermove", 20, 8);
    await pointer("pointerup", 20, 8);
    await room.rest();
    expect(room.relayed).toContain("stroke");

    // A second one, with the key pressed while it is still being drawn.
    await pointer("pointerdown", 8, 24);
    await pointer("pointermove", 20, 24);
    await room.pressUndo();
    expect(room.undosSent()).toBe(0);

    await pointer("pointerup", 20, 24);
    await room.rest();
    await room.pressUndo();
    expect(room.undosSent()).toBe(1);
  });

  it("agree after one of them undoes, which rebuilds the canvas", async () => {
    // Undo is the only thing that replays history, and replaying is where the
    // screen and the buffers come apart: the layers are rewritten for whoever
    // the entries belong to, and the repaint that follows has to name them.
    // Both bugs that did that were invisible to anything not comparing
    // screens after an undo.
    const room = twoClients();
    const alice = room.open("1");
    const bob = room.open("2");
    await act(async () => {
      await alice.painter.ready;
      await bob.painter.ready;
    });
    act(() => {
      alice.painter.setLocalActorId("1");
      bob.painter.setLocalActorId("2");
    });

    await act(async () => {
      // Alice marks her own layers.
      await room.send("1", { kind: "undo-boundary" });
      await room.send("1", stroke("1", 8, 10, 255));
      // Bob marks his.
      await room.send("2", { kind: "undo-boundary" });
      await room.send("2", stroke("2", 8, 30, 128));
      await room.settle();
      await new Promise((resolve) => setTimeout(resolve, 120));
    });
    const together = onScreen(alice.element);
    expect(onScreen(bob.element)).toEqual(together);

    // Bob takes his back. His goes; hers stays; both screens still agree.
    await act(async () => {
      bob.painter.undo();
      await new Promise((resolve) => setTimeout(resolve, 60));
      await room.settle();
      await new Promise((resolve) => setTimeout(resolve, 180));
    });

    expect(room.relayed).toContain("undo");
    const after = onScreen(alice.element);
    expect(onScreen(bob.element)).toEqual(after);
    // Something changed, and it was his mark rather than hers.
    expect(after).not.toEqual(together);
    const hers = ((10 * WIDTH) + 9) * 4;
    expect(after.slice(hers, hers + 3)).toEqual([0, 0, 255]);
    const his = ((30 * WIDTH) + 9) * 4;
    expect(after.slice(his, his + 3)).toEqual([255, 255, 255]);

    act(() => {
      alice.painter.unmount();
      bob.painter.unmount();
    });
  });
});
