import React, { useLayoutEffect, useState } from "react";
import { ToolboxPanel, type ToolboxPanelProps } from "./ToolboxPanel";
import { NeoLayersPanel } from "./NeoLayersPanel";
import {
  NeoPalettePresetsPanel,
  PALETTE_PRESETS_WIDTH,
} from "./NeoPalettePresetsPanel";
import type { ResolvedPalettePreset } from "../hooks/usePalettePresets";
import { windowBounds } from "../utils/windowDrag";
import {
  anchorTo,
  minimumTop,
  PANEL_MARGIN,
  PANEL_PITCH,
  type PanelPositions,
} from "./toolboxAnchor";

/**
 * The painter's two floating control panels: NEO's column and our extra
 * controls. They deliberately have no shared resize behaviour. Once opened,
 * each is its own draggable window and clamps itself into the viewport.
 */
export interface ToolboxPanelsProps
  extends Omit<
    ToolboxPanelProps,
    | "section"
    | "initialPosition"
    | "minimumY"
    | "palettePresetsOpen"
    | "onTogglePalettePresets"
  > {
  /** The painter's area: what the panels are kept inside. */
  anchorRef?: React.RefObject<HTMLElement | null>;
  /** The drawing itself, which is what they open beside. */
  canvasRef?: React.RefObject<HTMLElement | null>;
  /** Overrides the opening positions entirely. */
  origin?: { x: number; y: number };
  /**
   * Where the layers window opens, when the host would rather say.
   *
   * The painter places it under its own columns and clear of the drawing,
   * which is right when the painter is the only thing on the page. A host with
   * windows of its own knows better -- the collaborative page stacks it under
   * the chat -- and the painter cannot see those to avoid them.
   */
  layersOrigin?: { x: number; y: number };
  /** Paints a participant's layers into a thumbnail for their row. */
  drawThumbnail?: (actorId: string, target: HTMLCanvasElement) => void;
  /** The drawing's shape, so the thumbnails share it. */
  canvasAspect?: number;
  /** Whole palettes to offer; none leaves the button out. */
  palettePresets?: readonly ResolvedPalettePreset[];
  /** Replaces all fourteen swatches, as `Neo.setColors` does. */
  onApplyPalette?: (colors: string[]) => void;
}

/**
 * Where the palettes window opens: beside the two columns, on the side away
 * from the drawing if there is room, since the point of opening it is to
 * watch the swatches and the drawing change together.
 *
 * Measured against both columns rather than the extras one it is opened from,
 * which would put it on top of NEO's swatches when the columns stand side by
 * side -- the very swatches it recolours.
 */
function paletteWindowOrigin(): { x: number; y: number } {
  const bounds = windowBounds();
  const columns = [".toolbox-neo", ".toolbox-extras"]
    .map((selector) => document.querySelector(selector)?.getBoundingClientRect())
    .filter((rect): rect is DOMRect => rect !== undefined);
  if (columns.length === 0) return { x: PANEL_MARGIN, y: PANEL_MARGIN };
  const left = Math.min(...columns.map((rect) => rect.left));
  const right = Math.max(...columns.map((rect) => rect.right));
  const top = Math.min(...columns.map((rect) => rect.top));
  const width = PALETTE_PRESETS_WIDTH + 8;
  const onLeft = (left + right) / 2 < bounds.width / 2;
  const outside = onLeft ? left - PANEL_MARGIN - width : right + PANEL_MARGIN;
  const inside = onLeft ? right + PANEL_MARGIN : left - PANEL_MARGIN - width;
  const fits = (x: number) => x >= 0 && x + width <= bounds.width;
  const x = fits(outside)
    ? outside
    : fits(inside)
      ? inside
      : Math.max(0, bounds.width - width);
  return { x, y: top };
}

export function ToolboxPanels({
  anchorRef,
  canvasRef,
  origin,
  layersOrigin,
  drawThumbnail,
  canvasAspect,
  palettePresets,
  onApplyPalette,
  ...shared
}: ToolboxPanelsProps) {
  /** Where the palettes window is, when it is open. */
  const [paletteOrigin, setPaletteOrigin] = useState<{ x: number; y: number } | null>(
    null
  );
  const offerPresets =
    palettePresets !== undefined && palettePresets.length > 0 && onApplyPalette !== undefined;
  const togglePalettePresets = offerPresets
    ? () => setPaletteOrigin((open) => (open ? null : paletteWindowOrigin()))
    : undefined;

  const [positions, setPositions] = useState<PanelPositions | null>(
    origin
      ? {
          neo: origin,
          extras: { x: origin.x + PANEL_PITCH, y: origin.y },
        }
      : null
  );

  /** As high as either panel may be dragged; see `minimumTop`. */
  const [ceiling, setCeiling] = useState(0);
  /**
   * Where the layers window opens: under the extras column.
   *
   * Measured rather than guessed, because the extras column is as tall as the
   * tools it was given and a constant would leave a gap under a short one and
   * cover a tall one.
   */
  const [layersTop, setLayersTop] = useState<number | null>(null);

  // Laid out rather than deferred: the panels have to be in the DOM by the end
  // of this commit, because that is when the painter reports itself ready and a
  // host may go looking for them.
  useLayoutEffect(() => {
    const area = anchorRef?.current?.getBoundingClientRect() ?? null;
    setCeiling(minimumTop(area));
    if (origin) return;
    setPositions(anchorTo(area, canvasRef?.current?.getBoundingClientRect()));
  }, [anchorRef, canvasRef, origin]);

  const [layersLeft, setLayersLeft] = useState<number | null>(null);

  useLayoutEffect(() => {
    if (!positions) return;
    if (layersOrigin) {
      setLayersLeft(layersOrigin.x);
      setLayersTop(layersOrigin.y);
      return;
    }
    const extrasHeight =
      document.querySelector(".toolbox-extras")?.getBoundingClientRect().height ?? 0;
    const width =
      document.querySelector(".toolbox-layers")?.getBoundingClientRect().width ?? 0;
    const area = anchorRef?.current?.getBoundingClientRect() ?? null;
    const canvas = canvasRef?.current?.getBoundingClientRect() ?? null;
    // Under the columns it hangs from, wherever those ended up.
    const stacked = positions.extras.y + extrasHeight + PANEL_MARGIN;

    if (!area || !canvas || width === 0) {
      setLayersTop(stacked);
      setLayersLeft(positions.extras.x);
      return;
    }

    // This window is three times the width of the columns, so the side they
    // are on may have no room for it. Take a side that does, preferring
    // theirs; if neither fits, go under the drawing rather than over it.
    const leftSlot = canvas.left - PANEL_MARGIN - width;
    const rightSlot = canvas.right + PANEL_MARGIN;
    const fitsLeft = leftSlot >= area.left + PANEL_MARGIN;
    const fitsRight = rightSlot + width <= area.right - PANEL_MARGIN;
    const columnsOnLeft = positions.extras.x < canvas.left;
    const preferred = columnsOnLeft
      ? (fitsLeft ? leftSlot : fitsRight ? rightSlot : null)
      : (fitsRight ? rightSlot : fitsLeft ? leftSlot : null);

    if (preferred === null) {
      setLayersLeft(area.left + PANEL_MARGIN);
      setLayersTop(canvas.bottom + PANEL_MARGIN);
      return;
    }
    setLayersLeft(preferred);
    setLayersTop(stacked);
  }, [positions, anchorRef, canvasRef, layersOrigin, shared.participantLayers]);

  if (!positions) return null;

  return (
    <>
      <ToolboxPanel
        {...shared}
        section="neo"
        initialPosition={positions.neo}
        minimumY={ceiling}
      />
      <ToolboxPanel
        {...shared}
        section="extras"
        initialPosition={positions.extras}
        minimumY={ceiling}
        palettePresetsOpen={paletteOrigin !== null}
        onTogglePalettePresets={togglePalettePresets}
      />
      {offerPresets && paletteOrigin && (
        <NeoPalettePresetsPanel
          presets={palettePresets}
          paletteColors={shared.paletteColors}
          onApply={onApplyPalette}
          initialPosition={paletteOrigin}
          minimumY={ceiling}
        />
      )}
      {shared.participantLayers &&
        shared.participantLayers.length > 0 &&
        layersTop !== null &&
        layersLeft !== null && (
          <NeoLayersPanel
            participants={shared.participantLayers}
            hidden={shared.hiddenOwners ?? new Set()}
            target={shared.targetOwner ?? ""}
            localActorId={shared.localActorId ?? ""}
            onToggleVisible={shared.onToggleOwnerVisible ?? (() => {})}
            onSelectTarget={shared.onSelectTargetOwner ?? (() => {})}
            initialPosition={{ x: layersLeft, y: layersTop }}
            minimumY={ceiling}
            drawThumbnail={drawThumbnail}
            canvasAspect={canvasAspect}
          />
        )}
    </>
  );
}
