import { describe, expect, it } from "vitest";
import { paintThumbnail } from "./thumbnail";

/**
 * A thumbnail pixel has to be the average of what it covers.
 *
 * Every fourth column red, the rest transparent: a quarter of every block is
 * red, so every thumbnail pixel should be the same pale red over white. A
 * single `drawImage` at this ratio gave alpha 47, 127 and 0 in turn across
 * the row -- whole columns dropped or doubled depending on where they fell --
 * and a stroke moving across those sample points is what made the layer
 * thumbnails flicker with the wrong colours.
 */
function striped(width: number, height: number): HTMLCanvasElement {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext("2d")!;
  context.fillStyle = "rgb(255, 0, 0)";
  for (let x = 0; x < width; x += 4) context.fillRect(x, 0, 1, height);
  return canvas;
}

function thumbnailOf(sources: HTMLCanvasElement[], width: number, height: number) {
  const target = document.createElement("canvas");
  target.width = width;
  target.height = height;
  paintThumbnail(target, sources);
  return target.getContext("2d")!.getImageData(0, 0, width, height).data;
}

describe("paintThumbnail", () => {
  for (const [width, height, thumbWidth] of [
    [1024, 768, 24],
    [300, 300, 18],
  ] as const) {
    it(`averages what each pixel covers, ${width}x${height}`, () => {
      const data = thumbnailOf([striped(width, height)], thumbWidth, 18);
      // A quarter red over white: (255, 191, 191).
      for (let i = 0; i < data.length; i += 4) {
        expect(data[i]).toBe(255);
        expect(Math.abs(data[i + 1] - 191)).toBeLessThanOrEqual(12);
        expect(Math.abs(data[i + 2] - 191)).toBeLessThanOrEqual(12);
        expect(data[i + 3]).toBe(255);
      }
    });
  }

  it("stacks the foreground over the background", () => {
    const background = document.createElement("canvas");
    background.width = 64;
    background.height = 48;
    const back = background.getContext("2d")!;
    back.fillStyle = "rgb(0, 0, 255)";
    back.fillRect(0, 0, 64, 48);
    const foreground = document.createElement("canvas");
    foreground.width = 64;
    foreground.height = 48;
    const front = foreground.getContext("2d")!;
    front.fillStyle = "rgb(0, 255, 0)";
    front.fillRect(0, 0, 64, 48);
    const data = thumbnailOf([background, foreground], 24, 18);
    expect([...data.slice(0, 4)]).toEqual([0, 255, 0, 255]);
  });

  it("is paper, not the panel, where nobody has drawn", () => {
    expect([...thumbnailOf([], 24, 18).slice(0, 4)]).toEqual([255, 255, 255, 255]);
  });
});
