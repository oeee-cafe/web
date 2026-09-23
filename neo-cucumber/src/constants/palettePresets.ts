/**
 * Preset palettes: whole sets of fourteen swatches, swapped in at once.
 *
 * NEO has no such thing. The boards that host it do -- POTI-board lists the
 * sets its admin keeps in `palette.txt` beside the painter, and applies one
 * with `Neo.setColors`, which recolours the swatches and nothing else: the
 * pressed swatch stays pressed and the drawing colour stays what it was.
 *
 * Colours here are in NEO's order, the order `getColors` and `setColors`
 * speak and `palette.txt` is written in, so a set copied off a board works
 * unchanged. Our own palette is in display order; see `fromNeoOrder`.
 */
import { msg } from "@lingui/core/macro";
import type { MessageDescriptor } from "@lingui/core";
import { NEO_PALETTE_ORDER } from "../neo/toolboxSpec";

/** A named set of fourteen colours, in NEO's order. */
export interface PalettePreset {
  name: string;
  /** Fourteen `#rrggbb` colours; the `#` may be left off, as `palette.txt` does. */
  colors: readonly string[];
}

export const PALETTE_SIZE = 14;

/**
 * NEO's order to ours.
 *
 * NEO writes its swatches two to a row as `color2, color1`, `color4, color3`
 * (see `NEO_PALETTE_ORDER`), so each pair is swapped. Getting this wrong puts
 * every preset on screen transposed, which is easy to miss in a set whose
 * neighbours are shades of each other.
 */
export function fromNeoOrder(colors: readonly string[]): string[] {
  return colors.map((_, i) => colors[i % 2 ? i - 1 : i + 1]);
}

const HEX = /^#?([0-9a-f]{6})$/i;

/**
 * A preset's colours in display order and in the form our palette stores them:
 * lower-case `#rrggbb`, the form `<input type="color">` hands back, so that a
 * colour picked there still finds its swatch.
 *
 * Returns null for anything that is not exactly fourteen colours.
 */
export function presetToPalette(colors: readonly string[]): string[] | null {
  if (colors.length !== PALETTE_SIZE) return null;
  const normalized: string[] = [];
  for (const color of colors) {
    const match = HEX.exec(color.trim());
    if (!match) return null;
    normalized.push(`#${match[1].toLowerCase()}`);
  }
  return fromNeoOrder(normalized);
}

/**
 * The sets POTI-board ships in `palette.txt`, and NEO's own palette first so
 * there is always a way back to it.
 *
 * Their names are POTI's, verbatim: the English from the English edition's
 * `palette.txt`, lower case and all, and the Japanese from the original's.
 * POTI has no Korean or Chinese, so those catalogs translate the English.
 */
export const BUILT_IN_PALETTE_PRESETS: readonly {
  name: MessageDescriptor;
  colors: readonly string[];
}[] = [
  {
    name: msg({ context: "palette preset", message: "Default" }),
    colors: NEO_PALETTE_ORDER,
  },
  {
    name: msg({ context: "palette preset", message: "skin" }),
    colors: "FFF0DC,FFE7D0,FFD6C0,FFCBB3,FFC0A3,FFB7A2,52443C,5E3920,B06A54,C07A64,DEA197,ECA385,000000,FFFFFF".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "red" }),
    colors: "FFEEF7,FFCAE4,FF9DCE,FF6AB5,FF2894,CF1874,FFE6E6,FFC4C4,FF7D7D,FF5151,FF0000,BF0000,851B53,800000".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "orange" }),
    colors: "FFE3D7,FFCBB3,FFA275,FF8040,FF5F11,DB4700,FFFFDD,FFFFA2,FFFF00,D9D900,AAAA00,7D7D00,BD3000,606000".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "green" }),
    colors: "C6FDD9,8EF09F,62D99D,1DB67C,1A8C5F,136246,E8FACD,B9E97E,9ADC65,65B933,4F8729,2B6824,0F3E2B,004000".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "blue" }),
    colors: "DFF4FF,80C6FF,60A8FF,1D56DC,273D8F,1C2260,C1FFFF,6DEEFC,44D0EE,209CCC,2C769A,295270,000040,003146".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "purple" }),
    colors: "E9D2FF,DAB5FF,CE9DFF,B366FF,9428FF,6900D2,E1E1FF,C1C1FF,8080FF,6262FF,3D44C9,33309E,3F007D,252D6B".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "sepia" }),
    colors: "ECD3BD,E4C098,C8A07D,896952,825444,5E4435,F7E2BD,DBC7AC,D9B571,C09450,AE7B3E,8E5C2F,493830,5F492C".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "character" }),
    colors: "FFEADD,FFCAAB,F19D71,52443C,5BADFF,0077D9,DED8F5,9C89C4,CF434A,F09450,FDF666,4AA683,000000,FFFFFF".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "pastel" }),
    colors: "F6CD8A,89CA9D,8DCFF4,9595C6,AE88B8,F49F9B,FFF99D,C7E19E,8CCCCA,94AAD6,9681B7,F4A0BD,8C6636,FFFFFF".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "grass" }),
    colors: "C7E19E,A8D59D,7DC622,528413,00B03B,007524,D1E1FF,8DCFE0,00A49E,CBB99C,766455,5B3714,0F0F0F,FFFFFF".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "sakura" }),
    colors: "FFFF80,EE9C00,C45914,FEE7DB,FFC89D,ECA385,F4C1D4,F4BDB0,ED6B9E,E76568,BD3131,AE687E,0F0F0F,FFFFFF".split(","),
  },
  {
    name: msg({ context: "palette preset", message: "grayscale" }),
    colors: "FFFFFF,EFEFEF,DFDFDF,CFCFCF,BFBFBF,AFAFAF,7F7F7F,5F5F5F,4F4F4F,3F3F3F,2F2F2F,1F1F1F,0F0F0F,000000".split(","),
  },
];
