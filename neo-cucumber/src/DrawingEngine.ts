import { LINETYPE, NeoPainter } from "./neo/NeoPainter";
import { BufferSurface } from "./neo/PixelSurface";
import { fillToolTypeFor, type RegionTool } from "./neo/tools";

/** A pair of layer buffers an operation can be pointed at. */
export interface LayerBuffers {
  foreground: Uint8ClampedArray;
  background: Uint8ClampedArray;
}

type LayerName = "foreground" | "background";

/**
 * Who a layer pair belongs to.
 *
 * A collaborative session gives every participant their own NEO: their own
 * background and foreground, drawn on only by them, composited as a unit
 * against everyone else's. Offline there is one participant and this is a
 * detail nobody sees, which is why every owner-taking method defaults to the
 * local one and the offline painter never names an owner at all.
 */
export type LayerOwner = string;

export const DEFAULT_LAYER_OWNER: LayerOwner = "local";

/** The buffers and their standing surfaces for one participant. */
interface OwnerSlot {
  layers: LayerBuffers;
  /** NEO addresses layers by index, background first. */
  surfaces: [BufferSurface, BufferSurface];
}

/** Which participant's which layer a live buffer is. */
interface BufferIdentity {
  owner: LayerOwner;
  layer: LayerName;
}

/** Repaint bookkeeping is per owner per layer, so one key names one canvas. */
function slotKey(owner: LayerOwner, layer: LayerName): string {
  return `${owner} ${layer}`;
}

/** Inclusive pixel bounds of a change, in layer coordinates. */
interface DirtyRegion {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

function unionRegion(into: DirtyRegion | null, x0: number, y0: number, x1: number, y1: number): DirtyRegion {
  if (!into) return { x0, y0, x1, y1 };
  into.x0 = Math.min(into.x0, x0);
  into.y0 = Math.min(into.y0, y0);
  into.x1 = Math.max(into.x1, x1);
  into.y1 = Math.max(into.y1, y1);
  return into;
}

export class DrawingEngine {
  public imageWidth: number;
  public imageHeight: number;
  public layers: { [key: string]: Uint8ClampedArray };
  public canvas: HTMLCanvasElement | null = null;

  // Offscreen canvases for hardware-accelerated compositing
  public layerCanvases: { [key: string]: HTMLCanvasElement } = {};
  public layerContexts: { [key: string]: CanvasRenderingContext2D } = {};

  // DOM canvases for direct rendering
  public domCanvases: { [key: string]: HTMLCanvasElement } = {};
  public domContexts: { [key: string]: CanvasRenderingContext2D } = {};

  // Batched update system. A repaint carries the region it has to cover:
  // `null` is nothing to do, `"all"` is the whole layer, and a rectangle is
  // the union of what has been drawn since the last one. Uploading the whole
  // canvas for a stroke costs far more than drawing the stroke did.
  private pendingUpdates = new Map<string, DirtyRegion | "all">();
  /** What the owned surfaces have written since the last repaint was queued. */
  private writtenRegions = new Map<string, DirtyRegion>();
  /** Reused staging buffer for partial uploads, grown to the largest region. */
  private uploadScratch = new Uint8ClampedArray(0);
  private updateScheduled = false;
  private rafId: number | null = null;

  /** The verified NEO transcription; all pixel work goes through it. */
  private readonly neo: NeoPainter;
  /** Surfaces over buffers we do not own, kept so replay does not reallocate. */
  private readonly foreignSurfaces = new WeakMap<Uint8ClampedArray, BufferSurface>();
  /** Every participant's layer pair, in the order they were created. */
  private readonly owners = new Map<LayerOwner, OwnerSlot>();
  /**
   * Reverse index from a live buffer to the participant and layer it is.
   *
   * The drawing methods take a buffer, not an owner, so this is what lets an
   * operation replayed into someone else's layer still repaint their canvas
   * and no one else's -- without a single call site having to name an owner.
   */
  private readonly bufferOwners = new Map<Uint8ClampedArray, BufferIdentity>();
  /**
   * How many times each participant's layers have been written to.
   *
   * Anything caching a copy of a pair can record this alongside it and ask
   * later whether the pair has moved on, rather than relying on whoever did
   * the writing to have remembered to say so. The history's savepoints share
   * arrays between themselves on exactly that question, and every mutation
   * path that forgot to report made a savepoint that was quietly out of date.
   *
   * Bumped from two places, which between them cover every way these buffers
   * change: the surfaces, through which all drawing goes, and the whole-layer
   * repaint, which is what a caller asks for after writing into a buffer in a
   * way no surface saw.
   */
  private readonly generations = new Map<LayerOwner, number>();
  /** Whose pair `layers` refers to, and whose NEO `neo.surfaces` addresses. */
  private localOwner: LayerOwner = DEFAULT_LAYER_OWNER;

  /** How many writes this participant's layers have taken. */
  public layerGeneration(owner: LayerOwner): number {
    return this.generations.get(owner) ?? 0;
  }

  private noteWrite(owner: LayerOwner): void {
    this.generations.set(owner, (this.generations.get(owner) ?? 0) + 1);
  }
  /** Whose layers interactive drawing paints into; null means our own. */
  private targetOwner: LayerOwner | null = null;
  /**
   * Told when the set of participants changes.
   *
   * A pair is allocated the moment an operation first mentions its owner,
   * which is deep inside applying a message. Whoever mounts canvases needs to
   * hear about it to give the new participant one.
   */
  private readonly ownersChangedListeners = new Set<() => void>();
  /**
   * Told when a participant's pair is re-keyed rather than created.
   *
   * Anything holding a copy of these layers keyed by name -- the history's
   * savepoints do -- would otherwise still be filing them under the name the
   * pair had before the server named its participant, and restoring one would
   * conjure a participant back out of the old name.
   */
  private readonly ownerRenamedListeners = new Set<
    (from: LayerOwner, to: LayerOwner) => void
  >();

  /** Registers a rename listener; returns a function that removes it. */
  public onOwnerRenamed(
    listener: (from: LayerOwner, to: LayerOwner) => void
  ): () => void {
    this.ownerRenamedListeners.add(listener);
    return () => {
      this.ownerRenamedListeners.delete(listener);
    };
  }

  /** Registers a listener; returns a function that removes it. */
  public onOwnersChanged(listener: () => void): () => void {
    this.ownersChangedListeners.add(listener);
    return () => {
      this.ownersChangedListeners.delete(listener);
    };
  }

  private announceOwners(): void {
    for (const listener of this.ownersChangedListeners) listener();
  }

  /** Mask settings, applied to every stroke. NEO's MASKTYPE_NONE by default. */
  public maskType = 0;
  public maskColor: [number, number, number] = [0, 0, 0];

  // Stroke continuation state accessors: collaborative replay tracks a
  // per-user stroke state externally so line-joint deduplication is a
  // deterministic function of the canonical message sequence
  public getStrokeState(): [[number, number], [number, number]] | null {
    const prev = this.neo.prevLine;
    return prev === null
      ? null
      : [
          [prev[0][0], prev[0][1]],
          [prev[1][0], prev[1][1]],
        ];
  }

  public setStrokeState(
    state: [[number, number], [number, number]] | null
  ): void {
    this.neo.prevLine = state;
  }
  private panOffsetX = 0;
  private panOffsetY = 0;
  /**
   * The scale the frame was last given, and the translate it was actually
   * given with it -- the pan plus whatever `snapToDevicePixels` added.
   * Anchoring a zoom needs both to find where the frame sits untransformed.
   */
  private viewZoom = 1;
  private appliedPanX = 0;
  private appliedPanY = 0;
  private isFlippedHorizontal = false;

  // Alpha calculation constants

  constructor(width: number = 500, height: number = 500) {
    this.imageWidth = width;
    this.imageHeight = height;

    this.neo = new NeoPainter(width, height);

    const slot = this.createOwnerSlot(DEFAULT_LAYER_OWNER);
    this.layers = slot.layers as unknown as { [key: string]: Uint8ClampedArray };
    // Layer-addressed operations (the region tools) resolve through this, so
    // it has to point at the engine's buffers rather than the painter's own
    // canvases, which nothing here draws to.
    this.neo.surfaces = slot.surfaces;
  }

  /**
   * Allocates a participant's layer pair, its surfaces and its offscreen
   * canvases.
   *
   * Only owned surfaces report their writes: they are the ones on screen, so
   * a fork's writes must not mark any display dirty. Each pair reports into
   * its own slot, which is what keeps one participant's stroke from
   * repainting another's canvas.
   */
  private createOwnerSlot(owner: LayerOwner): OwnerSlot {
    const { imageWidth: width, imageHeight: height } = this;
    const layers: LayerBuffers = {
      background: new Uint8ClampedArray(width * height * 4),
      foreground: new Uint8ClampedArray(width * height * 4),
    };
    const surfaceFor = (layer: LayerName) =>
      new BufferSurface(layers[layer], width, height, (x0, y0, x1, y1) => {
        // Whose pair this is, resolved at the moment of the write rather than
        // captured when the slot was made. A participant is re-keyed when the
        // server names them, and a captured name would keep filing regions
        // under an owner nothing reads -- so the repaint that follows finds
        // nothing to do and the pixels sit in the buffer, invisible, until
        // something else forces a full repaint.
        const live = this.bufferOwners.get(layers[layer]);
        const owning = live?.owner ?? owner;
        this.noteWrite(owning);
        const key = slotKey(owning, layer);
        this.writtenRegions.set(
          key,
          unionRegion(this.writtenRegions.get(key) ?? null, x0, y0, x1, y1),
        );
      });
    const slot: OwnerSlot = {
      layers,
      surfaces: [surfaceFor("background"), surfaceFor("foreground")],
    };

    for (const layer of ["background", "foreground"] as const) {
      this.bufferOwners.set(layers[layer], { owner, layer });
      const key = slotKey(owner, layer);
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.imageSmoothingEnabled = false;
        this.layerCanvases[key] = canvas;
        this.layerContexts[key] = ctx;
      }
    }

    this.owners.set(owner, slot);
    // The constructor allocates the local pair before any listener exists, so
    // this is a no-op there and a real notification for everyone after.
    this.announceOwners();
    return slot;
  }

  /**
   * The pair interactive drawing paints into.
   *
   * Usually the local participant's own, but anyone may work on somebody
   * else's layers, and the optimistic paint has to land where the canonical
   * echo will put it -- otherwise the mark shows up twice: once in the wrong
   * pair, once in the right one.
   */
  public get drawTarget(): LayerBuffers {
    return this.layersFor(this.targetOwner ?? this.localOwner);
  }

  /** Points interactive drawing at a participant's layers. */
  public setDrawTarget(owner: LayerOwner | null): void {
    this.targetOwner = owner;
    // Aiming at somebody who has joined but not yet drawn brings their pair
    // into being. Without this there is no slot to point at, and the region
    // tools -- which reach their pixels through `neo.surfaces` rather than
    // through an argument -- would keep drawing into whoever was selected
    // before.
    this.layersFor(owner ?? this.localOwner);
    const slot = this.owners.get(owner ?? this.localOwner);
    if (slot) this.neo.surfaces = slot.surfaces;
  }

  /** The participant's layer pair, allocated on first mention. */
  public layersFor(owner: LayerOwner): LayerBuffers {
    return (this.owners.get(owner) ?? this.createOwnerSlot(owner)).layers;
  }

  /** True if this participant already has a pair; does not allocate one. */
  public hasOwner(owner: LayerOwner): boolean {
    return this.owners.has(owner);
  }

  /** Every participant with a layer pair, in creation order. */
  public ownerIds(): LayerOwner[] {
    return [...this.owners.keys()];
  }

  /**
   * Renames the local participant's pair.
   *
   * A collaborative client does not learn its session id until the server's
   * WELCOME, by which point the engine already exists. Rather than allocate a
   * second pair and leave the first stranded, the pair the painter has been
   * drawing into is re-keyed to the id everyone else will address it by.
   */
  public setLocalOwner(owner: LayerOwner): void {
    if (owner === this.localOwner) return;
    if (this.owners.has(owner)) {
      throw new Error(`Layer owner ${owner} is already taken`);
    }
    const slot = this.owners.get(this.localOwner);
    if (!slot) return;
    const previous = this.localOwner;
    this.owners.delete(previous);
    this.owners.set(owner, slot);
    if (this.targetOwner === previous) this.targetOwner = owner;
    this.localOwner = owner;

    for (const layer of ["background", "foreground"] as const) {
      this.bufferOwners.set(slot.layers[layer], { owner, layer });
      const from = slotKey(previous, layer);
      const to = slotKey(owner, layer);
      for (const record of [this.layerCanvases, this.layerContexts, this.domCanvases, this.domContexts]) {
        const held = (record as Record<string, unknown>)[from];
        if (held !== undefined) {
          (record as Record<string, unknown>)[to] = held;
          delete (record as Record<string, unknown>)[from];
        }
      }
      for (const map of [this.pendingUpdates, this.writtenRegions]) {
        const held = map.get(from);
        if (held !== undefined) {
          (map as Map<string, unknown>).set(to, held);
          map.delete(from);
        }
      }
    }
    const carried = this.generations.get(previous);
    if (carried !== undefined) {
      this.generations.delete(previous);
      this.generations.set(owner, carried);
    }
    for (const listener of this.ownerRenamedListeners) listener(previous, owner);
    this.announceOwners();
  }


  // Get individual layer canvas for hardware compositing
  public getLayerCanvas(
    layerName: "foreground" | "background",
    owner: LayerOwner = this.localOwner
  ): HTMLCanvasElement | null {
    // Update the offscreen canvas with current layer data
    const key = slotKey(owner, layerName);
    const layerData = this.owners.get(owner)?.layers[layerName];
    const context = this.layerContexts[key];
    const canvas = this.layerCanvases[key];

    if (!context || !canvas || !layerData) {
      return null;
    }

    // Push current layer data to canvas
    const imageData = new ImageData(
      new Uint8ClampedArray(layerData),
      this.imageWidth,
      this.imageHeight
    );
    context.putImageData(imageData, 0, 0);

    return canvas;
  }

  // Get individual layer canvas for direct rendering
  public getLayerCanvasForRendering(
    layerName: "foreground" | "background",
    owner: LayerOwner = this.localOwner
  ): HTMLCanvasElement | null {
    return this.layerCanvases[slotKey(owner, layerName)] || null;
  }

  /**
   * The mounted canvas context for a participant's layer, if it has one.
   *
   * Callers want the pixels currently on screen rather than the buffer behind
   * them; how the record holding them is keyed is this class's business.
   */
  /**
   * The mounted canvas for a participant's layer.
   *
   * For drawing one canvas into another -- a thumbnail of somebody's work is
   * two `drawImage` calls from these. Reading the buffers instead would mean
   * copying the whole layer to build an ImageData every time, which at the
   * largest canvas size is megabytes per participant per refresh.
   */
  public domCanvasFor(
    layerName: LayerName,
    owner: LayerOwner = this.localOwner
  ): HTMLCanvasElement | undefined {
    return this.domCanvases[slotKey(owner, layerName)];
  }

  /**
   * The colour on screen at a point, as NEO's eyedropper reads it.
   *
   * Composited over white through every mounted layer that is showing, bottom
   * to top, so it is what the eye is actually on: hiding a participant or a
   * tier takes them out of it, because both are expressed as `display` on
   * these very canvases and this asks the same question the screen does.
   *
   * NEO's own `pickColor` does the same thing and agrees with this, though
   * its arithmetic reads as though it does not: the variable it calls `r`
   * holds the sampled blue, and it then packs `r | g << 8 | b << 16` -- which
   * `getColorString` renders high byte first, putting red back in the red
   * position. Two swaps that cancel. Worth knowing before 'fixing' either of
   * them, and worth knowing that `getColor` packs the other way round, ABGR
   * with red low, which is what makes the pair look wrong at a glance.
   */
  public pickVisibleColor(
    x: number,
    y: number
  ): { r: number; g: number; b: number } | null {
    const px = Math.floor(x);
    const py = Math.floor(y);
    if (px < 0 || py < 0 || px >= this.imageWidth || py >= this.imageHeight) {
      return null;
    }
    const showing = Object.values(this.domCanvases)
      .filter((canvas) => canvas.style.display !== "none")
      .sort(
        (a, b) => (Number(a.style.zIndex) || 0) - (Number(b.style.zIndex) || 0)
      );

    let r = 255;
    let g = 255;
    let b = 255;
    for (const canvas of showing) {
      const context = canvas.getContext("2d");
      if (!context) continue;
      const [sr, sg, sb, sa] = context.getImageData(px, py, 1, 1).data;
      const alpha = sa / 255;
      r = r * (1 - alpha) + sr * alpha;
      g = g * (1 - alpha) + sg * alpha;
      b = b * (1 - alpha) + sb * alpha;
    }
    return {
      r: Math.max(0, Math.min(255, Math.round(r))),
      g: Math.max(0, Math.min(255, Math.round(g))),
      b: Math.max(0, Math.min(255, Math.round(b))),
    };
  }

  public domContextFor(
    layerName: LayerName,
    owner: LayerOwner = this.localOwner
  ): CanvasRenderingContext2D | undefined {
    return this.domContexts[slotKey(owner, layerName)];
  }

  // Attach DOM canvases for direct updating
  public attachDOMCanvases(
    backgroundCanvas: HTMLCanvasElement,
    foregroundCanvas: HTMLCanvasElement,
    owner: LayerOwner = this.localOwner
  ) {
    this.layersFor(owner);
    const pairs = [
      ["background", backgroundCanvas],
      ["foreground", foregroundCanvas],
    ] as const;

    for (const [layer, canvas] of pairs) {
      const key = slotKey(owner, layer);
      this.domCanvases[key] = canvas;
      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.imageSmoothingEnabled = false;
        this.domContexts[key] = ctx;
      }
    }
  }

  /** Forgets a participant's canvases, buffers and repaint bookkeeping. */
  public releaseOwner(owner: LayerOwner): void {
    const slot = this.owners.get(owner);
    if (!slot || owner === this.localOwner) return;
    for (const layer of ["background", "foreground"] as const) {
      this.bufferOwners.delete(slot.layers[layer]);
      const key = slotKey(owner, layer);
      delete this.layerCanvases[key];
      delete this.layerContexts[key];
      delete this.domCanvases[key];
      delete this.domContexts[key];
      this.pendingUpdates.delete(key);
      this.writtenRegions.delete(key);
    }
    this.owners.delete(owner);
    this.announceOwners();
  }

  /**
   * Uploads a layer's pending region to its DOM canvas.
   *
   * Building a full-canvas ImageData is what makes a repaint expensive, not
   * the blit -- passing a dirty rectangle to `putImageData` while still
   * handing it the whole buffer saves nothing. So a partial repaint stages
   * just its own rows and hands over an ImageData that size.
   */
  private updateDOMCanvas(owner: LayerOwner, layerName: LayerName) {
    const key = slotKey(owner, layerName);
    const domCtx = this.domContexts[key];
    const layerData = this.owners.get(owner)?.layers[layerName];
    const region = this.pendingUpdates.get(key);
    if (!domCtx || !layerData || !region) return;
    this.pendingUpdates.delete(key);

    if (region === "all") {
      // The buffer itself, not a copy of it. `ImageData` wraps what it is
      // given and `putImageData` reads it before returning, so the copy bought
      // nothing -- and this is the path a checkpoint takes, where it bought
      // nothing once per layer per participant.
      domCtx.putImageData(
        // The cast is the `SharedArrayBuffer` case `ImageDataArray` excludes.
        // Every layer here is allocated as a plain `new Uint8ClampedArray(n)`,
        // and nothing in this engine can produce a shared one.
        new ImageData(
          layerData as Uint8ClampedArray<ArrayBuffer>,
          this.imageWidth,
          this.imageHeight
        ),
        0,
        0
      );
      return;
    }

    const x0 = Math.max(0, Math.min(this.imageWidth - 1, Math.floor(region.x0)));
    const y0 = Math.max(0, Math.min(this.imageHeight - 1, Math.floor(region.y0)));
    const x1 = Math.max(x0, Math.min(this.imageWidth - 1, Math.ceil(region.x1)));
    const y1 = Math.max(y0, Math.min(this.imageHeight - 1, Math.ceil(region.y1)));
    const width = x1 - x0 + 1;
    const height = y1 - y0 + 1;
    const needed = width * height * 4;
    if (this.uploadScratch.length < needed) {
      this.uploadScratch = new Uint8ClampedArray(needed);
    }
    const rowBytes = width * 4;
    for (let row = 0; row < height; row++) {
      const from = ((y0 + row) * this.imageWidth + x0) * 4;
      this.uploadScratch.set(layerData.subarray(from, from + rowBytes), row * rowBytes);
    }
    domCtx.putImageData(
      new ImageData(this.uploadScratch.subarray(0, needed), width, height),
      x0,
      y0
    );
  }

  // Update all attached DOM canvases
  public updateAllDOMCanvases() {
    for (const owner of this.owners.keys()) {
      for (const layer of ["background", "foreground"] as const) {
        this.pendingUpdates.set(slotKey(owner, layer), "all");
        this.updateDOMCanvas(owner, layer);
      }
    }
  }

  // Batched update methods
  private scheduleBatchedUpdate() {
    if (!this.updateScheduled) {
      this.updateScheduled = true;
      this.rafId = requestAnimationFrame(() => this.processBatchedUpdates());
    }
  }

  private processBatchedUpdates() {
    for (const owner of this.owners.keys()) {
      this.updateDOMCanvas(owner, "background");
      this.updateDOMCanvas(owner, "foreground");
    }
    this.updateScheduled = false;
    this.rafId = null;
  }

  /** Queues a repaint of the whole layer: everything in it may have changed. */
  public queueLayerUpdate(layerName: LayerName, owner: LayerOwner = this.localOwner) {
    // Asked for after putting pixels somewhere this engine cannot describe --
    // a decoded snapshot, a restored savepoint, text composited a pixel at a
    // time. Whatever it was, the layers are not what they were.
    this.noteWrite(owner);
    const key = slotKey(owner, layerName);
    this.pendingUpdates.set(key, "all");
    this.writtenRegions.delete(key);
    this.scheduleBatchedUpdate();
  }

  /**
   * Queues a repaint of just the pixels drawn since the last one.
   *
   * For callers whose every write went through this engine's own surfaces --
   * which is all of them except `drawText`, and anything that assigns into a
   * layer buffer directly. Those must use `queueLayerUpdate`, or the screen
   * keeps pixels the buffer no longer has.
   */
  public queueLayerRegionUpdate(layerName: LayerName, owner: LayerOwner = this.localOwner) {
    const key = slotKey(owner, layerName);
    const written = this.writtenRegions.get(key);
    if (!written) return;
    this.writtenRegions.delete(key);
    const pending = this.pendingUpdates.get(key);
    this.pendingUpdates.set(
      key,
      pending === "all"
        ? "all"
        : unionRegion(pending ?? null, written.x0, written.y0, written.x1, written.y1)
    );
    this.scheduleBatchedUpdate();
  }

  // Force immediate update of all pending layers
  public flushBatchedUpdates() {
    if (this.updateScheduled && this.rafId) {
      cancelAnimationFrame(this.rafId);
      this.processBatchedUpdates();
    }
  }

  // For critical operations that need immediate rendering (like initialization)
  public updateAllDOMCanvasesImmediate() {
    // Cancel any pending batched update and clear queue
    if (this.updateScheduled && this.rafId) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
      this.updateScheduled = false;
    }
    this.updateAllDOMCanvases();
  }

  // Pan offset management - applies to all canvases
  public updatePanOffset(
    deltaX: number,
    deltaY: number,
    container?: HTMLCanvasElement | HTMLDivElement,
    zoomScale?: number
  ) {
    this.panOffsetX += deltaX;
    this.panOffsetY += deltaY;
    this.updateCanvasPan(container, zoomScale);
  }

  /** The zoom the frame is showing right now, as a scale. */
  public get zoom(): number {
    return this.viewZoom;
  }

  /**
   * Change the zoom, keeping the artwork under (`clientX`, `clientY`) where it
   * is on screen, and apply the scale and the compensating pan in one write.
   *
   * They used to land separately -- the scale when React re-rendered, the pan
   * on an animation frame after that -- and the frame in between drew the
   * canvas scaled about its centre, which is the jump away from the pointer.
   *
   * The frame is `scale(z) translate(p)` about its centre, so a point `a`
   * pixels from the untransformed centre is at `z * (a/z_local + p)`; holding
   * it still across `z -> z'` moves the pan by `a * (1/z' - 1/z)`.
   */
  public zoomAround(
    newZoom: number,
    container: HTMLCanvasElement | HTMLDivElement | undefined,
    clientX?: number,
    clientY?: number
  ) {
    const oldZoom = this.viewZoom;
    const frame = container ? this.frameFor(container) : null;
    if (
      frame &&
      clientX !== undefined &&
      clientY !== undefined &&
      oldZoom > 0 &&
      newZoom > 0
    ) {
      const rect = frame.getBoundingClientRect();
      // Scaling about the centre leaves the centre where the translate put
      // it, so taking the translate back out finds the layout centre.
      const centreX = rect.left + rect.width / 2 - this.appliedPanX * oldZoom;
      const centreY = rect.top + rect.height / 2 - this.appliedPanY * oldZoom;
      const factor = 1 / newZoom - 1 / oldZoom;
      this.panOffsetX += (clientX - centreX) * factor;
      this.panOffsetY += (clientY - centreY) * factor;
    }
    this.updateCanvasPan(container, newZoom);
  }

  public resetPan(container?: HTMLCanvasElement | HTMLDivElement, zoomScale?: number) {
    this.panOffsetX = 0;
    this.panOffsetY = 0;
    this.updateCanvasPan(container, zoomScale);
  }

  public setFlippedHorizontal(isFlipped: boolean, container?: HTMLCanvasElement | HTMLDivElement, zoomScale?: number) {
    this.isFlippedHorizontal = isFlipped;
    this.updateCanvasPan(container, zoomScale);
  }

  /** The `.canvas-container` frame for whatever element a caller holds. */
  private frameFor(container: HTMLCanvasElement | HTMLDivElement): HTMLElement | null {
    const frame =
      container.tagName === "CANVAS"
        ? container.closest<HTMLElement>(".canvas-container")
        : container;
    return frame ?? document.querySelector<HTMLElement>(".canvas-container");
  }

  private updateCanvasPan(container?: HTMLCanvasElement | HTMLDivElement, zoomScale?: number) {
    if (zoomScale) this.viewZoom = zoomScale;
    if (!container) return;
    const actualContainer = this.frameFor(container);
    if (!actualContainer) return;

    this.clampPanToViewport(actualContainer, this.viewZoom);

    // Transform around the canvas centre, matching the flex-centred painter
    // area. Keeping the transform origin explicit makes the pan bounds below
    // independent of browser defaults.
    actualContainer.style.transformOrigin = "center";
    const scaleTransform = `scale(${this.viewZoom})`;
    const translateTransform = `translate(${this.panOffsetX}px, ${this.panOffsetY}px)`;
    const transform = [scaleTransform, translateTransform]
      .filter(Boolean)
      .join(" ");

    // Apply transform to the container itself
    actualContainer.style.transform = transform;

    // Flipping is a canvas-view operation, not a transform of the canvas
    // frame. Keep zoom and pan on the frame while mirroring only the plane
    // containing the rendered layers and interaction overlays.
    const canvasContent =
      actualContainer.querySelector<HTMLElement>(".canvas-content");
    if (canvasContent) {
      canvasContent.style.transformOrigin = "center";
      canvasContent.style.transform = this.isFlippedHorizontal ? "scaleX(-1)" : "";
    }

    this.appliedPanX = this.panOffsetX;
    this.appliedPanY = this.panOffsetY;
    this.snapToDevicePixels(actualContainer, this.viewZoom, scaleTransform);
  }

  /**
   * Land the artwork's top-left corner on a whole device pixel.
   *
   * A whole-number zoom only keeps pixels square if they also start on a
   * pixel boundary. Put the canvas half a pixel off and every artwork pixel
   * straddles two screen pixels, which nearest-neighbour sampling resolves
   * one way for some and the other way for the rest -- the same thin-thick
   * wobble a fractional zoom gives, at 2x. NEO rounds its view position in
   * `setZoomPosition` for exactly this.
   *
   * Here the half pixel has several sources that are awkward to predict
   * together: flex centring puts the frame at `(viewport - width) / 2`, the
   * centre origin shifts an odd-sized canvas by half a pixel per zoom step,
   * and a zoom anchored under the pointer produces a fractional pan. So
   * rather than rounding any one of them, this measures where the canvas
   * actually ended up and moves it the rest of the way. The correction is
   * applied to the transform only, never stored in the pan, so repeated
   * adjustments do not drift.
   */
  private snapToDevicePixels(
    container: HTMLElement,
    zoom: number,
    scaleTransform: string
  ) {
    const artwork =
      container.querySelector<HTMLElement>(".canvas-content canvas") ??
      container.querySelector<HTMLElement>(".canvas-content");
    if (!artwork || zoom <= 0) return;

    const box = artwork.getBoundingClientRect();
    const ratio = window.devicePixelRatio || 1;
    const nudge = (edge: number) =>
      (Math.round(edge * ratio) - edge * ratio) / ratio;
    const dx = nudge(box.left);
    const dy = nudge(box.top);
    if (dx === 0 && dy === 0) return;

    // The translate sits inside the scale, so a screen distance is divided
    // by the zoom to become a translate distance.
    this.appliedPanX = this.panOffsetX + dx / zoom;
    this.appliedPanY = this.panOffsetY + dy / zoom;
    container.style.transform = [
      scaleTransform,
      `translate(${this.appliedPanX}px, ${this.appliedPanY}px)`,
    ]
      .filter(Boolean)
      .join(" ");
  }

  /**
   * Bound pan to the painter viewport: the canvas can travel, but always
   * leaves a recovery strip in view so it cannot be lost beyond an edge.
   *
   * This used to hold a canvas that fits entirely inside the viewport, which
   * fought every zoom anchored near an edge -- the pointer asked for a pan
   * the clamp refused, and the drawing slid out from under it as it crossed
   * from fitting to not.
   */
  private clampPanToViewport(container: HTMLElement, zoom: number) {
    const viewport = container.parentElement?.getBoundingClientRect();
    if (!viewport || zoom <= 0) return;

    const visibleStrip = 48;
    const scaledWidth = container.offsetWidth * zoom;
    const scaledHeight = container.offsetHeight * zoom;
    const maxScreenPan = (scaledSize: number, viewportSize: number) =>
      Math.max(
        (viewportSize - scaledSize) / 2,
        (viewportSize + scaledSize) / 2 - visibleStrip
      );

    const maxPanX = maxScreenPan(scaledWidth, viewport.width) / zoom;
    const maxPanY = maxScreenPan(scaledHeight, viewport.height) / zoom;
    this.panOffsetX = Math.max(-maxPanX, Math.min(this.panOffsetX, maxPanX));
    this.panOffsetY = Math.max(-maxPanY, Math.min(this.panOffsetY, maxPanY));
  }

  /** Maps the painter's brush names onto NEO's line types. */
  private static lineTypeFor(brushType: string): number {
    switch (brushType) {
      case "eraser":
        return LINETYPE.ERASER;
      case "halftone":
        return LINETYPE.TONE;
      case "brush":
        return LINETYPE.BRUSH;
      case "dodge":
        return LINETYPE.DODGE;
      case "burn":
        return LINETYPE.BURN;
      case "blur":
        return LINETYPE.BLUR;
      default:
        // solid, and the non-drawing tools that still stamp a pixel
        return LINETYPE.PEN;
    }
  }

  /**
   * Wraps a pixel buffer so a kernel can draw into it.
   *
   * Every participant's live layers get their own standing surfaces, which is
   * how an operation replayed into someone else's layer marks their canvas
   * dirty and nobody else's. Anything else is a buffer canvasHistory owns --
   * it replays history into temporary forks when compacting -- and it gets a
   * surface over *that* buffer. Falling through to the foreground surface
   * instead, as this used to, meant a compaction drew its replay onto the
   * live canvas.
   */
  private surfaceFor(ctx: Uint8ClampedArray): BufferSurface {
    const live = this.bufferOwners.get(ctx);
    if (live) {
      const slot = this.owners.get(live.owner);
      if (slot) {
        return slot.surfaces[live.layer === "background" ? 0 : 1];
      }
    }
    let surface = this.foreignSurfaces.get(ctx);
    if (!surface) {
      surface = new BufferSurface(ctx, this.imageWidth, this.imageHeight);
      this.foreignSurfaces.set(ctx, surface);
    }
    return surface;
  }

  /**
   * Runs an operation against buffers other than the live layers.
   *
   * Region ops reach their pixels through `neo.surfaces` rather than through
   * an argument -- merge needs both layers at once -- so redirecting them
   * means swapping that pair for the duration and putting it back after.
   */
  private withTargets<T>(
    targets: LayerBuffers | undefined,
    run: () => T
  ): T {
    if (!targets) return run();
    const saved = this.neo.surfaces;
    this.neo.surfaces = [
      this.surfaceFor(targets.background),
      this.surfaceFor(targets.foreground),
    ];
    try {
      return run();
    } finally {
      this.neo.surfaces = saved;
    }
  }

  /** Only live buffers are on screen; a fork's repaint would be a lie. */
  private queueUpdateIfLive(ctx: Uint8ClampedArray): void {
    const live = this.bufferOwners.get(ctx);
    if (live) this.queueLayerRegionUpdate(live.layer, live.owner);
  }

  /**
   * As above, but for a write this engine cannot describe -- one that went
   * into the buffer rather than through a surface. The whole layer repaints
   * because nothing knows any better.
   */
  private queueFullUpdateIfLive(ctx: Uint8ClampedArray): void {
    const live = this.bufferOwners.get(ctx);
    if (live) this.queueLayerUpdate(live.layer, live.owner);
  }

  public doFloodFill(
    ctx: Uint8ClampedArray,
    startX: number,
    startY: number,
    fillR: number,
    fillG: number,
    fillB: number,
    fillA: number
  ) {
    // NEO packs the fill colour ABGR
    const color =
      ((fillA & 0xff) << 24) |
      ((fillB & 0xff) << 16) |
      ((fillG & 0xff) << 8) |
      (fillR & 0xff);
    this.neo.doFloodFill(this.surfaceFor(ctx), startX, startY, color);

    this.queueUpdateIfLive(ctx);
  }

  /**
   * Floods, and says which pixels it touched.
   *
   * A fill that is going to be sent as pixels has to be cropped to the region
   * it actually covered, and the surfaces already record exactly that as they
   * write -- so this reads the region before the repaint consumes it rather
   * than comparing the layer against a copy of itself.
   *
   * Returns null when the fill changed nothing, which is a click on a pixel
   * that is already the fill colour.
   */
  public floodFillCapturingRegion(
    ctx: Uint8ClampedArray,
    startX: number,
    startY: number,
    fillR: number,
    fillG: number,
    fillB: number,
    fillA: number
  ): {
    x: number;
    y: number;
    width: number;
    height: number;
    /** One bit per pixel of the box, row-major: did the flood reach here. */
    coverage: Uint8Array;
  } | null {
    // Compared against a copy rather than read from the surfaces: NEO's flood
    // writes the whole layer back in one go, so what the surfaces record is
    // "everything" and says nothing about where the paint landed.
    const before = new Uint32Array(
      ctx.buffer.slice(ctx.byteOffset, ctx.byteOffset + ctx.byteLength)
    );

    this.doFloodFill(ctx, startX, startY, fillR, fillG, fillB, fillA);

    const after = new Uint32Array(ctx.buffer, ctx.byteOffset, before.length);
    const { imageWidth: width } = this;
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    for (let i = 0; i < before.length; i++) {
      if (before[i] === after[i]) continue;
      const x = i % width;
      const y = (i - x) / width;
      if (x < x0) x0 = x;
      if (x > x1) x1 = x;
      if (y < y0) y0 = y;
      if (y > y1) y1 = y;
    }
    // Nothing changed: a click on a pixel that is already the fill colour.
    if (x1 < x0) return null;

    // Which pixels the flood reached, rather than what the box now looks like.
    // A box carries whatever else happens to lie inside it, and stamping that
    // back down on a replay resurrects work somebody has since undone.
    const boxWidth = x1 - x0 + 1;
    const boxHeight = y1 - y0 + 1;
    const coverage = new Uint8Array(Math.ceil((boxWidth * boxHeight) / 8));
    for (let row = 0; row < boxHeight; row++) {
      const source = (y0 + row) * width + x0;
      const bitRow = row * boxWidth;
      for (let column = 0; column < boxWidth; column++) {
        if (before[source + column] === after[source + column]) continue;
        const bit = bitRow + column;
        coverage[bit >> 3] |= 0x80 >> (bit & 7);
      }
    }
    return { x: x0, y: y0, width: boxWidth, height: boxHeight, coverage };
  }

  /**
   * Sets every pixel the coverage names to one colour.
   *
   * How a flood fill arrives from somebody else. NEO's flood replaces a pixel
   * outright rather than blending into it, so this does too -- and only where
   * the flood actually reached, leaving everything else in the box as it
   * currently is rather than as it was when the fill was made.
   */
  public paintCoveredPixels(
    ctx: Uint8ClampedArray,
    x: number,
    y: number,
    width: number,
    height: number,
    color: { r: number; g: number; b: number; a: number },
    coverage: Uint8Array
  ): void {
    const packed = new Uint32Array(1);
    new Uint8ClampedArray(packed.buffer).set([color.r, color.g, color.b, color.a]);
    const pixels = new Uint32Array(ctx.buffer, ctx.byteOffset, ctx.byteLength / 4);
    for (let row = 0; row < height; row++) {
      const into = (y + row) * this.imageWidth + x;
      const bitRow = row * width;
      for (let column = 0; column < width; column++) {
        const bit = bitRow + column;
        if (coverage[bit >> 3] & (0x80 >> (bit & 7))) {
          pixels[into + column] = packed[0];
        }
      }
    }
    // Written straight into the buffer, so no surface saw it: the whole layer
    // repaints, which is also what tells the history it has moved on.
    this.queueFullUpdateIfLive(ctx);
  }

  public drawLine(
    ctx: Uint8ClampedArray,
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    brushSize: number,
    brushType: string,
    r: number,
    g: number,
    b: number,
    opacity: number
  ) {
    this.neo._currentColor = [r, g, b, opacity];
    this.neo._currentWidth = brushSize;
    this.neo._currentMaskType = this.maskType;
    this.neo._currentMask = this.maskColor;

    // Whole pixels, always. NEO's Bresenham walks from one endpoint to the
    // other by steps of one and stops on `x0 === x1 && y0 === y1`, so an
    // endpoint with a fraction is an endpoint the walk steps straight past:
    // the equality never holds and the loop does not end. Every caller here
    // already rounds -- `screenToArtwork` and the stroke smoother both do, and
    // wire coordinates are integers -- so this changes no drawing that has
    // ever happened, and is the difference between a bug and a frozen tab for
    // one that has not.
    this.neo.drawLine(
      this.surfaceFor(ctx),
      Math.round(x0),
      Math.round(y0),
      Math.round(x1),
      Math.round(y1),
      DrawingEngine.lineTypeFor(brushType)
    );

    this.queueUpdateIfLive(ctx);
  }

  /**
   * Applies a region tool to a rectangle, in the layer the caller names.
   *
   * One entry point rather than a switch at every call site: the painter, the
   * replay recorder and the collaborative history all need the same mapping
   * from tool to kernel, and three copies of it would eventually disagree.
   */
  public applyRegionTool(
    tool: RegionTool,
    layer: "foreground" | "background",
    rect: { x: number; y: number; width: number; height: number },
    color: { r: number; g: number; b: number; a: number },
    brushSize: number,
    targets?: LayerBuffers
  ): void {
    const index = layer === "foreground" ? 1 : 0;
    const { x, y, width, height } = rect;

    this.neo._currentColor = [color.r, color.g, color.b, color.a];
    this.neo._currentWidth = brushSize;
    this.neo._currentMaskType = this.maskType;
    this.neo._currentMask = this.maskColor;

    const fillType = fillToolTypeFor(tool);
    this.withTargets(targets, () => {
    if (fillType !== null) {
      this.neo.doFill(index, x, y, width, height, fillType);
    } else {
      switch (tool) {
        case "eraseRect":
          this.neo.eraseRect(index, x, y, width, height);
          break;
        case "blurRect":
          this.neo.blurRect(index, x, y, width, height);
          break;
        case "merge":
          this.neo.merge(index, x, y, width, height);
          break;
        case "flipH":
          this.neo.flipH(index, x, y, width, height);
          break;
        case "flipV":
          this.neo.flipV(index, x, y, width, height);
          break;
        case "turn":
          this.neo.turn(index, x, y, width, height);
          break;
        case "copy":
          // Reads into the clipboard; writes nothing
          this.neo.copy(index, x, y, width, height);
          break;
        case "paste":
          // `rect` is where the copy lands -- the source moved by however far
          // it was dragged, at the size it was copied -- so the offset NEO
          // takes separately is already folded in. The recorded `.pch` frame
          // keeps NEO's own form, source and offset; see useOfflineDrawing.
          this.neo.paste(index, x, y, width, height, 0, 0);
          break;
      }
    }
    });

    // merge writes both layers, the rest write one, and copy writes neither.
    // The surfaces reported which, so asking for both costs nothing when only
    // one of them has a region to upload. A fork's buffers are not live and
    // resolve to nothing, which is how a compaction stays off the screen.
    const written = targets ?? (this.layers as unknown as LayerBuffers);
    this.queueUpdateIfLive(written.background);
    this.queueUpdateIfLive(written.foreground);
  }

  /**
   * The clipboard copy/paste works through. Exposed because a collaborative
   * session keeps one per user rather than one per client: the pixels a paste
   * needs are the ones the *sender* copied.
   */
  public getClipboard(): ImageData | null {
    return this.neo.getClipboard();
  }

  public setClipboard(data: ImageData | null): void {
    this.neo.setClipboard(data);
  }

  /** Clears a whole layer, NEO's EraseAllTool. */
  public eraseAll(layer: "foreground" | "background", targets?: LayerBuffers): void {
    this.withTargets(targets, () =>
      this.neo.eraseAll(layer === "foreground" ? 1 : 0)
    );
    this.queueUpdateIfLive((targets ?? (this.layers as unknown as LayerBuffers))[layer]);
  }

  /**
   * Draws text onto a layer.
   *
   * NEO rasterises text through a scratch canvas and composites the result, so
   * this does the same and then blends that canvas into the layer buffer --
   * fillText has no buffer equivalent to call.
   */
  public drawText(
    layer: "foreground" | "background",
    x: number,
    y: number,
    color: { r: number; g: number; b: number },
    alpha: number,
    text: string,
    fontSize: string,
    fontFamily: string,
    into?: Uint8ClampedArray
  ): void {
    if (!text) return;
    const scratch = this.neo.canvasCtx[0];
    scratch.clearRect(0, 0, this.imageWidth, this.imageHeight);

    // NEO packs the colour with red in the low byte
    const packed = color.r | (color.g << 8) | (color.b << 16);
    this.neo.doText(0, x, y, packed, alpha, text, fontSize, fontFamily);

    const drawn = scratch.getImageData(0, 0, this.imageWidth, this.imageHeight).data;
    const target = into ?? this.layers[layer];
    for (let i = 0; i < target.length; i += 4) {
      const a1 = drawn[i + 3] / 255;
      if (a1 === 0) continue;
      const a0 = target[i + 3] / 255;
      const a = a0 + a1 - a0 * a1;
      target[i] = (drawn[i] * a1 + target[i] * a0 * (1 - a1)) / a;
      target[i + 1] = (drawn[i + 1] * a1 + target[i + 1] * a0 * (1 - a1)) / a;
      target[i + 2] = (drawn[i + 2] * a1 + target[i + 2] * a0 * (1 - a1)) / a;
      target[i + 3] = Math.round(a * 255);
    }
    scratch.clearRect(0, 0, this.imageWidth, this.imageHeight);
    // Composited into the buffer a pixel at a time rather than through a
    // surface, so there is no recorded region -- and glyph metrics are the
    // font's business anyway. Repaint the layer.
    this.queueFullUpdateIfLive(target);
  }

  /** Draws a cubic bezier through the four control points. */
  public drawBezier(
    layer: "foreground" | "background",
    points: [number, number, number, number, number, number, number, number],
    brushSize: number,
    brushType: string,
    color: { r: number; g: number; b: number; a: number },
    target?: Uint8ClampedArray
  ): void {
    this.neo._currentColor = [color.r, color.g, color.b, color.a];
    this.neo._currentWidth = brushSize;
    this.neo._currentMaskType = this.maskType;
    this.neo._currentMask = this.maskColor;
    this.neo.drawBezier(
      this.surfaceFor(target ?? this.layers[layer]),
      points[0], points[1], points[2], points[3],
      points[4], points[5], points[6], points[7],
      DrawingEngine.lineTypeFor(brushType)
    );
    this.neo.prevLine = null;
    // Whoever's buffer it was, as every other kernel here does: a bezier
    // aimed at another participant's layers is on screen too.
    this.queueUpdateIfLive(target ?? this.layers[layer]);
  }

  /**
   * Remembers the canvas the host mounted us into.
   *
   * Nothing is painted here. The drawing itself lives in one DOM canvas per
   * participant per layer, stacked in the container and repainted through
   * `queueLayerRegionUpdate`; this canvas is underneath them and takes pointer
   * events. It used to be handed a full-canvas composite built by a
   * pixel-by-pixel loop over a buffer this engine kept for the purpose -- of a
   * blank canvas, onto a blank canvas, once, and never again.
   */
  public initialize(ctx?: CanvasRenderingContext2D) {
    if (ctx) {
      this.canvas = ctx.canvas;
      ctx.imageSmoothingEnabled = false;
    }
  }

  public dispose() {
    // Cancel any pending animation frame
    if (this.rafId) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }

    // Clean up resources if needed
    this.layers.background = new Uint8ClampedArray(0);
    this.layers.foreground = new Uint8ClampedArray(0);
    this.owners.clear();
    this.bufferOwners.clear();

    // Clean up offscreen canvases
    this.layerCanvases = {};
    this.layerContexts = {};
    this.domCanvases = {};
    this.domContexts = {};

    // Reset batched update state
    this.pendingUpdates.clear();
    this.writtenRegions.clear();
    this.updateScheduled = false;
  }
}
