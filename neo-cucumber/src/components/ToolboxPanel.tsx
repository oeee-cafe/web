import React from "react";
import { Trans, useLingui } from "@lingui/react/macro";
import { Icon } from "./Icon";
import { NeoToolColumn } from "./neo/NeoToolColumn";
import {
  NEO_BUTTON_ON,
  NEO_COLOR_INPUT,
  NEO_ICON_BUTTON,
  NEO_PANEL_BUTTON,
} from "./neo/neoClasses";
import { NeoWindow } from "./neo/NeoWindow";
import { type ParticipantLayer } from "./neo/NeoParticipantLayers";
import { useTheme } from "../hooks/useTheme";
import { usePressRepeat } from "../hooks/usePressRepeat";

import { NON_NEO_TOOLS } from "../constants/drawing";
import { usePainterLabels } from "../hooks/usePainterLabels";
import type { ToolId } from "../neo/tools";

/**
 * Icons for the three tools NEO has no artwork for, since they have no button
 * in its toolbox to carry one.
 */
const NON_NEO_TOOL_ICONS: Record<string, string> = {
  fill: "material-symbols:format-color-fill",
  pan: "material-symbols:pan-tool",
};
import type { DrawingState } from "../types/drawing";

interface HistoryState {
  canUndo: boolean;
  canRedo: boolean;
}

export interface ToolboxPanelProps {
  /**
   * Which half to show. NEO's toolbox holds tools, colour and layers and
   * nothing else, so our own controls -- undo, zoom, flip, save -- can be
   * split into a second panel rather than sitting in a column claiming to be
   * NEO's. "all" puts both in one panel.
   */
  section?: "all" | "neo" | "extras";
  drawingState: DrawingState;
  historyState: HistoryState;
  paletteColors: string[];
  selectedPaletteIndex: number;
  currentZoom: number;
  isOwner: boolean;
  /** Tools to offer; defaults to everything the painter has. */
  tools: readonly ToolId[];
  isSaving: boolean;
  sessionEnded: boolean;
  onUndo: () => void;
  onRedo: () => void;
  onUpdateBrushType: (type: ToolId) => void;
  onUpdateDrawingState: React.Dispatch<React.SetStateAction<DrawingState>>;
  onUpdateColor: (color: string) => void;
  onSetSelectedPaletteIndex: (index: number) => void;
  onSetPaletteColor: (index: number, color: string) => void;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onZoomReset: () => void;
  onZoomFit: () => void;
  onSaveCollaborativeDrawing: () => void;
  /** NEO's Right Click button: whether the next press is a right press. */
  virtualRight?: boolean;
  onToggleVirtualRight?: () => void;
  /**
   * Everyone with a layer pair, top of the stack first. Absent outside a
   * collaborative session, where there is only ever one participant.
   */
  participantLayers?: ParticipantLayer[];
  hiddenOwners?: ReadonlySet<string>;
  targetOwner?: string;
  localActorId?: string;
  onToggleOwnerVisible?: (actorId: string) => void;
  onSelectTargetOwner?: (actorId: string) => void;
  initialPosition?: { x: number; y: number };
  /** How high the panel may be dragged. See `minimumTop`. */
  minimumY?: number;
}

/**
 * The floating window NEO's tool column lives in.
 *
 * NEO itself docks its column to the canvas rather than floating it, but our
 * painter has to sit inside a page it does not own, so the column travels in a
 * draggable panel. Everything inside the panel is NEO's; the panel is ours.
 */
export const ToolboxPanel = ({
  drawingState,
  historyState,
  paletteColors,
  selectedPaletteIndex,
  currentZoom,
  isOwner,
  tools,
  isSaving,
  sessionEnded,
  onUndo,
  onRedo,
  onUpdateBrushType,
  onUpdateDrawingState,
  onUpdateColor,
  onSetSelectedPaletteIndex,
  onSetPaletteColor,
  onZoomIn,
  onZoomOut,
  onZoomReset,
  onZoomFit,
  onSaveCollaborativeDrawing,
  virtualRight,
  onToggleVirtualRight,
  section = "all",
  initialPosition,
  minimumY = 0,
}: ToolboxPanelProps) => {
  const { t } = useLingui();
  const labels = usePainterLabels();
  const { theme, toggle: toggleTheme } = useTheme();
  const zoomOutPress = usePressRepeat(onZoomOut);
  const zoomInPress = usePressRepeat(onZoomIn);

  const showNeo = section !== "extras";
  const showExtras = section !== "neo";

  return (
    <NeoWindow
      initialPosition={initialPosition ?? { x: 304, y: 70 }}
      // NEO's own labels overflow their buttons -- its English toolbox reads
      // "Halftone" spilling past the edge -- and that spill has to stop at the
      // panel rather than land on the canvas.
      className={`w-max overflow-hidden toolbox-${section}`}
      minimumY={minimumY}
    >
      <div className="flex flex-col gap-[2px] p-[2px]">
        {showNeo && (
          <NeoToolColumn
            drawingState={drawingState}
            paletteColors={paletteColors}
            selectedPaletteIndex={selectedPaletteIndex}
            tools={tools}
            onUpdateDrawingState={onUpdateDrawingState}
            onUpdateBrushType={onUpdateBrushType}
            onUpdateColor={onUpdateColor}
            onSetSelectedPaletteIndex={onSetSelectedPaletteIndex}
            onSetPaletteColor={onSetPaletteColor}
          />
        )}


        {showExtras && (
          <div className="flex w-[50px] flex-col gap-[2px]">
            {/*
              Everything NEO keeps outside its toolbox, in the same width the
              NEO column paints in so the two panels line up. Every row is the
              same two-column grid so the buttons line up too: a flex row will
              not shrink a button below its icon, and two of those overflowed
              the column and widened the whole panel.
            */}

            {/* View navigation: pan, fit, zoom, reset, and flip. */}
            <div className="grid grid-cols-2 gap-[2px]">
              {tools.includes("pan") && (
                <>
                  <button
                    type="button"
                    onClick={() => onUpdateBrushType("pan")}
                    aria-pressed={drawingState.brushType === "pan"}
                    title={labels.tools.pan}
                    className={`${NEO_ICON_BUTTON} ${
                      drawingState.brushType === "pan" ? NEO_BUTTON_ON : ""
                    } flex items-center justify-center`}
                  >
                    <Icon icon={NON_NEO_TOOL_ICONS.pan} width={14} height={14} />
                  </button>
                  <button
                    type="button"
                    onClick={onZoomFit}
                    title={t`Fit canvas`}
                    aria-label={t`Fit canvas`}
                    className={`${NEO_ICON_BUTTON} flex items-center justify-center`}
                  >
                    <Icon
                      icon="material-symbols:fit-screen"
                      width={14}
                      height={14}
                    />
                  </button>
                </>
              )}
              <button
                type="button"
                {...zoomOutPress}
                title={t`Zoom out`}
                className={`${NEO_ICON_BUTTON} flex items-center justify-center`}
              >
                <Icon icon="material-symbols:zoom-out" width={14} height={14} />
              </button>
              <button
                type="button"
                {...zoomInPress}
                title={t`Zoom in`}
                className={`${NEO_ICON_BUTTON} flex items-center justify-center`}
              >
                <Icon icon="material-symbols:zoom-in" width={14} height={14} />
              </button>
              <button
                type="button"
                onClick={onZoomReset}
                title={t`Reset zoom`}
                className={`${NEO_ICON_BUTTON} min-w-0 overflow-hidden text-center text-[9px] leading-[14px] tabular-nums`}
              >
                {Math.round(currentZoom * 100)}%
              </button>
              <button
                type="button"
                onClick={() =>
                  onUpdateDrawingState((prev) => ({
                    ...prev,
                    isFlippedHorizontal: !prev.isFlippedHorizontal,
                  }))
                }
                title={t`Toggle horizontal flip`}
                className={`${NEO_ICON_BUTTON} ${
                  drawingState.isFlippedHorizontal ? NEO_BUTTON_ON : ""
                } flex items-center justify-center`}
              >
                <Icon icon="material-symbols:flip" width={14} height={14} />
              </button>
            </div>

            {/*
              A colour picker, which NEO does not have -- its swatches and
              RGBA sliders are how it mixes a colour. This sets the drawing
              colour only; right-clicking a swatch is still what stores it.
            */}
            <input
              type="color"
              value={drawingState.color}
              onChange={(e) => onUpdateColor(e.target.value)}
              title={t`Pick a colour`}
              aria-label={t`Pick a colour`}
              className={NEO_COLOR_INPUT}
            />

            {/* Undo and redo. NEO puts these in the bar above the canvas. */}
            <div className="grid grid-cols-2 gap-[2px]">
              <button
                type="button"
                onClick={onUndo}
                disabled={!historyState.canUndo}
                title={t`Undo`}
                className={`${NEO_ICON_BUTTON} flex items-center justify-center`}
              >
                <Icon icon="material-symbols:undo" width={14} height={14} />
              </button>
              <button
                type="button"
                onClick={onRedo}
                disabled={!historyState.canRedo}
                title={t`Redo`}
                className={`${NEO_ICON_BUTTON} flex items-center justify-center`}
              >
                <Icon icon="material-symbols:redo" width={14} height={14} />
              </button>
            </div>

            {/*
              Fill and NEO's Right Click button, under undo and redo: the four
              buttons NEO keeps together in the bar above its canvas
              (container.js: redo, undo, fill, right). Right Click says the
              next press is a right press -- which takes the colour under the
              pointer instead of drawing -- for a tablet or trackpad with no
              way to make one, and releases itself once that press happens.

              Paste used to sit beside fill. NEO has no paste button, so it
              went, and the space it left is filled with a neighbour NEO
              actually gives fill rather than with something new. A tool
              alone in this row takes the whole row, so a host that offers
              no Right Click does not get the gap back.
            */}
            {(() => {
              const cells = [
                ...NON_NEO_TOOLS.filter(
                  (tool) => tool !== "pan" && tool !== "paste" && tools.includes(tool)
                ).map((tool) => (
                  <button
                    key={tool}
                    type="button"
                    onClick={() => onUpdateBrushType(tool)}
                    aria-pressed={drawingState.brushType === tool}
                    title={labels.tools[tool] ?? tool}
                    className={`${NEO_ICON_BUTTON} ${
                      drawingState.brushType === tool ? NEO_BUTTON_ON : ""
                    } flex items-center justify-center`}
                  >
                    <Icon icon={NON_NEO_TOOL_ICONS[tool]} width={14} height={14} />
                  </button>
                )),
                ...(onToggleVirtualRight
                  ? [
                      <button
                        key="right"
                        type="button"
                        onClick={onToggleVirtualRight}
                        aria-pressed={virtualRight ?? false}
                        title={t`Pick a colour from the drawing with the next click`}
                        aria-label={t`Right click`}
                        className={`${NEO_ICON_BUTTON} ${virtualRight ? NEO_BUTTON_ON : ""} flex items-center justify-center`}
                      >
                        <Icon icon="material-symbols:colorize" width={14} height={14} />
                      </button>,
                    ]
                  : []),
              ];
              if (cells.length === 0) return null;
              return (
                <div className="grid grid-cols-2 gap-[2px]">
                  {cells.length === 1 ? (
                    <div className="col-span-2 grid">{cells[0]}</div>
                  ) : (
                    cells
                  )}
                </div>
              );
            })()}

            {/*
              Light and dark. The palette follows the OS on its own, so this
              is for wanting the painter dark on a light desktop or the other
              way round -- and it is in this panel because NEO has no such
              thing to reproduce.
            */}
            <button
              type="button"
              onClick={toggleTheme}
              title={theme === "dark" ? t`Switch to light` : t`Switch to dark`}
              className={NEO_PANEL_BUTTON}
            >
              <Icon
                icon={
                  theme === "dark"
                    ? "material-symbols:light-mode"
                    : "material-symbols:dark-mode"
                }
                width={12}
                height={12}
              />
              {/* Labelled, not just an icon: as the ninth glyph in a stack of
                  identical buttons it was effectively invisible. */}
              <span className="text-[10px] leading-none">
                {theme === "dark" ? <Trans>Light</Trans> : <Trans>Dark</Trans>}
              </span>
            </button>

            {isOwner && (
              <button
                type="button"
                onClick={onSaveCollaborativeDrawing}
                disabled={isSaving || sessionEnded}
                title={t`Save drawing to gallery`}
                className={NEO_PANEL_BUTTON}
              >
                {isSaving ? (
                  <Icon
                    icon="material-symbols:refresh"
                    width={14}
                    height={14}
                    className="animate-spin"
                  />
                ) : (
                  <span className="text-[11px]">
                    <Trans>Save</Trans>
                  </span>
                )}
              </button>
            )}
          </div>
        )}

      </div>
    </NeoWindow>
  );
};
