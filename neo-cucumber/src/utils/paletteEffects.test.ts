import { describe, expect, it } from "vitest";
import { applyPaletteEffect, gradientPalette } from "./paletteEffects";

describe("palette effects", () => {
  it("brightens and darkens each channel by ten, clamped", () => {
    expect(applyPaletteEffect(["#00f5fa", "#123456"], "bright")).toEqual([
      "#0affff",
      "#1c3e60",
    ]);
    expect(applyPaletteEffect(["#00050a", "#123456"], "dark")).toEqual([
      "#000000",
      "#082a4c",
    ]);
  });

  it("inverts as 255 minus each channel", () => {
    expect(applyPaletteEffect(["#000000", "#ffffff", "#b47575"], "invert")).toEqual([
      "#ffffff",
      "#000000",
      "#4b8a8a",
    ]);
  });

  it("is not undone by the opposite step once a channel has clamped", () => {
    // POTI's does the same: brightness lost at an end is lost.
    const once = applyPaletteEffect(["#fafafa"], "bright");
    expect(applyPaletteEffect(once, "dark")).toEqual(["#f5f5f5"]);
  });
});

describe("gradation", () => {
  it("steps a fifteenth of the way at a time, and stops short of the end", () => {
    const ramp = gradientPalette("#000000", "#ffffff");
    expect(ramp).toHaveLength(14);
    expect(ramp[0]).toBe("#000000");
    expect(ramp[1]).toBe("#111111");
    expect(ramp[13]).toBe("#dddddd");
  });

  it("truncates each step towards zero", () => {
    // (0x10 - 0x00) / 15 is 1.07, so one per colour
    expect(gradientPalette("#101010", "#000000")[13]).toBe("#030303");
  });

  it("runs downwards as readily as upwards", () => {
    const ramp = gradientPalette("#f0a000", "#00a0f0");
    expect(ramp[0]).toBe("#f0a000");
    expect(ramp[13]).toBe("#20a0d0");
  });
});
