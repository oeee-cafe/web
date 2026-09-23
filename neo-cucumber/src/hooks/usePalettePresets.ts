import { useMemo } from "react";
import { useLingui } from "@lingui/react";
import {
  BUILT_IN_PALETTE_PRESETS,
  presetToPalette,
  type PalettePreset,
} from "../constants/palettePresets";

/** A preset ready to apply: its name in the active locale, colours in display order. */
export interface ResolvedPalettePreset {
  name: string;
  colors: string[];
}

/**
 * The host's presets if it gave any, else the painter's own, in the form the
 * palette stores. `mount` has already refused a malformed set, so anything
 * that fails to convert here is dropped rather than reported twice.
 */
export function usePalettePresets(
  hostPresets: readonly PalettePreset[] | undefined,
): ResolvedPalettePreset[] {
  const { i18n } = useLingui();
  return useMemo(() => {
    const source = hostPresets
      ? hostPresets.map((preset) => ({ name: preset.name, colors: preset.colors }))
      : BUILT_IN_PALETTE_PRESETS.map((preset) => ({
          name: i18n._(preset.name),
          colors: preset.colors,
        }));
    // A loop rather than `flatMap`, which is Firefox 62.
    const resolved: ResolvedPalettePreset[] = [];
    for (const { name, colors } of source) {
      const palette = presetToPalette(colors);
      if (palette) resolved.push({ name, colors: palette });
    }
    return resolved;
  }, [hostPresets, i18n]);
}
