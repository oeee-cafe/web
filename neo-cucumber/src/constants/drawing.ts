import type { ToolId } from "../neo/tools";

// Brush size bounds, shared by the size slider and the [ / ] shortcuts
export const MIN_BRUSH_SIZE = 1;
export const MAX_BRUSH_SIZE = 30;

/**
 * Pixels of drag per brush size, for every gesture that drags a size: the
 * slider once it has been pressed, and Ctrl+Alt on the canvas.
 *
 * Seven is NEO's own slow rate -- SizeSlider.slide's `(y - y0) / 7`, which it
 * uses only once the pointer has left the slider. NEO's Ctrl+Alt drag is far
 * faster, a size per pixel, so all thirty fit in a flick; that sweeps rather
 * than tunes, and on a trackpad it could not land on a size at all. Both run
 * at seven here, so a size is found the same way wherever it is dragged.
 *
 * Only how a size is chosen differs from NEO. A size draws exactly as it
 * does there, so nothing recorded can tell.
 */
export const SIZE_DRAG_PIXELS = 7;

// Two-tone mode gives each pen its own remembered size, like Tegaki does:
// a thin foreground pen for drawing, a fat background pen for erasing.
// Indices match the two-tone palette: 0 = background, 1 = foreground.
export const TWO_TONE_BACKGROUND_PEN_INDEX = 0;
export const TWO_TONE_FOREGROUND_PEN_INDEX = 1;
export const TWO_TONE_INITIAL_PEN_SIZES = [10, 2];

export const DEFAULT_PALETTE_COLORS = [
  "#ffffff",
  "#000000",
  "#888888",
  "#b47575",
  "#c096c0",
  "#fa9696",
  "#8080ff",
  "#ffb6ff",
  "#e7e58d",
  "#25c7c9",
  "#99cb7b",
  "#e7962d",
  "#f9ddcf",
  "#fcece2",
];

// Brush initialization function
export const initializeBrushes = (): { [key: number]: Uint8Array } => {
  const brush: { [key: number]: Uint8Array } = {};
  
  // init brush
  for (let r = 1; r <= 30; r++) {
    brush[r] = new Uint8Array(r * r);
    const mask = brush[r];
    let index = 0;
    for (let x = 0; x < r; x++) {
      for (let y = 0; y < r; y++) {
        const xx = x + 0.5 - r / 2.0;
        const yy = y + 0.5 - r / 2.0;
        mask[index++] = xx * xx + yy * yy <= (r * r) / 4 ? 1 : 0;
      }
    }
  }
  
  // Special brush modifications
  brush[3][0] = 0;
  brush[3][2] = 0;
  brush[3][6] = 0;
  brush[3][8] = 0;

  brush[5][1] = 0;
  brush[5][3] = 0;
  brush[5][5] = 0;
  brush[5][9] = 0;
  brush[5][15] = 0;
  brush[5][19] = 0;
  brush[5][21] = 0;
  brush[5][23] = 0;
  
  return brush;
};

// Tone initialization function
export const initializeTones = (): { [key: string]: Uint8Array } => {
  const tone: { [key: string]: Uint8Array } = {};
  
  // Initialize tone patterns similar to Neo.Painter
  const tonePattern = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
  const toneData: Uint8Array[] = [];

  for (let i = 0; i < 16; i++) {
    const arr = new Uint8Array(16);
    for (let j = 0; j < 16; j++) {
      arr[j] = i >= tonePattern[j] ? 1 : 0;
    }
    toneData.push(arr);
  }

  // Fill the tone object for compatibility
  for (let i = 0; i < 16; i++) {
    tone[i] = toneData[i];
  }
  
  return tone;
};
/**
 * Every image-editing tool NEO dispatches in `Painter.setToolByType`.
 *
 * This deliberately includes fill and paste even though neither has a tip in
 * NEO's tool column: fill is a button above the canvas, and copy switches to
 * paste when its selection is finished. It deliberately excludes NEO's Hand
 * and Slider tools, which only operate the UI.
 */
export const NEO_DRAWING_TOOLS: readonly ToolId[] = [
  "solid",
  "eraser",
  "fill",
  "eraseAll",
  "eraseRect",
  "copy",
  "paste",
  "merge",
  "flipH",
  "flipV",
  "brush",
  "text",
  "halftone",
  "blur",
  "dodge",
  "burn",
  // Dragged out over a rectangle rather than stroked
  "rect",
  "rectFill",
  "ellipse",
  "ellipseFill",
  "blurRect",
  "turn",
];

/** Tools added for operating the modern canvas, never for editing its pixels. */
export const CONVENIENCE_TOOLS: readonly ToolId[] = [
  "pan",
];

/** Every selectable tool in neo-cucumber. */
export const ALL_TOOLS: readonly ToolId[] = [
  ...NEO_DRAWING_TOOLS,
  ...CONVENIENCE_TOOLS,
];

/**
 * What a collaborative session offers. Every drawing tool has a wire code, so
 * this is the full set; it stays a separate constant because fill and pan are
 * chrome rather than strokes and a session could still restrict tools later.
 */
export const SHARED_TOOLS: readonly ToolId[] = ALL_TOOLS;

/**
 * Tools NEO's toolbox has no button for, so ours cannot either.
 *
 * `fill` is a button above NEO's canvas rather than in its column, `paste` is
 * reached by finishing a copy, and `pan` is entirely ours. They are offered
 * beside the NEO column instead of inside it.
 */
export const NON_NEO_TOOLS: readonly ToolId[] = ["fill", "paste", "pan"];

// NEO's tool tips -- which tools each button cycles through, and in what
// order -- live in neo/toolboxSpec.ts alongside the rest of the toolbox it
// was read off. They were duplicated here as TOOL_GROUPS.
