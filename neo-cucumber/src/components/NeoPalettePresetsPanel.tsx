import { useState } from "react";
import { useLingui } from "@lingui/react/macro";
import { NeoWindow } from "./neo/NeoWindow";
import { NEO_COLOR_INPUT, NEO_FIELD, NEO_ICON_BUTTON } from "./neo/neoClasses";
import type { ResolvedPalettePreset } from "../hooks/usePalettePresets";
import { fromNeoOrder } from "../constants/palettePresets";
import {
  applyPaletteEffect,
  gradientPalette,
  type PaletteEffect,
} from "../utils/paletteEffects";

export interface NeoPalettePresetsPanelProps {
  presets: readonly ResolvedPalettePreset[];
  /** The swatches as they are now, in display order. */
  paletteColors: readonly string[];
  onApply: (colors: string[]) => void;
  initialPosition: { x: number; y: number };
  minimumY?: number;
}

/** The window's inner width; see `paletteWindowOrigin`. */
export const PALETTE_PRESETS_WIDTH = 132;

const HEX = /^[0-9a-f]{6}$/i;

function sameColors(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((color, i) => color === b[i]);
}

const channels = (color: string) =>
  [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16));

/** POTI's `GetBright`: white text on a colour whose brightest channel is dark. */
const textOn = (color: string) =>
  Math.max(...channels(color)) < 128 ? "#ffffff" : "#000000";

/** POTI's `GradSelC`: the colour's inverse, for the swatch number over it. */
const inverse = (color: string) => applyPaletteEffect([color], "invert")[0];

/** `#rrggbb` to the six upper-case digits POTI's fields show. */
const digits = (color: string) => color.slice(1).toUpperCase();

/**
 * POTI-board's palette panel, laid out as POTI lays it out.
 *
 * A list of names under PALETTE, each painted in one of its own colours
 * (POTI takes the fifth, in NEO's order), the three effect buttons beneath it,
 * and GRADATION below that. Picking a name applies the set; the list marks a
 * name only while the swatches still are that set.
 *
 * Gradation speaks NEO's swatch numbers, 1 to 14, as POTI's does -- and so in
 * NEO's order, which zigzags across the two columns. Changing either number
 * reloads both colours from the palette, as POTI's `GetPalette` does; the
 * colours can then be typed or picked before pressing Ok.
 */
export function NeoPalettePresetsPanel({
  presets,
  paletteColors,
  onApply,
  initialPosition,
  minimumY = 0,
}: NeoPalettePresetsPanelProps) {
  const { t } = useLingui();
  // The swap is its own inverse, so this also goes from display order to NEO's.
  const neoColors = fromNeoOrder(paletteColors);

  const [startIndex, setStartIndex] = useState(0);
  const [endIndex, setEndIndex] = useState(11);
  const [startText, setStartText] = useState(() => digits(neoColors[0] ?? "#000000"));
  const [endText, setEndText] = useState(() => digits(neoColors[11] ?? "#ffffff"));

  const pickEnds = (start: number, end: number) => {
    setStartIndex(start);
    setEndIndex(end);
    setStartText(digits(neoColors[start]));
    setEndText(digits(neoColors[end]));
  };

  const applied = presets.findIndex((preset) => sameColors(preset.colors, paletteColors));

  const gradationReady = HEX.test(startText) && HEX.test(endText);

  // POTI's words, verbatim: Bright, Dark, Invert.
  const effects: { effect: PaletteEffect; label: string }[] = [
    { effect: "bright", label: t({ context: "palette effect", message: "Bright" }) },
    { effect: "dark", label: t({ context: "palette effect", message: "Dark" }) },
    { effect: "invert", label: t({ context: "palette effect", message: "Invert" }) },
  ];

  const fieldset = "m-0 min-w-0 border border-(--neo-panel-shadow) px-[3px] pt-0 pb-[3px]";
  const legend = "px-[2px] text-[10px] leading-[12px]";
  const button = `${NEO_ICON_BUTTON} min-w-0 truncate text-[11px]`;

  const swatchNumber = (value: number, onChange: (index: number) => void, label: string) => (
    <select
      value={value}
      onChange={(e) => onChange(Number(e.target.value))}
      aria-label={label}
      className={`${NEO_FIELD} h-[18px] w-[36px] shrink-0 cursor-default p-0 text-[11px]`}
    >
      {neoColors.map((color, i) => (
        <option key={i} value={i} style={{ backgroundColor: color, color: inverse(color) }}>
          {i + 1}
        </option>
      ))}
    </select>
  );

  const colorField = (
    text: string,
    setText: (text: string) => void,
    label: string,
  ) => (
    <>
      <input
        type="text"
        value={text}
        maxLength={6}
        spellCheck={false}
        onChange={(e) => setText(e.target.value.replace(/^#/, "").toUpperCase())}
        aria-label={label}
        className={`${NEO_FIELD} h-[18px] w-[52px] min-w-0 px-[2px] font-mono text-[11px]`}
      />
      <span className="block w-[22px] shrink-0">
        <input
          type="color"
          value={HEX.test(text) ? `#${text.toLowerCase()}` : "#000000"}
          onChange={(e) => setText(digits(e.target.value))}
          aria-label={label}
          className={NEO_COLOR_INPUT}
        />
      </span>
    </>
  );

  return (
    <NeoWindow
      initialPosition={initialPosition}
      className="overflow-hidden toolbox-palettes"
      minimumY={minimumY}
    >
      <div
        className="flex flex-col gap-[2px] p-[3px] text-(--neo-text)"
        style={{ width: PALETTE_PRESETS_WIDTH }}
      >
        <fieldset className={fieldset}>
          <legend className={legend}>
            {t({ context: "palette window", message: "PALETTE" })}
          </legend>
          {/*
            Drawn rather than a native list box, which cannot be styled far
            enough: each name is painted in its set's colour, and that paint
            covers the browser's selection highlight entirely; and options
            take their height from the font, so Japanese names, which fall back
            to a taller face, gave rows of different heights.
          */}
          <div
            role="listbox"
            aria-label={t`Palettes`}
            className={`${NEO_FIELD} block max-h-[422px] cursor-default overflow-y-auto p-px`}
          >
            {presets.map((preset, index) => {
              const color = fromNeoOrder(preset.colors)[4];
              const text = textOn(color);
              const selected = index === applied;
              return (
                <div
                  key={index}
                  role="option"
                  aria-selected={selected}
                  onClick={() => onApply(preset.colors)}
                  title={preset.name}
                  className="h-[14px] truncate px-[3px] text-[11px] leading-[14px]"
                  style={{
                    backgroundColor: color,
                    color: text,
                    // An inset ring in the text's own colour, which is chosen
                    // to read against this background.
                    boxShadow: selected ? `inset 0 0 0 1px ${text}` : undefined,
                  }}
                >
                  {preset.name}
                </div>
              );
            })}
          </div>
          {/* They act on the swatches as they are now, preset or not. */}
          <div className="mt-[3px] grid grid-cols-3 gap-[2px]">
            {effects.map(({ effect, label }) => (
              <button
                key={effect}
                type="button"
                onClick={() => onApply(applyPaletteEffect(paletteColors, effect))}
                className={button}
              >
                {label}
              </button>
            ))}
          </div>
        </fieldset>

        <fieldset className={fieldset}>
          <legend className={legend}>
            {t({ context: "palette window", message: "GRADATION" })}
          </legend>
          <div className="flex flex-col gap-[2px]">
            <div className="flex items-center gap-[2px]">
              {swatchNumber(startIndex, (i) => pickEnds(i, endIndex), t`Gradation start`)}
              {colorField(startText, setStartText, t`Gradation start`)}
            </div>
            <div className="flex items-center gap-[2px]">
              {swatchNumber(endIndex, (i) => pickEnds(startIndex, i), t`Gradation end`)}
              {colorField(endText, setEndText, t`Gradation end`)}
            </div>
            <button
              type="button"
              disabled={!gradationReady}
              onClick={() =>
                onApply(fromNeoOrder(gradientPalette(`#${startText}`, `#${endText}`)))
              }
              className={button}
            >
              {t({ context: "palette window", message: "Ok" })}
            </button>
          </div>
        </fieldset>
      </div>
    </NeoWindow>
  );
}
