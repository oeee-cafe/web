import React, { useState } from "react";
import { NeoColorSliders } from "./NeoColorSliders";
import { NeoColorTips } from "./NeoColorTips";
import { NeoLayerControl } from "./NeoLayerControl";
import { NeoReserveControl } from "./NeoReserveControl";
import { NeoSizeSlider } from "./NeoSizeSlider";
import { NeoToolSet } from "./NeoToolSet";
import { NEO_COLUMN } from "./neoClasses";
import { NEO_DEFAULT_RESERVES, type NeoReserve } from "../../neo/toolboxSpec";
import type { DrawingState } from "../../types/drawing";
import type { DrawType, ToolId } from "../../neo/tools";

interface NeoToolColumnProps {
  drawingState: DrawingState;
  paletteColors: string[];
  selectedPaletteIndex: number;
  /** Which tools this mode offers. */
  tools: readonly ToolId[];
  onUpdateDrawingState: React.Dispatch<React.SetStateAction<DrawingState>>;
  onUpdateBrushType: (tool: ToolId) => void;
  onUpdateColor: (color: string) => void;
  onSetSelectedPaletteIndex: (index: number) => void;
  onSetPaletteColor: (index: number, color: string) => void;
  /** Whether the toolbox's sticky right-click button is armed. */
  virtualRight?: boolean;
  /** Called once an armed press has been spent, so the button can release. */
  onVirtualRightUsed?: () => void;
}

/**
 * NEO's tool column, whole.
 *
 * The order and spacing are `createContainer`'s: seven tool tips, the fourteen
 * swatches, four channel sliders, the size slider, three reserves and the
 * layer button. Nothing else -- undo, zoom and saving live outside it in NEO
 * too, in the bar above the canvas and the footer, so they stay outside here.
 *
 * Every drawing mode that is not two-tone mounts this, which is why it takes
 * state and callbacks rather than reaching for a hook of its own.
 */
export function NeoToolColumn({
  drawingState,
  paletteColors,
  selectedPaletteIndex,
  tools,
  onUpdateDrawingState,
  onUpdateBrushType,
  onUpdateColor,
  onSetSelectedPaletteIndex,
  onSetPaletteColor,
  virtualRight,
  onVirtualRightUsed,
}: NeoToolColumnProps) {
  /**
   * The three tool memories. They are UI state rather than drawing state --
   * nothing downstream reads them, they only write into it -- so they live
   * here and each mode gets its own set, as NEO's do.
   */
  const [reserves, setReserves] = useState<NeoReserve[]>(() =>
    NEO_DEFAULT_RESERVES.map((r) => ({ ...r }))
  );

  const loadReserve = (index: number) => {
    const reserve = reserves[index];
    if (!reserve) return;
    onUpdateBrushType(reserve.tool);
    onUpdateColor(reserve.color.toLowerCase());
    onUpdateDrawingState((prev) => ({
      ...prev,
      brushSize: reserve.size,
      opacity: Math.round(reserve.alpha * 255),
      // ReserveControl.load restores the draw type only for the freehand-ish
      // tools, since it is meaningless for the rest
      drawType: ["solid", "brush", "halftone"].includes(reserve.tool)
        ? reserve.drawType
        : prev.drawType,
    }));
  };

  const saveReserve = (index: number) => {
    setReserves((prev) =>
      prev.map((reserve, i) =>
        i === index
          ? {
              size: drawingState.brushSize,
              color: drawingState.color,
              alpha: drawingState.opacity / 255,
              tool: drawingState.brushType,
              drawType: drawingState.drawType ?? "freehand",
            }
          : reserve
      )
    );
  };

  return (
    <div className={NEO_COLUMN}>
      <NeoToolSet
        brushType={drawingState.brushType}
        drawType={drawingState.drawType ?? "freehand"}
        maskType={drawingState.maskType ?? 0}
        color={drawingState.color}
        alpha={drawingState.opacity}
        maskColor={drawingState.maskColor ?? "#000000"}
        tools={tools}
        onSelectTool={onUpdateBrushType}
        onSelectDrawType={(drawType: DrawType) =>
          onUpdateDrawingState((prev) => ({ ...prev, drawType }))
        }
        onSelectMaskType={(maskType) =>
          onUpdateDrawingState((prev) => ({ ...prev, maskType }))
        }
        onAdoptMaskColor={() =>
          onUpdateDrawingState((prev) => ({ ...prev, maskColor: prev.color }))
        }
      />

      <NeoColorTips
        paletteColors={paletteColors}
        selectedPaletteIndex={selectedPaletteIndex}
        onSelect={(index, color) => {
          onSetSelectedPaletteIndex(index);
          onUpdateColor(color);
        }}
        onOverwrite={(index) => onSetPaletteColor(index, drawingState.color)}
      />

      {/*
        NEO picks colour with four channel sliders rather than a picker, and
        alpha is one of them -- so this is the opacity control too.
      */}
      <NeoColorSliders
        color={drawingState.color}
        alpha={drawingState.opacity}
        onChange={(color, alpha) => {
          onUpdateColor(color);
          onUpdateDrawingState((prev) => ({ ...prev, opacity: alpha }));
        }}
      />

      <NeoSizeSlider
        value={drawingState.brushSize}
        color={drawingState.color}
        onChange={(brushSize) =>
          onUpdateDrawingState((prev) => ({ ...prev, brushSize }))
        }
      />

      <NeoReserveControl
        reserves={reserves}
        onLoad={loadReserve}
        onSave={saveReserve}
        virtualRight={virtualRight}
        onVirtualRightUsed={onVirtualRightUsed}
      />

      <NeoLayerControl
        current={drawingState.layerType}
        fgVisible={drawingState.fgVisible}
        bgVisible={drawingState.bgVisible}
        onSwitch={() =>
          onUpdateDrawingState((prev) => ({
            ...prev,
            layerType:
              prev.layerType === "foreground" ? "background" : "foreground",
          }))
        }
        onToggleVisible={() =>
          onUpdateDrawingState((prev) =>
            prev.layerType === "foreground"
              ? { ...prev, fgVisible: !prev.fgVisible }
              : { ...prev, bgVisible: !prev.bgVisible }
          )
        }
      />
    </div>
  );
}
