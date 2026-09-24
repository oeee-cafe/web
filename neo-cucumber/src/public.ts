import { mountOfflinePainter } from "./mountOfflinePainter";

export type {
  CanonicalPainterOperation,
  LocalPainterOperation,
  PainterBrush,
  PainterCheckpoint,
  PainterCheckpointLayers,
  PainterColor,
  PainterLayer,
  PainterMask,
  PainterOperation,
  PainterPoint,
  PainterRegionTool,
  PainterSessionArchive,
} from "./operations";

export type { PalettePreset } from "./constants/palettePresets";
import type { PalettePreset } from "./constants/palettePresets";

/**
 * NEO's chrome as class names, for controls the host renders beside the
 * painter. They name rules carried by `neo-cucumber/style.css`; see
 * `./styles` for what each one is.
 */
export {
  NEO_BUTTON,
  NEO_BUTTON_ON,
  NEO_FIELD,
  NEO_ICON_BUTTON,
  NEO_KBD,
  NEO_PANEL,
  NEO_PANEL_BUTTON,
  NEO_RESIZE_GRIP,
  NEO_RESIZE_HANDLE,
  NEO_TITLEBAR,
  NEO_TITLEBAR_DOT,
  NEO_TITLEBAR_HANDLE,
  NEO_WELL,
} from "./styles";

/**
 * Moving and sizing a floating panel the way the painter's own windows do:
 * by its title bar, and by the corner named by `NEO_RESIZE_HANDLE`.
 * Framework-neutral, and both report rather than apply -- a React window keeps
 * the numbers in state, a plain one writes them to `style`.
 */
export {
  attachWindowDrag,
  attachWindowResize,
  clampWindowPosition,
  type WindowDragOptions,
  type WindowPosition,
  type WindowResizeOptions,
  type WindowSize,
} from "./utils/windowDrag";

/**
 * What the painter calls things.
 *
 * `painterLabels()` resolves every tool, mask and layer name into the locale
 * the painter is running in, so a host can label controls of its own with the
 * same words the column beside them uses rather than guessing at them. Pass
 * overrides to `mount` to replace any of them.
 */
export {
  painterLabels,
  type PainterLabelOverrides,
  type PainterLabels,
} from "./labels";

/**
 * Placing a window the way the painter places its own: `anchorBesideCanvas`
 * puts one against either side of the drawing, `minimumTop` is how high it may
 * go given the element the painter was mounted into, and `PANEL_MARGIN` is the
 * gap all of them leave.
 *
 * Exported so a host's windows line up with the toolboxes and respect the same
 * chrome. A host that hardcodes its own numbers is a host whose windows drift
 * out of line the moment its header changes height.
 */
export {
  anchorBesideCanvas,
  minimumTop,
  PANEL_MARGIN,
} from "./components/toolboxAnchor";

/**
 * The icons the painter's own controls are drawn with, for a host labelling
 * controls beside them.
 *
 * Their artwork is bundled rather than fetched from Iconify's API, so they
 * appear with the frame that asks for them -- which is also why a host should
 * take this one rather than `@iconify/react`'s: an icon that is not in the
 * bundled set is still a network round trip, and none at all offline. See
 * `./components/materialSymbols` for what is bundled.
 */
export { Icon, type IconProps } from "./components/Icon";
/** Where a host's own marks go in the canvas stack, relative to the painter's. */
export { CANVAS_Z_INDEX } from "./neo/canvasStack";
/** zlib, as the painter already carries it for fill coverage on the wire. */
export { deflateZlib, inflateZlib } from "./utils/rasterCodec";

/**
 * Lingui, set up the way the painter sets itself up, for a host with catalogs
 * of its own.
 *
 * `activateLocale` loads a catalog into the `i18n` it is given and activates
 * it, with English for any language the catalogs lack; `DefaultI18n` is the
 * element the painter renders its `<Trans>` text into. A host rendering its
 * own messages beside the painter's takes both, so the two agree on what an
 * unsupported language falls back to and on what a message is wrapped in.
 * The catalogs stay the host's: the package's are loaded by `mount`.
 */
export { activateLocale, type Catalogs } from "./utils/activateLocale";
export { DefaultI18n } from "./components/DefaultI18n";

/**
 * Public API for neo-cucumber.
 *
 * This file is deliberately host- and framework-neutral. It is the contract
 * exported by the package. Canvas and toolbox implementation details are not
 * package exports.
 */

/** The painter behavior, independent of whichever controls render around it. */
export type PainterMode =
  | { kind: "standard" }
  | {
      kind: "two-tone";
      backgroundColor: string;
      foregroundColor: string;
    };

/**
 * Optional controls supplied by neo-cucumber.
 *
 * The toolbox itself is intentionally not a public component API. Consumers
 * may opt into the maintained preset, or mount only the drawing canvas.
 */
export type PainterControls =
  | { kind: "none" }
  | { kind: "toolbox" };

export interface PainterOptions {
  /** Integer dimensions: width 1–1024 and height 1–800. */
  width: number;
  height: number;
  mode: PainterMode;
  controls: PainterControls;
  /** BCP 47 language tag used by prebuilt controls. */
  locale?: string;
  /**
   * Replacements for the painter's own words, for a host that would rather
   * choose them. Anything left out keeps the painter's; see `painterLabels`.
   */
  labels?: import("./neo/labels").PainterLabelOverrides;
  /**
   * Sets of fourteen swatches the toolbox offers to swap in at once, as
   * POTI-board does with its `palette.txt`. Colours are in NEO's order -- the
   * order `Neo.getColors` returns and `palette.txt` is written in -- so a set
   * copied off a board works unchanged.
   *
   * Replaces the painter's own list, which is NEO's palette followed by the
   * sets POTI-board ships. An empty list offers none, and takes the button
   * away with it. Ignored in two-tone mode, whose palette is its two pens.
   */
  palettePresets?: readonly PalettePreset[];
  /**
   * Whether to record a `.pch` replay of this drawing. On by default.
   *
   * A collaborative host turns it off. Such a session saves a flattened image
   * and never asks for a replay, and the format could not describe it in any
   * case: `.pch` addresses two layers, and a session has a pair per
   * participant. Recording one anyway costs a list that grows with every mark
   * and, at each restore point, two full-canvas images kept for nothing.
   *
   * With it off, `exportReplay` and `save` reject rather than hand back an
   * empty file that looks like a drawing nobody made.
   */
  recordReplay?: boolean;
  /** Called after the pixels or replay history change. */
  onChange?: (state: PainterChange) => void;
  /** Called for asynchronous errors that cannot be returned to the caller. */
  onError?: (error: PainterError) => void;
  /** Optional controlled-operation sink used by collaborative hosts. */
  synchronization?: {
    /**
     * Identity stamped on local operations until the server assigns one.
     * Hosts whose canonical stream is keyed by a server-assigned id must
     * adopt it with `setLocalActorId` before the first local operation.
     */
    actorId: string;
    onOperation(operation: import("./operations").LocalPainterOperation): void;
    /** Ephemeral canvas-space hover position, or null after leaving. */
    onPointerMove?: (position: import("./operations").PainterPoint | null) => void;
    /** Called when a pointer stroke ends so hosts can retire remote cursors. */
    onPointerUp?: () => void;
  };
}

export type PainterCommand = "toggle-eraser" | "previous-tool";

export interface PainterChange {
  canUndo: boolean;
  canRedo: boolean;
  strokeCount: number;
  dirty: boolean;
}

export type PainterErrorCode =
  | "invalid-options"
  | "image-load-failed"
  | "export-failed"
  | "unmounted"
  | "internal";

export interface PainterError extends Error {
  code: PainterErrorCode;
  cause?: unknown;
}

/** A URL, URL string, or browser-owned image bytes. */
export type ImageSource = URL | string | Blob;

export interface PainterExport {
  png: Blob;
  replay: Blob;
  width: number;
  height: number;
  strokeCount: number;
}

/**
 * Stable lifecycle owned by neo-cucumber rather than React.
 * Every async method rejects with PainterError after unmounting.
 */
export interface PainterHandle {
  /** Resolves after canvases, history, and optional controls are ready. */
  readonly ready: Promise<void>;

  /**
   * Capture PNG and replay atomically from one canvas state.
   * This does not perform network I/O.
   */
  save(): Promise<PainterExport>;

  /** Export the composited artwork without changing replay history. */
  exportPng(): Promise<Blob>;

  /** Export a NEO-compatible .pch without changing the visible canvas. */
  exportReplay(): Promise<Blob>;

  /**
   * Load artwork into a layer. Hosts should await `ready` first.
   * Loading before the first user edit is the supported continuation flow.
   */
  loadImage(source: ImageSource): Promise<void>;

  /** Undo or redo through the painter's active history policy. */
  undo(): void;
  redo(): void;

  /**
   * What a pen's own gestures stand for -- Apple Pencil's double-tap and
   * squeeze, as the iOS app forwards them: the eraser and back again, or the
   * tool before this one. The same switch the toolbox makes. Standard mode
   * only; two-tone's pens have no eraser to switch to.
   */
  command(command: PainterCommand): void;

  /**
   * Fingers pan and pinch rather than draw, from the first stroke rather than
   * from the first time a pen is seen. For a host that knows a pen is the only
   * thing drawing here.
   */
  preferPen(): void;

  /** Enable or suspend pointer-driven editing without unmounting the painter. */
  setInteractionEnabled(enabled: boolean): void;

  /**
   * Adopt the identity the server assigned this connection.
   *
   * The optimistic fork and the canonical stream have to name the same actor:
   * stroke continuation, undo attribution and fork reconciliation are all
   * keyed by it, so a host that lets the two disagree splits one person in
   * two. Call this as soon as the server announces the id and before any
   * local operation -- the fork must be empty, since operations already
   * stamped with the old identity are not re-keyed.
   */
  setLocalActorId(actorId: string): void;

  /**
   * Name the participants for the layer toolbox.
   *
   * The painter knows every actor that has drawn, because their layers exist,
   * but not who they are: names and colours belong to the host's roster. Any
   * actor left unnamed is listed by its id.
   */
  setParticipants(
    participants: { actorId: string; name: string; color?: string }[],
  ): void;

  /**
   * Hide these participants' layers, and show everyone else's.
   *
   * The same switch the layers window's eye makes, for a host that shows no
   * layers window -- the session replay, which picks whose marks to look at
   * from its own list. A way of looking and not an edit: nothing is emitted,
   * and saving still composites everyone.
   */
  setHiddenParticipants(actorIds: string[]): void;

  /**
   * Place the layers window yourself.
   *
   * The painter opens it under its own columns and clear of the drawing, which
   * is right when the painter is the only thing on the page. A host with
   * windows of its own knows better -- the collaborative page stacks it under
   * the chat -- and the painter cannot see those to keep out of their way.
   */
  setLayersOrigin(origin: { x: number; y: number } | null): void;

  /** Apply a server-ordered echo or remote operation in controlled mode. */
  applyCanonicalOperation(
    operation: import("./operations").CanonicalPainterOperation,
  ): Promise<void>;

  /** Capture both editable layers at a canonical compaction boundary. */
  exportCheckpoint(sequence: number): Promise<import("./operations").PainterCheckpoint>;

  /** Replace both editable layers and reset controlled history to a checkpoint. */
  applyCheckpoint(checkpoint: import("./operations").PainterCheckpoint): Promise<void>;

  /** Export the canonical log since the last applied checkpoint. */
  exportSessionArchive(): Promise<import("./operations").PainterSessionArchive>;

  /** Compact confirmed history through a server-approved canonical sequence. */
  compactCanonicalHistory(sequence: number): Promise<void>;

  /** True when no pointer gesture or optimistic operation is outstanding. */
  isSynchronizationSettled(): boolean;

  /**
   * The recent history of what arrived and what reconciliation decided about
   * it, for attaching to a report of a canvas that came out wrong.
   *
   * The operations are recoverable from `exportSessionArchive`; the order they
   * arrived in relative to local work, and which branch each one took, is not
   * recoverable from anything after the fact. Drawpile keeps the equivalent
   * for the same reason. Empty outside controlled mode.
   */
  synchronizationTrace(): import("./utils/canvasHistory").HistoryTraceEvent[];

  /** Idempotently release listeners, canvases, controls, and framework roots. */
  unmount(): void;
}

/** The framework-neutral library shape. */
export interface NeoCucumberLibrary {
  mount(element: HTMLElement, options: PainterOptions): PainterHandle;
}

export const mount: NeoCucumberLibrary["mount"] = mountOfflinePainter;
