import { describe, expect, it } from "vitest";
import {
  BUILT_IN_PALETTE_PRESETS,
  fromNeoOrder,
  presetToPalette,
} from "./palettePresets";
import { DEFAULT_PALETTE_COLORS } from "./drawing";
import { NEO_PALETTE_ORDER } from "../neo/toolboxSpec";

describe("palette presets", () => {
  it("puts NEO's own palette on screen exactly as the painter opens", () => {
    // If this fails, either the pair swap is wrong or the two copies of NEO's
    // palette have drifted, and choosing "Default" would not restore it.
    expect(presetToPalette(NEO_PALETTE_ORDER)).toEqual(DEFAULT_PALETTE_COLORS);
  });

  it("swaps each pair, and only each pair", () => {
    expect(fromNeoOrder(["1", "2", "3", "4"])).toEqual(["2", "1", "4", "3"]);
  });

  it("takes palette.txt's spelling: no #, upper case", () => {
    const palette = presetToPalette(
      "FFFFFF,EFEFEF,DFDFDF,CFCFCF,BFBFBF,AFAFAF,7F7F7F,5F5F5F,4F4F4F,3F3F3F,2F2F2F,1F1F1F,0F0F0F,000000".split(","),
    );
    expect(palette?.slice(0, 2)).toEqual(["#efefef", "#ffffff"]);
  });

  it("refuses anything that is not fourteen colours", () => {
    expect(presetToPalette(NEO_PALETTE_ORDER.slice(0, 13))).toBeNull();
    expect(presetToPalette([...NEO_PALETTE_ORDER.slice(0, 13), "#fff"])).toBeNull();
    expect(presetToPalette([...NEO_PALETTE_ORDER.slice(0, 13), "red"])).toBeNull();
  });

  it("ships only well-formed sets", () => {
    for (const preset of BUILT_IN_PALETTE_PRESETS) {
      expect(presetToPalette(preset.colors), preset.name.message).not.toBeNull();
    }
  });
});
