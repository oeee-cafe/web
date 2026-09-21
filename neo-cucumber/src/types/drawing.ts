import type { DrawType, ToolId } from "../neo/tools";

/** Everything the engine can rasterise, plus the two non-drawing tools. */
export type BrushType =
  | "solid"
  | "brush"
  | "halftone"
  | "eraser"
  | "dodge"
  | "burn"
  | "blur"
  | "fill"
  | "pan";

/** Brush types that can be represented as stroke operations. */
export type WireBrushType =
  | "solid"
  | "halftone"
  | "eraser"
  | "brush"
  | "dodge"
  | "burn"
  | "blur";

export const WIRE_BRUSH_TYPES: readonly WireBrushType[] = [
  "solid",
  "halftone",
  "eraser",
  "brush",
  "dodge",
  "burn",
  "blur",
];

export function isWireBrushType(type: string): type is WireBrushType {
  return (WIRE_BRUSH_TYPES as readonly string[]).includes(type);
}

export type LayerType = "foreground" | "background";

export interface DrawingState {
  brushSize: number;
  opacity: number;
  color: string;
  brushType: ToolId;
  drawType?: DrawType;
  maskType?: number;
  maskColor?: string;
  layerType: LayerType;
  zoomLevel: number;
  fgVisible: boolean;
  bgVisible: boolean;
  isFlippedHorizontal: boolean;
}
