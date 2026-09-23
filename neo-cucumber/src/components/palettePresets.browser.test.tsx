import { afterEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "vitest/browser";
import { act } from "react";
import { mount, type PainterHandle, type PainterOptions } from "../public";
import { DEFAULT_PALETTE_COLORS } from "../constants/drawing";
import "../App.css";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/*
 * Through the buttons rather than through setPaletteColors: what matters is
 * that pressing a preset recolours the swatches NEO's column shows, and that
 * it leaves the drawing colour alone the way Neo.setColors does.
 */

let painter: PainterHandle | null = null;
let element: HTMLElement | null = null;

afterEach(() => {
  if (painter) act(() => painter?.unmount());
  painter = null;
  element?.remove();
  element = null;
});

async function open(options: Partial<PainterOptions> = {}) {
  element = document.createElement("div");
  element.style.cssText = "position:fixed;inset:0";
  document.body.appendChild(element);
  act(() => {
    painter = mount(element!, {
      width: 200,
      height: 200,
      mode: { kind: "standard" },
      controls: { kind: "toolbox" },
      locale: "en",
      ...options,
    });
  });
  await act(async () => painter!.ready);
  await vi.waitFor(() => {
    if (!document.querySelector(".toolbox-extras")) throw new Error("no toolbox yet");
  });
}

const swatches = () =>
  Array.from(document.querySelectorAll<HTMLElement>(".toolbox-neo [data-color]")).map(
    (swatch) => swatch.dataset.color,
  );

const paletteButton = () =>
  document.querySelector<HTMLButtonElement>('.toolbox-extras button[aria-label="Palettes"]');

const presetOptions = () =>
  Array.from(document.querySelectorAll<HTMLElement>('.toolbox-palettes [role="option"]'));

/** The name the list marks, if the swatches still are a preset. */
const selectedPreset = () =>
  presetOptions().find((option) => option.getAttribute("aria-selected") === "true")
    ?.textContent ?? null;

const choose = (name: string) =>
  userEvent.click(presetOptions().find((option) => option.textContent === name)!);

const windowButton = (label: string) =>
  Array.from(document.querySelectorAll<HTMLButtonElement>(".toolbox-palettes button")).find(
    (button) => button.textContent === label,
  )!;

describe("palette presets", () => {
  it("stays closed until the palette button opens it, and closes again", async () => {
    await open();
    expect(document.querySelector(".toolbox-palettes")).toBeNull();
    expect(paletteButton()!.getAttribute("aria-pressed")).toBe("false");

    act(() => paletteButton()!.click());
    expect(document.querySelector(".toolbox-palettes")).not.toBeNull();

    act(() => paletteButton()!.click());
    expect(document.querySelector(".toolbox-palettes")).toBeNull();
  });

  it("recolours every swatch and keeps the drawing colour", async () => {
    await open();
    expect(swatches()).toEqual(DEFAULT_PALETTE_COLORS);
    const colorInput = document.querySelector<HTMLInputElement>(
      '.toolbox-extras input[type="color"]',
    )!;
    const before = colorInput.value;

    act(() => paletteButton()!.click());
    expect(paletteButton()!.getAttribute("aria-pressed")).toBe("true");
    expect(selectedPreset()).toBe("Default");

    await act(() => choose("grayscale"));
    // palette.txt's grayscale, white to black, read two to a row
    expect(swatches()).toEqual([
      "#efefef", "#ffffff", "#cfcfcf", "#dfdfdf", "#afafaf", "#bfbfbf",
      "#5f5f5f", "#7f7f7f", "#3f3f3f", "#4f4f4f", "#1f1f1f", "#2f2f2f",
      "#000000", "#0f0f0f",
    ]);
    expect(selectedPreset()).toBe("grayscale");
    expect(colorInput.value).toBe(before);

    await act(() => choose("Default"));
    expect(swatches()).toEqual(DEFAULT_PALETTE_COLORS);
  });

  it("lets the name go once a swatch is overwritten, so it can be picked again", async () => {
    await open();
    act(() => paletteButton()!.click());
    await act(() => choose("red"));
    const red = swatches();
    const swatch = document.querySelector<HTMLElement>(".toolbox-neo [data-color]")!;
    act(() => {
      swatch.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    });
    expect(selectedPreset()).toBeNull();
    await act(() => choose("red"));
    expect(swatches()).toEqual(red);
  });

  it("brightens, darkens and inverts every swatch, keeping the drawing colour", async () => {
    await open();
    const colorInput = document.querySelector<HTMLInputElement>(
      '.toolbox-extras input[type="color"]',
    )!;
    const before = colorInput.value;
    act(() => paletteButton()!.click());
    const effect = windowButton;

    act(() => effect("Invert").click());
    // White and black trade places; #888888 becomes #777777.
    expect(swatches().slice(0, 3)).toEqual(["#000000", "#ffffff", "#777777"]);
    expect(selectedPreset()).toBeNull();

    act(() => effect("Invert").click());
    expect(swatches()).toEqual(DEFAULT_PALETTE_COLORS);

    act(() => effect("Dark").click());
    expect(swatches().slice(0, 3)).toEqual(["#f5f5f5", "#000000", "#7e7e7e"]);
    act(() => effect("Bright").click());
    expect(swatches().slice(0, 3)).toEqual(["#ffffff", "#0a0a0a", "#888888"]);
    expect(colorInput.value).toBe(before);
  });

  it("grades between two swatches, numbered and ordered as NEO numbers them", async () => {
    await open();
    act(() => paletteButton()!.click());
    const [startNumber, endNumber] = Array.from(
      document.querySelectorAll<HTMLSelectElement>(".toolbox-palettes select[aria-label^='Gradation']"),
    );
    const [startText, endText] = Array.from(
      document.querySelectorAll<HTMLInputElement>(".toolbox-palettes input[type='text']"),
    );
    // POTI opens on swatches 1 and 12: NEO's black and #99CB7B.
    expect([startText.value, endText.value]).toEqual(["000000", "99CB7B"]);

    // Picking a number reloads both colours from the palette.
    await act(() => userEvent.selectOptions(endNumber, "2"));
    expect(endText.value).toBe("FFFFFF");
    await act(() => userEvent.selectOptions(startNumber, "1"));

    act(() => windowButton("Ok").click());
    // Black to white in NEO's order, which puts NEO's first colour on the
    // right of the top row.
    expect(swatches().slice(0, 4)).toEqual(["#111111", "#000000", "#333333", "#222222"]);
    expect(swatches()[12]).toBe("#dddddd");

    // A typed colour is what Ok grades from.
    await act(async () => {
      await userEvent.clear(startText);
      await userEvent.type(startText, "ff0000");
    });
    act(() => windowButton("Ok").click());
    expect(swatches()[1]).toBe("#ff0000");
  });

  it("offers the host's sets in place of its own", async () => {
    await open({
      palettePresets: [
        {
          name: "Mine",
          colors: "111111,222222,333333,444444,555555,666666,777777,888888,999999,AAAAAA,BBBBBB,CCCCCC,DDDDDD,EEEEEE".split(","),
        },
      ],
    });
    act(() => paletteButton()!.click());
    expect(presetOptions().map((option) => option.textContent)).toEqual(["Mine"]);
    await act(() => choose("Mine"));
    expect(swatches().slice(0, 4)).toEqual(["#222222", "#111111", "#444444", "#333333"]);
  });

  it("has no button when the host offers no sets", async () => {
    await open({ palettePresets: [] });
    expect(paletteButton()).toBeNull();
  });

  it("refuses a malformed set at mount", () => {
    const target = document.createElement("div");
    expect(() =>
      mount(target, {
        width: 100,
        height: 100,
        mode: { kind: "standard" },
        controls: { kind: "none" },
        palettePresets: [{ name: "Short", colors: ["#000000"] }],
      }),
    ).toThrow(expect.objectContaining({ code: "invalid-options" }));
  });
});
