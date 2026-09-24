import type React from "react";
import { useLayoutEffect, useState } from "react";
import { Trans, useLingui } from "@lingui/react/macro";
import { CustomSlider } from "./CustomSlider";
import { anchorBesideCanvas, minimumTop } from "./toolboxAnchor";
import { TIMER_DURATIONS_MINUTES } from "../hooks/useDrawingTimer";
import { NeoWindow } from "./neo/NeoWindow";
import {
  NEO_BUTTON,
  NEO_BUTTON_ON,
  NEO_KBD,
} from "./neo/neoClasses";
import {
  MAX_BRUSH_SIZE,
  MIN_BRUSH_SIZE,
  TWO_TONE_BACKGROUND_PEN_INDEX,
  TWO_TONE_FOREGROUND_PEN_INDEX,
} from "../constants/drawing";

interface SimplifiedToolboxProps {
  brushSize: number;
  paletteColors: string[]; // Should be [backgroundColor, foregroundColor]
  selectedPaletteIndex: number;
  canUndo: boolean;
  canRedo: boolean;
  timerMinutes: number;
  timerRemainingSeconds: number;
  onBrushSizeChange: (size: number) => void;
  onSelectPen: (index: number) => void;
  onTimerChange: (minutes: number) => void;
  onUndo: () => void;
  onRedo: () => void;
  /** The painter's area: what the panel is kept inside. */
  anchorRef?: React.RefObject<HTMLElement | null>;
  /** The drawing itself, which is what the panel opens beside. */
  canvasRef?: React.RefObject<HTMLElement | null>;
}

/** The width the panel is given below, needed before it is rendered. */
const TWO_TONE_PANEL_WIDTH = 200;

const formatRemaining = (seconds: number): string => {
  if (seconds > 60) return `${Math.ceil(seconds / 60)}m`;
  return `${seconds}s`;
};

export const SimplifiedToolbox = ({
  brushSize,
  paletteColors,
  selectedPaletteIndex,
  canUndo,
  canRedo,
  timerMinutes,
  timerRemainingSeconds,
  onBrushSizeChange,
  onSelectPen,
  onTimerChange,
  onUndo,
  onRedo,
  anchorRef,
  canvasRef,
}: SimplifiedToolboxProps) => {
  const { t } = useLingui();
  const [ceiling, setCeiling] = useState(0);
  const [opensAt, setOpensAt] = useState<{ x: number; y: number } | null>(null);
  // See ToolboxPanels: in the DOM by the end of this commit, not the next one.
  useLayoutEffect(() => {
    const area = anchorRef?.current?.getBoundingClientRect() ?? null;
    setCeiling(minimumTop(area));
    setOpensAt(
      anchorBesideCanvas(
        area,
        canvasRef?.current?.getBoundingClientRect() ?? null,
        TWO_TONE_PANEL_WIDTH,
      ),
    );
  }, [anchorRef, canvasRef]);
  const backgroundColor = paletteColors[TWO_TONE_BACKGROUND_PEN_INDEX] || "#ffffff";
  const foregroundColor = paletteColors[TWO_TONE_FOREGROUND_PEN_INDEX] || "#000000";

  const remaining = formatRemaining(timerRemainingSeconds);

  const shortcuts: [string, string][] = [
    ["X", t`Swap pens`],
    ["P / B", t`Foreground pen`],
    ["E", t`Background pen`],
    ["[ / ]", t`Pen size`],
    ["Ctrl+Z", t`Undo`],
    ["Ctrl+Y", t`Redo`],
  ];

  return (
    <NeoWindow
      initialPosition={opensAt ?? { x: 0, y: ceiling }}
      className="z-40 w-[200px] overflow-hidden"
      minimumY={ceiling}
      constrainToViewport
    >
      <div className="flex flex-col gap-[6px] p-[4px] font-[Arial] text-[12px] leading-[16px]">
      {/* Pen Size Slider */}
      <div>
        <CustomSlider
          label={`Size: ${brushSize}`}
          min={MIN_BRUSH_SIZE}
          max={MAX_BRUSH_SIZE}
          value={brushSize}
          onChange={onBrushSizeChange}
        />
      </div>

      {/* Color Picker */}
      <div className="flex flex-col gap-[3px] border-t border-t-(--neo-panel-shadow) pt-[5px]">
        <span className="text-[11px] font-bold leading-[14px]">
          <Trans>Color</Trans>
        </span>
        <div className="grid grid-cols-2 gap-[4px]">
          {/* Background Color */}
          <button
            type="button"
            title={t`Background pen`}
            aria-label={t`Background pen`}
            aria-pressed={selectedPaletteIndex === TWO_TONE_BACKGROUND_PEN_INDEX}
            // A colour, and colours are not felt (NeoColorTips).
            data-haptic="none"
            onClick={() => onSelectPen(TWO_TONE_BACKGROUND_PEN_INDEX)}
            className={`${NEO_BUTTON} ${
              selectedPaletteIndex === TWO_TONE_BACKGROUND_PEN_INDEX
                ? NEO_BUTTON_ON
                : ""
            } flex h-[38px] items-center justify-center p-[3px]`}
          >
            <div
              className="h-full w-full border border-(--neo-tool-frame)"
              style={{ backgroundColor }}
            />
          </button>

          {/* Foreground Color */}
          <button
            type="button"
            title={t`Foreground pen`}
            aria-label={t`Foreground pen`}
            aria-pressed={selectedPaletteIndex === TWO_TONE_FOREGROUND_PEN_INDEX}
            data-haptic="none"
            onClick={() => onSelectPen(TWO_TONE_FOREGROUND_PEN_INDEX)}
            className={`${NEO_BUTTON} ${
              selectedPaletteIndex === TWO_TONE_FOREGROUND_PEN_INDEX
                ? NEO_BUTTON_ON
                : ""
            } flex h-[38px] items-center justify-center p-[3px]`}
          >
            <div
              className="h-full w-full border border-(--neo-tool-frame)"
              style={{ backgroundColor: foregroundColor }}
            />
          </button>
        </div>
      </div>

      {/* Drawing Timer */}
      <div className="flex flex-col gap-[3px] border-t border-t-(--neo-panel-shadow) pt-[5px]">
        <label htmlFor="drawing-timer" className="text-[11px] font-bold leading-[14px]">
          <Trans>Timer</Trans>
        </label>
        <select
          id="drawing-timer"
          value={timerMinutes}
          onChange={(e) => onTimerChange(Number(e.target.value))}
          className={`${NEO_BUTTON} h-[22px] w-full cursor-pointer py-0`}
        >
          {TIMER_DURATIONS_MINUTES.map((minutes) => (
            <option key={minutes} value={minutes}>
              {minutes === 0 ? t`Off` : t`${minutes} min`}
            </option>
          ))}
        </select>
        <div className="text-[11px] leading-[14px]">
          {timerMinutes > 0 ? (
            <Trans>Remaining: {remaining}</Trans>
          ) : (
            <Trans>Timer off</Trans>
          )}
        </div>
      </div>

      {/* Undo/Redo Buttons */}
      <div className="grid grid-cols-2 gap-[3px] border-t border-t-(--neo-panel-shadow) pt-[5px]">
        <button
          type="button"
          onClick={onUndo}
          disabled={!canUndo}
          className={`${NEO_BUTTON} disabled:cursor-not-allowed`}
        >
          <Trans>Undo</Trans>
        </button>
        <button
          type="button"
          onClick={onRedo}
          disabled={!canRedo}
          className={`${NEO_BUTTON} disabled:cursor-not-allowed`}
        >
          <Trans>Redo</Trans>
        </button>
      </div>

      {/* Keyboard Shortcuts */}
      <div className="flex flex-col gap-[2px] border-t border-t-(--neo-panel-shadow) pt-[5px]">
        <span className="text-[11px] font-bold leading-[14px]">
          <Trans>Shortcuts</Trans>
        </span>
        <dl className="flex flex-col gap-[2px] text-[10px] leading-[13px]">
          {shortcuts.map(([keys, description]) => (
            <div key={keys} className="flex justify-between gap-2">
              <dt><kbd className={NEO_KBD}>{keys}</kbd></dt>
              <dd className="text-right">{description}</dd>
            </div>
          ))}
        </dl>
      </div>
      </div>
    </NeoWindow>
  );
};
